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
pub mod shared;
pub mod thumbs;
pub mod video;

use std::collections::HashSet;
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
    /// Thumbnails copied from the Home Library's shared cache instead of being made.
    pub thumbnails_from_share: usize,
    /// Files recognised from the shared index without being opened.
    pub index_hits: usize,
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
    /// Fingerprint and metadata came from the shared index; the file wasn't opened.
    from_index: bool,
    /// The thumbnail was copied from the shared cache.
    thumb_from_share: bool,
}

fn mtime(md: &std::fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ingest(
    f: &Found,
    rel: String,
    cache: &ThumbCache,
    shared: Option<&shared::SharedCache>,
) -> std::result::Result<Ingested, (PathBuf, String)> {
    let path = f.path.clone();
    let fail = |e: String| (path.clone(), e);
    let mime = metadata::media_type(&path).ok_or_else(|| fail("not a photo".into()))?;

    // Home Library: known to the shared index with the same size and date? Then we already
    // know its fingerprint and metadata, and can copy its thumbnail: no need to open it.
    if let Some(e) = shared.and_then(|s| s.lookup(&rel, f.size, f.modified)) {
        let local_had = cache.has_thumb(&e.fingerprint);
        let copied = !local_had && shared.is_some_and(|s| s.fetch(&e.fingerprint, cache, false));
        if local_had || copied {
            return Ok(Ingested {
                modified: f.modified,
                size: f.size,
                fingerprint: e.fingerprint.clone(),
                meta: e.meta.clone(),
                thumb: Ok(copied),
                from_index: true,
                thumb_from_share: copied,
                path,
                rel,
            });
        }
    }

    let mut it = read_and_ingest(f, rel, mime, cache).map_err(fail)?;
    if let (Some(s), Ok(_)) = (shared, &it.thumb) {
        s.publish(&it.fingerprint, cache);
    }
    it.from_index = false;
    Ok(it)
}

/// Read the file itself: fingerprint, metadata, and a thumbnail if the local cache lacks one.
fn read_and_ingest(
    f: &Found,
    rel: String,
    mime: &str,
    cache: &ThumbCache,
) -> std::result::Result<Ingested, String> {
    let path = f.path.clone();
    let done = |fingerprint: String, size: u64, meta: PhotoMeta, thumb| Ingested {
        modified: f.modified,
        size,
        fingerprint,
        meta,
        thumb,
        from_index: false,
        thumb_from_share: false,
        path: path.clone(),
        rel: rel.clone(),
    };

    // Plain images: open the file once. The first and last MiB give the fingerprint and the
    // EXIF; the rest is read (in one go) only if a thumbnail still has to be made. On a
    // network share this is several times fewer round trips than opening the file per step.
    if matches!(mime, "image/jpeg" | "image/png" | "image/webp" | "image/tiff") {
        let ht = fingerprint::read_head_tail(&path).map_err(|e| e.to_string())?;
        let (fingerprint, size) = (ht.fingerprint(), ht.size);
        let mut meta = metadata::read_meta_from(&path, mime, &ht.head);
        if meta.taken_at.is_none() {
            meta.taken_at = metadata::unix_to_local_string(f.modified);
        }
        let thumb = if cache.has_thumb(&fingerprint) {
            Ok(false)
        } else {
            match ht.into_full() {
                Ok(bytes) => {
                    cache.generate_from_bytes(&bytes, &fingerprint, mime, meta.orientation).map(|_| true)
                }
                Err(e) => Err(e.to_string()),
            }
        };
        return Ok(done(fingerprint, size, meta, thumb));
    }

    let (fingerprint, size) = fingerprint::fingerprint(&path).map_err(|e| e.to_string())?;
    let meta = metadata::read_meta(&path, mime);
    let thumb = if cache.has_thumb(&fingerprint) {
        Ok(false)
    } else {
        cache.generate(&path, &fingerprint, mime, meta.orientation).map(|_| true)
    };
    Ok(done(fingerprint, size, meta, thumb))
}

/// Write one ingested file to the catalog. Returns (new_photo, thumbnail_made).
fn record(
    catalog: &Catalog,
    volume: &Volume,
    it: &Ingested,
    report: &mut ScanReport,
) -> zd_catalog::Result<i64> {
    if it.from_index {
        report.index_hits += 1;
    }
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
            if it.thumb_from_share {
                report.thumbnails_from_share += 1;
            } else if *made {
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

/// A photo file found while listing a folder, with the size and date the listing already
/// reported (on Windows that costs no extra trip to a network share).
#[derive(Debug, Clone)]
pub struct Found {
    pub path: PathBuf,
    pub size: u64,
    pub modified: i64,
}

/// [`find_media`], calling `on_file(count_so_far, path)` for each file found; stops early
/// (returning what it has) when `on_file` returns false.
pub fn find_media_with(folder: &Path, on_file: &mut dyn FnMut(usize, &Path) -> bool) -> Vec<PathBuf> {
    find_media_files(folder, on_file).into_iter().map(|f| f.path).collect()
}

pub fn find_media_files(folder: &Path, on_file: &mut dyn FnMut(usize, &Path) -> bool) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    let walk = walkdir::WalkDir::new(folder).follow_links(false).into_iter().filter_entry(|e| {
        e.depth() == 0 || !(e.file_type().is_dir() && skip_dir(&e.file_name().to_string_lossy()))
    });
    for e in walk.filter_map(|e| e.ok()) {
        if e.file_type().is_file() && metadata::media_type(e.path()).is_some() {
            let Ok(md) = e.metadata() else { continue };
            out.push(Found { size: md.len(), modified: mtime(&md), path: e.into_path() });
            if !on_file(out.len(), out.last().map(|f| f.path.as_path()).unwrap_or(folder)) {
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

    // The Home Library carries a shared thumbnail cache and index (see `shared`).
    let mut shared = volume
        .is_home_library
        .then(|| volume.last_mount.as_deref().and_then(|m| shared::SharedCache::open(Path::new(m))))
        .flatten();
    // The computer serving the share reads its files fast, so it fills the shared cache with
    // everything it already knows. Other computers only add what they had to read anyway.
    let backfill =
        shared.as_ref().is_some_and(|s| s.writable()) && zd_volumes::home::served_here(catalog, &volume);

    let files = find_media_files(&folder, &mut |found, path| {
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
    let mut seen: HashSet<String> = HashSet::with_capacity(files.len());

    // Cheap pass: skip what hasn't changed since last time, using the size and date from the
    // listing (no extra round trip per file). Committed every 500 files so the UI sees
    // progress and a Stop takes effect quickly.
    let mut todo: Vec<(Found, String)> = Vec::new();
    for (n, batch) in files.chunks(500).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            save_shared(&mut shared);
            return Ok(report);
        }
        catalog.batch(|c| {
            for f in batch {
                let rel = rel_of(&f.path);
                seen.insert(rel.clone());
                let known = match c.file_record(volume.id, &rel)? {
                    Some(rec) if rec.size == f.size && rec.modified == f.modified => {
                        c.photo(rec.photo_id)?.filter(|p| p.has_thumbnail)
                    }
                    _ => None,
                };
                match known {
                    Some(photo) => {
                        c.touch_file(volume.id, &rel)?;
                        report.unchanged += 1;
                        if let Some(sh) = shared.as_mut() {
                            if backfill {
                                sh.publish(&photo.fingerprint, cache);
                            }
                            if backfill || sh.has_thumb(&photo.fingerprint) {
                                sh.record(shared::Entry {
                                    rel,
                                    size: f.size,
                                    modified: f.modified,
                                    fingerprint: photo.fingerprint,
                                    meta: photo.meta,
                                });
                            }
                        }
                    }
                    None => todo.push((f.clone(), rel)),
                }
            }
            Ok::<_, Error>(())
        })?;
        let done = (n * 500 + batch.len()).min(total);
        progress(&Progress {
            phase: Phase::Checking,
            done,
            total,
            current: batch.last().map(|f| f.path.clone()).unwrap_or_default(),
        });
    }

    // Expensive pass, in parallel chunks; each chunk is committed as one transaction.
    let mut done = report.unchanged;
    for chunk in todo.chunks(32) {
        if cancel.load(Ordering::Relaxed) {
            report.cancelled = true;
            save_shared(&mut shared);
            return Ok(report);
        }
        let sh = shared.as_ref();
        let results: Vec<_> = chunk.par_iter().map(|(f, rel)| ingest(f, rel.clone(), cache, sh)).collect();
        let mut last = PathBuf::new();
        catalog.batch(|c| {
            for r in results {
                done += 1;
                match r {
                    Ok(it) => {
                        record(c, &volume, &it, &mut report)?;
                        if let (Some(sh), true) = (shared.as_mut(), it.thumb.is_ok()) {
                            sh.record(shared::Entry {
                                rel: it.rel.clone(),
                                size: it.size,
                                modified: it.modified,
                                fingerprint: it.fingerprint.clone(),
                                meta: it.meta.clone(),
                            });
                        }
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

    if let Some(sh) = shared.as_mut() {
        sh.forget_unseen(&prefix, &seen);
    }
    save_shared(&mut shared);
    report.missing = catalog.mark_unseen_missing(volume.id, &prefix, &started)?;
    Ok(report)
}

/// Saving the shared index is best-effort: a read-only share just means no sharing.
fn save_shared(shared: &mut Option<shared::SharedCache>) {
    if let Some(sh) = shared.as_mut() {
        let _ = sh.save();
    }
}

/// Where the full-screen preview came from (see [`ensure_preview`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewSource {
    /// Already in the local cache.
    Cached,
    /// Copied from the Home Library's shared cache (fast, ~0.5 MB).
    Shared,
    /// Made from the original file just now.
    Original,
}

/// Make sure the 2560 px preview of a photo is in the local cache, for the full-screen
/// viewer. Cheapest source first: the local cache, then the Home Library's shared cache
/// (`share_root` is the Home Library's folder), then the original file (if its drive is
/// plugged in). Making it from the original also refreshes the grid thumbnail.
pub fn ensure_preview(
    cache: &ThumbCache,
    fingerprint: &str,
    mime: &str,
    orientation: u16,
    original: Option<&Path>,
    share_root: Option<&Path>,
) -> std::result::Result<(PathBuf, PreviewSource), String> {
    let dest = cache.path(fingerprint, Size::Preview);
    if dest.is_file() {
        return Ok((dest, PreviewSource::Cached));
    }
    if let Some(root) = share_root {
        let src = ThumbCache::new(root.join(shared::DIR_NAME)).path(fingerprint, Size::Preview);
        if src.is_file() && shared::copy_atomic(&src, &dest).is_ok() {
            return Ok((dest, PreviewSource::Shared));
        }
    }
    let original = original.ok_or_else(|| "its drive isn't plugged in".to_string())?;
    cache.generate(original, fingerprint, mime, orientation)?;
    Ok((dest, PreviewSource::Original))
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
