//! Getting photos into ZD-Photos.
//!
//! * [`scan`] catalogues a folder (or a whole drive) in place. Unchanged files are
//!   skipped by size + modification time; new or changed files are fingerprinted,
//!   their metadata read and thumbnails cached. Files that disappeared are flagged
//!   missing, never forgotten.
//! * [`card`] imports from a camera SD card: copy, verify byte for byte, catalogue,
//!   and only then offer to clear the card.
//!
//! "Photo" throughout includes videos (Canon R7 / Pixel MP4 and MOV): they're catalogued,
//! fingerprinted, dated and thumbnailed the same way.

pub mod card;
pub mod fingerprint;
pub mod metadata;
pub mod thumbs;
pub mod video;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use serde::Serialize;
use zd_catalog::{Catalog, PhotoMeta, Volume};

pub use thumbs::{Size, ThumbCache};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Catalog(#[from] zd_catalog::Error),
    #[error(transparent)]
    Volumes(#[from] zd_volumes::Error),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("{0} isn't on a drive ZD-Photos knows about yet; refresh drives first")]
    NotOnVolume(PathBuf),
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub phase: Phase,
    pub done: usize,
    pub total: usize,
    pub current: PathBuf,
}

/// What a scan is doing right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Phase {
    /// Listing files (total not known yet; `done` counts files found so far).
    Finding,
    /// Re-checking photos already in the catalog (cheap: size and date only).
    Checking,
    /// Reading, fingerprinting and making thumbnails for new or changed files.
    Adding,
}

#[derive(Debug, Default, Serialize)]
pub struct ScanReport {
    pub files_found: usize,
    pub unchanged: usize,
    pub new_photos: usize,
    /// Known photos found in a new place (moved, renamed, or a backup copy).
    pub new_locations: usize,
    pub thumbnails_made: usize,
    pub missing: usize,
    pub failed: Vec<(PathBuf, String)>,
    /// Stopped before the end; nothing was marked missing.
    pub cancelled: bool,
}

/// The expensive, parallel part of taking in one file.
struct Ingested {
    path: PathBuf,
    rel: String,
    size: u64,
    modified: i64,
    fingerprint: String,
    meta: PhotoMeta,
    thumb: std::result::Result<bool, String>,
}

fn mtime(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ingest(
    path: PathBuf,
    rel: String,
    cache: &ThumbCache,
) -> std::result::Result<Ingested, (PathBuf, String)> {
    let fail = |e: String| (path.clone(), e);
    let mime = metadata::media_type(&path).ok_or_else(|| fail("not a photo".into()))?;
    let md = std::fs::metadata(&path).map_err(|e| fail(e.to_string()))?;
    let (fingerprint, size) = fingerprint::fingerprint(&path).map_err(|e| fail(e.to_string()))?;
    let meta = metadata::read_meta(&path, mime);
    let thumb = if cache.has(&fingerprint) {
        Ok(false)
    } else {
        cache.generate(&path, &fingerprint, mime, meta.orientation).map(|_| true)
    };
    Ok(Ingested { modified: mtime(&md), path, rel, size, fingerprint, meta, thumb })
}

/// Write one ingested file to the catalog. Returns (new_photo, thumbnail_made).
fn record(
    catalog: &Catalog,
    volume: &Volume,
    it: &Ingested,
    report: &mut ScanReport,
) -> zd_catalog::Result<i64> {
    let (photo_id, created) = catalog.add_photo(&it.fingerprint, it.size, &it.meta)?;
    let previously_here = catalog.file_record(volume.id, &it.rel)?.map(|r| r.photo_id);
    catalog.put_file(photo_id, volume.id, &it.rel, it.size, it.modified)?;
    if created {
        report.new_photos += 1;
    } else if previously_here != Some(photo_id) {
        report.new_locations += 1;
    }
    match &it.thumb {
        Ok(made) => {
            catalog.set_has_thumbnail(photo_id, true)?;
            if *made {
                report.thumbnails_made += 1;
            }
        }
        Err(e) => report.failed.push((it.path.clone(), format!("thumbnail: {e}"))),
    }
    Ok(photo_id)
}

/// Folders we never descend into.
fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || name.eq_ignore_ascii_case("$RECYCLE.BIN")
        || name.eq_ignore_ascii_case("System Volume Information")
        || name == "@eaDir" // Synology thumbnails
        || name == "#recycle"
}

/// All photo files under `folder`.
pub fn find_media(folder: &Path) -> Vec<PathBuf> {
    find_media_with(folder, &mut |_, _| true)
}

