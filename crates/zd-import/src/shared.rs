//! The shared cache on the Home Library.
//!
//! Pulling every full-size photo across Wi-Fi just to make a thumbnail is slow (~10 MB per
//! R7 JPEG). So the Home Library carries its own cache in a hidden `.zdphotos` folder at the
//! share's root:
//!
//! ```text
//! .zdphotos/
//!   thumbs/ab/<fingerprint>.jpg     512 px, same layout as the local cache
//!   previews/ab/<fingerprint>.jpg   2560 px
//!   index.jsonl                     one line per file: path, size, mtime, fingerprint, metadata
//! ```
//!
//! The computer that serves the share (nobara-zd) fills it while scanning its own disk, which
//! is fast. Any other computer scanning the share looks each file up in the index by path,
//! size and modification time: on a match it takes the fingerprint and metadata from the
//! index and copies the ~50 KB thumbnail, without opening the photo at all. Files it does
//! have to read are added to the cache and index for everyone else.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use zd_catalog::PhotoMeta;

use crate::thumbs::{Size, ThumbCache};

pub const DIR_NAME: &str = ".zdphotos";
const INDEX: &str = "index.jsonl";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Path relative to the share root, '/'-separated.
    #[serde(rename = "p")]
    pub rel: String,
    #[serde(rename = "s")]
    pub size: u64,
    /// Modification time, unix seconds.
    #[serde(rename = "m")]
    pub modified: i64,
    #[serde(rename = "f")]
    pub fingerprint: String,
    pub meta: PhotoMeta,
}

pub struct SharedCache {
    root: PathBuf,
    cache: ThumbCache,
    index: HashMap<String, Entry>,
    updates: HashMap<String, Entry>,
    removed: HashSet<String>,
    writable: bool,
}

impl SharedCache {
    /// Open (creating if possible) the shared cache at the root of a Home Library.
    /// Returns `None` only if there's no cache and none can be created.
    pub fn open(share_root: &Path) -> Option<SharedCache> {
        let root = share_root.join(DIR_NAME);
        let writable = fs::create_dir_all(&root).is_ok() && probe_writable(&root);
        if !root.is_dir() {
            return None;
        }
        let index = load_index(&root.join(INDEX));
        Some(SharedCache {
            cache: ThumbCache::new(&root),
            root,
            index,
            updates: HashMap::new(),
            removed: HashSet::new(),
            writable,
        })
    }

    pub fn writable(&self) -> bool {
        self.writable
    }

    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// The indexed entry for this file, if its size and modification time still match.
    pub fn lookup(&self, rel: &str, size: u64, modified: i64) -> Option<&Entry> {
        self.index.get(rel).filter(|e| e.size == size && e.modified == modified)
    }

    /// Copy the shared thumbnail (and preview, if `with_preview`) into the local cache.
    /// Returns true if the local cache now has a thumbnail.
    pub fn fetch(&self, fingerprint: &str, local: &ThumbCache, with_preview: bool) -> bool {
        let sizes: &[Size] = if with_preview { &[Size::Thumb, Size::Preview] } else { &[Size::Thumb] };
        for &size in sizes {
            let dest = local.path(fingerprint, size);
            if dest.is_file() {
                continue;
            }
            let src = self.cache.path(fingerprint, size);
            if !src.is_file() || copy_atomic(&src, &dest).is_err() {
                return size != Size::Thumb && local.has_thumb(fingerprint);
            }
        }
        local.has_thumb(fingerprint)
    }

    /// Copy locally made thumbnails up to the share (what's missing there). Cheap to call.
    pub fn publish(&self, fingerprint: &str, local: &ThumbCache) {
        if !self.writable {
            return;
        }
        for size in [Size::Thumb, Size::Preview] {
            let dest = self.cache.path(fingerprint, size);
            let src = local.path(fingerprint, size);
            if !dest.is_file() && src.is_file() {
                let _ = copy_atomic(&src, &dest);
            }
        }
    }

    pub fn has_thumb(&self, fingerprint: &str) -> bool {
        self.cache.has_thumb(fingerprint)
    }

    /// Record (or refresh) a file in the index.
    pub fn record(&mut self, entry: Entry) {
        let same = self.index.get(&entry.rel).is_some_and(|e| {
            e.size == entry.size && e.modified == entry.modified && e.fingerprint == entry.fingerprint
        });
        if !same {
            self.removed.remove(&entry.rel);
            self.index.insert(entry.rel.clone(), entry.clone());
            self.updates.insert(entry.rel.clone(), entry);
        }
    }

