//! Importing from a camera SD card.
//!
//! Flow the UI follows: [`plan`] (what's new on the card?) → [`import`] (copy into dated
//! folders, verify every copy, catalogue it) → optionally [`clear_verified`] (delete from
//! the card only what was verified).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use zd_catalog::{Catalog, VolumeKind};

use crate::{fingerprint, ingest, record, Error, Progress, Result, ScanReport, ThumbCache};

#[derive(Debug, Clone, Serialize)]
pub struct CardItem {
    pub src: PathBuf,
    pub size: u64,
    pub fingerprint: String,
    pub taken_at: Option<String>,
    /// Already safely on one of her drives (not just on another card).
    pub already_imported: bool,
}

/// Look at everything under DCIM and work out what's new.
pub fn plan(catalog: &Catalog, card_root: &Path) -> Result<Vec<CardItem>> {
    let dcim = card_root.join("DCIM");
    let search = if dcim.is_dir() { dcim } else { card_root.to_path_buf() };
    let mut items = Vec::new();
    for src in crate::find_media(&search) {
        let (fp, size) = fingerprint::fingerprint(&src).map_err(|e| Error::Io(src.clone(), e))?;
        let already_imported = match catalog.photo_by_fingerprint(&fp)? {
            Some(p) => {
                catalog.locations(p.id)?.iter().any(|l| l.volume_kind != VolumeKind::Card && !l.missing)
            }
            None => false,
        };
        let mime = crate::metadata::media_type(&src).unwrap_or("image/jpeg");
        let taken_at = crate::metadata::read_meta(&src, mime).taken_at;
        items.push(CardItem { src, size, fingerprint: fp, taken_at, already_imported });
    }
    items.sort_by(|a, b| a.taken_at.cmp(&b.taken_at).then_with(|| a.src.cmp(&b.src)));
    Ok(items)
}

#[derive(Debug, Default, Serialize)]
pub struct CardReport {
    /// (card file, verified copy) pairs. Only these may be cleared from the card.
    pub copied: Vec<(PathBuf, PathBuf)>,
    pub skipped_already_imported: usize,
    pub failed: Vec<(PathBuf, String)>,
    pub catalog: ScanReport,
}

/// `2026-05-04T07:12:00` -> `2026/2026-05-04`
pub fn dated_folder(taken_at: Option<&str>) -> PathBuf {
    match taken_at.and_then(|t| t.get(..10)).filter(|d| d.len() == 10 && d.as_bytes()[4] == b'-') {
        Some(day) => PathBuf::from(&day[..4]).join(day),
        None => PathBuf::from("Undated"),
    }
}

/// Copy new photos into `dest_root/YYYY/YYYY-MM-DD/`, verify each copy, catalogue it.
pub fn import(
    catalog: &Catalog,
    cache: &ThumbCache,
    items: &[CardItem],
    dest_root: &Path,
    progress: &mut dyn FnMut(&Progress),
) -> Result<CardReport> {
    fs::create_dir_all(dest_root).map_err(|e| Error::Io(dest_root.to_path_buf(), e))?;
    let volumes = catalog.volumes()?;
    let (volume, prefix) =
        zd_volumes::locate(dest_root, &volumes).ok_or_else(|| Error::NotOnVolume(dest_root.to_path_buf()))?;
    let volume = volume.clone();

    let mut report = CardReport::default();
    let total = items.len();
    for (i, item) in items.iter().enumerate() {
        progress(&Progress { done: i, total, current: item.src.clone() });
        if item.already_imported {
            report.skipped_already_imported += 1;
            continue;
        }
        let sub = dated_folder(item.taken_at.as_deref());
        match copy_verified(&item.src, &dest_root.join(&sub)) {
            Ok((dest, hash)) => {
                let rel_in_dest = zd_volumes::rel_string(dest.strip_prefix(dest_root).unwrap_or(&dest));
                let rel = if prefix.is_empty() { rel_in_dest } else { format!("{prefix}/{rel_in_dest}") };
                match ingest(dest.clone(), rel, cache) {
                    Ok(it) => {
                        let id = record(catalog, &volume, &it, &mut report.catalog)?;
                        catalog.set_full_hash(id, &hash)?;
                        report.copied.push((item.src.clone(), dest));
                    }
                    Err(f) => report.failed.push(f),
                }
            }
            Err(e) => report.failed.push((item.src.clone(), e)),
        }
    }
    progress(&Progress { done: total, total, current: dest_root.to_path_buf() });
    Ok(report)
}

/// Copy `src` into `dir`, fsync, and check the copy hashes identically.
/// Returns the final path and the file's full hash.
fn copy_verified(src: &Path, dir: &Path) -> std::result::Result<(PathBuf, String), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let src_hash = fingerprint::full_hash(src).map_err(|e| e.to_string())?;
    let name = src.file_name().ok_or("no file name")?.to_string_lossy().into_owned();
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.clone(), String::new()),
    };

    // Same name already there? If it's the same file, we're done; otherwise pick a new name.
    let mut dest = dir.join(&name);
    let mut n = 1;
    while dest.exists() {
        if fingerprint::full_hash(&dest).map_err(|e| e.to_string())? == src_hash {
            return Ok((dest, src_hash));
        }
        dest = dir.join(format!("{stem}-{n}{ext}"));
        n += 1;
    }

    let tmp = dir.join(format!(".{name}.zdpart"));
    let result = (|| -> std::io::Result<()> {
        let mut input = fs::File::open(src)?;
        let mut out = fs::File::create(&tmp)?;
        std::io::copy(&mut input, &mut out)?;
        out.flush()?;
        out.sync_all()?;
        if let Ok(t) = fs::metadata(src).and_then(|m| m.modified()) {
            let _ = out.set_modified(t); // keep the camera's timestamp
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    let copy_hash = fingerprint::full_hash(&tmp).map_err(|e| e.to_string())?;
    if copy_hash != src_hash {
        let _ = fs::remove_file(&tmp);
        return Err("copy did not match the card (bad card or cable?) — left on card".into());
    }
    fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    Ok((dest, src_hash))
}

/// Delete from the card only files whose copy was verified, and whose copy still exists.
/// Returns how many were deleted.
pub fn clear_verified(report: &CardReport) -> usize {
    report.copied.iter().filter(|(src, dest)| dest.is_file() && fs::remove_file(src).is_ok()).count()
}