/// [`find_media`], calling `on_file(count_so_far, path)` for each file found; stops early
/// (returning what it has) when `on_file` returns false.
pub fn find_media_with(folder: &Path, on_file: &mut dyn FnMut(usize, &Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let walk = walkdir::WalkDir::new(folder).follow_links(false).into_iter().filter_entry(|e| {
        e.depth() == 0 || !(e.file_type().is_dir() && skip_dir(&e.file_name().to_string_lossy()))
    });
    for e in walk.filter_map(|e| e.ok()) {
        if e.file_type().is_file() && metadata::media_type(e.path()).is_some() {
            out.push(e.into_path());
            if !on_file(out.len(), out.last().map(PathBuf::as_path).unwrap_or(folder)) {
                break;
            }
        }
    }
    out
}

/// Catalogue every photo under `folder`, which must be on a known, online volume.
pub fn scan(
    catalog: &Catalog,
    cache: &ThumbCache,
    folder: &Path,
    progress: &mut dyn FnMut(&Progress),
) -> Result<ScanReport> {
    scan_cancellable(catalog, cache, folder, progress, &AtomicBool::new(false))
}

/// [`scan`], stopping early (between batches of files) once `cancel` is set.
/// A stopped scan keeps everything it catalogued but marks nothing missing.
pub fn scan_cancellable(
    catalog: &Catalog,
    cache: &ThumbCache,
    folder: &Path,
    progress: &mut dyn FnMut(&Progress),
    cancel: &AtomicBool,
) -> Result<ScanReport> {
    let volumes = catalog.volumes()?;
    let folder = std::fs::canonicalize(folder).map_err(|e| Error::Io(folder.to_path_buf(), e))?;
    let (volume, prefix) =
        zd_volumes::locate(&folder, &volumes).ok_or_else(|| Error::NotOnVolume(folder.clone()))?;
    let volume = volume.clone();
    let started = zd_catalog::timestamp();

    let files = find_media_with(&folder, &mut |found, path| {
        if found % 200 == 0 {
            progress(&Progress { phase: Phase::Finding, done: found, total: 0, current: path.to_path_buf() });
        }
        !cancel.load(Ordering::Relaxed)
    });
    let mut report = ScanReport { files_found: files.len(), ..Default::default() };
    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        return Ok(report);
    }
    let total = files.len();
    let rel_of = |p: &Path| -> String {
        let r = zd_volumes::rel_string(p.strip_prefix(&folder).unwrap_or(p));
        if prefix.is_empty() {
            r
        } else {
            format!("{prefix}/{r}")
        }
    };

    // Cheap pass: skip what hasn't changed since last time. Committed every 500 files so the
    // UI sees progress and a Stop takes effect quickly.
    let mut todo = Vec::new();
    for (n, batch) in files.chunks(500).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
        }
        catalog.batch(|c| {
            for path in batch {
                let rel = rel_of(path);
                let unchanged = match (c.file_record(volume.id, &rel)?, std::fs::metadata(path)) {
                    (Some(rec), Ok(md)) => {
                        rec.size == md.len()
                            && rec.modified == mtime(&md)
                            && c.photo(rec.photo_id)?.is_some_and(|p| p.has_thumbnail)
                    }
                    _ => false,
                };
                if unchanged {
                    c.touch_file(volume.id, &rel)?;
                    report.unchanged += 1;
                } else {
                    todo.push((path.clone(), rel));
                }
            }
            Ok::<_, Error>(())
        })?;
        let done = (n * 500 + batch.len()).min(total);
        progress(&Progress {
            phase: Phase::Checking,
            done,
            total,
            current: batch.last().cloned().unwrap_or_default(),
        });
    }

    // Expensive pass, in parallel chunks; each chunk is committed as one transaction.
    let mut done = report.unchanged;
    for chunk in todo.chunks(32) {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            return Ok(report);
        }
        let results: Vec<_> =
            chunk.par_iter().map(|(p, rel)| ingest(p.clone(), rel.clone(), cache)).collect();
        let mut last = PathBuf::new();
        catalog.batch(|c| {
            for r in results {
                done += 1;
                match r {
                    Ok(it) => {
                        record(c, &volume, &it, &mut report)?;
                        last = it.path;
                    }
                    Err(f) => report.failed.push(f),
                }
            }
            Ok::<_, Error>(())
        })?;
        // Reported after the commit, so anything reacting to it sees the new photos.
        progress(&Progress { phase: Phase::Adding, done, total, current: last });
    }

    report.missing = catalog.mark_unseen_missing(volume.id, &prefix, &started)?;
    Ok(report)
}

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::Path;
    use zd_catalog::{Catalog, VolumeKind};
    use zd_volumes::MountInfo;

    pub fn jpeg(path: &Path, w: u32, h: u32, seed: u8) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x as u8) ^ seed, y as u8, seed]));
        img.save(path).unwrap();
    }

    pub fn drive(c: &Catalog, root: &Path, fs_id: &str, kind: VolumeKind) {
        let mut mounts: Vec<MountInfo> = c
            .volumes()
            .unwrap()
            .into_iter()
            .filter(|v| v.online)
            .map(|v| MountInfo {
                mount: v.last_mount.unwrap().into(),
                label: Some(v.label),
                fs_id: v.fs_id,
                network_path: v.network_path,
                kind: v.kind,
                device: None,
            })
            .collect();
        mounts.push(MountInfo {
            mount: root.to_path_buf(),
            label: Some(fs_id.into()),
            fs_id: Some(fs_id.into()),
            network_path: None,
            kind,
            device: None,
        });
        zd_volumes::refresh(c, &mounts).unwrap();
    }
}

#[cfg(test)]
mod tests;