    /// After a complete scan of `prefix` (`""` = whole share), drop entries for files that
    /// weren't seen.
    pub fn forget_unseen(&mut self, prefix: &str, seen: &HashSet<String>) {
        let prefix = prefix.trim_matches('/');
        let gone: Vec<String> = self
            .index
            .keys()
            .filter(|rel| prefix.is_empty() || rel.starts_with(&format!("{prefix}/")))
            .filter(|rel| !seen.contains(*rel))
            .cloned()
            .collect();
        for rel in gone {
            self.index.remove(&rel);
            self.updates.remove(&rel);
            self.removed.insert(rel);
        }
    }

    /// Write the index if anything changed. Re-reads the file first and applies only our
    /// changes, so two computers scanning at once don't wipe out each other's work.
    pub fn save(&mut self) -> std::io::Result<()> {
        if !self.writable || (self.updates.is_empty() && self.removed.is_empty()) {
            return Ok(());
        }
        let path = self.root.join(INDEX);
        let mut merged = load_index(&path);
        for rel in &self.removed {
            merged.remove(rel);
        }
        for (rel, e) in &self.updates {
            merged.insert(rel.clone(), e.clone());
        }
        let tmp = self.root.join(format!("{INDEX}.tmp-{}", std::process::id()));
        {
            let mut w = std::io::BufWriter::new(fs::File::create(&tmp)?);
            let mut rels: Vec<&String> = merged.keys().collect();
            rels.sort();
            for rel in rels {
                serde_json::to_writer(&mut w, &merged[rel])?;
                w.write_all(b"\n")?;
            }
            w.flush()?;
        }
        fs::rename(&tmp, &path)?;
        self.index = merged;
        self.updates.clear();
        self.removed.clear();
        Ok(())
    }
}

fn load_index(path: &Path) -> HashMap<String, Entry> {
    let Ok(f) = fs::File::open(path) else { return HashMap::new() };
    BufReader::with_capacity(1 << 20, f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Entry>(&l).ok())
        .map(|e| (e.rel.clone(), e))
        .collect()
}

fn probe_writable(dir: &Path) -> bool {
    let p = dir.join(format!(".write-test-{}", std::process::id()));
    let ok = fs::write(&p, b"ok").is_ok();
    let _ = fs::remove_file(&p);
    ok
}

pub(crate) fn copy_atomic(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(dir) = dest.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = dest.with_extension(format!("part{}", std::process::id()));
    fs::copy(src, &tmp)?;
    fs::rename(&tmp, dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(rel: &str, fp: &str) -> Entry {
        Entry {
            rel: rel.into(),
            size: 10,
            modified: 1000,
            fingerprint: fp.into(),
            meta: PhotoMeta { mime: "image/jpeg".into(), orientation: 1, ..Default::default() },
        }
    }

    #[test]
    fn index_roundtrip_and_merge() {
        let d = tempfile::tempdir().unwrap();
        let mut a = SharedCache::open(d.path()).unwrap();
        assert!(a.writable() && a.is_empty());
        a.record(entry("2026/a.jpg", "fa"));
        a.record(entry("2026/b.jpg", "fb"));
        a.save().unwrap();

        // A second computer adds a file while the first is still open.
        let mut b = SharedCache::open(d.path()).unwrap();
        assert_eq!(b.len(), 2);
        b.record(entry("2026/c.jpg", "fc"));
        b.save().unwrap();

        // The first computer removes one; its save keeps the second computer's addition.
        let seen: HashSet<String> = ["2026/a.jpg".to_string()].into();
        a.forget_unseen("2026", &seen);
        a.save().unwrap();
        let c = SharedCache::open(d.path()).unwrap();
        let mut rels: Vec<&String> = c.index.keys().collect();
        rels.sort();
        assert_eq!(rels, vec!["2026/a.jpg", "2026/c.jpg"]);

        assert!(c.lookup("2026/a.jpg", 10, 1000).is_some());
        assert!(c.lookup("2026/a.jpg", 11, 1000).is_none(), "size changed");
        assert!(c.lookup("2026/a.jpg", 10, 1001).is_none(), "file edited");
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(DIR_NAME)).unwrap();
        let good = serde_json::to_string(&entry("x.jpg", "f")).unwrap();
        fs::write(d.path().join(DIR_NAME).join(INDEX), format!("garbage\n{good}\n{{\"p\":1}}\n")).unwrap();
        assert_eq!(SharedCache::open(d.path()).unwrap().len(), 1);
    }
}
