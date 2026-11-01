//! The ZD-Photos catalog.
//!
//! The catalog is the single source of truth. Photos are identified by a
//! content fingerprint, never by path, and every file location is recorded
//! relative to the root of a *volume* (drive, network share, SD card). That is
//! what lets a drive be unplugged, re-lettered, or mounted somewhere else
//! without losing tags or edits.

mod schema;

use std::path::Path;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

pub use rusqlite;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error(
        "catalog was created by a newer version of ZD-Photos (schema v{found}, this build knows v{known})"
    )]
    TooNew { found: u32, known: u32 },
    #[error("not found: {0}")]
    NotFound(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Current UTC time in the fixed-width format the catalog stores (sortable as text).
pub fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

fn now() -> String {
    timestamp()
}

// ---------------------------------------------------------------- volumes ---

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeKind {
    Internal,
    Removable,
    Network,
    Card,
}

impl VolumeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VolumeKind::Internal => "internal",
            VolumeKind::Removable => "removable",
            VolumeKind::Network => "network",
            VolumeKind::Card => "card",
        }
    }
    fn parse(s: &str) -> Self {
        match s {
            "removable" => VolumeKind::Removable,
            "network" => VolumeKind::Network,
            "card" => VolumeKind::Card,
            _ => VolumeKind::Internal,
        }
    }
}

/// What the volume detector knows about a mounted volume right now.
#[derive(Debug, Clone)]
pub struct VolumeSighting {
    pub uuid: String,
    pub label: String,
    pub kind: VolumeKind,
    pub fs_id: Option<String>,
    pub network_path: Option<String>,
    pub mount: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Volume {
    pub id: i64,
    pub uuid: String,
    pub label: String,
    pub kind: VolumeKind,
    pub fs_id: Option<String>,
    pub network_path: Option<String>,
    pub is_home_library: bool,
    pub last_mount: Option<String>,
    pub last_seen: Option<String>,
    pub online: bool,
}

impl Volume {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Volume {
            id: r.get("id")?,
            uuid: r.get("uuid")?,
            label: r.get("label")?,
            kind: VolumeKind::parse(&r.get::<_, String>("kind")?),
            fs_id: r.get("fs_id")?,
            network_path: r.get("network_path")?,
            is_home_library: r.get("is_home_library")?,
            last_mount: r.get("last_mount")?,
            last_seen: r.get("last_seen")?,
            online: r.get("online")?,
        })
    }
}

// ----------------------------------------------------------------- photos ---

/// Metadata read from a file (EXIF and image header).
#[derive(Debug, Clone, Default, Serialize)]
pub struct PhotoMeta {
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub orientation: u16,
    pub taken_at: Option<String>,
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub lens: Option<String>,
    pub gps_lat: Option<f64>,
    pub gps_lon: Option<f64>,
    /// Videos only.
    pub duration_ms: Option<u64>,
}

impl PhotoMeta {
    pub fn is_video(&self) -> bool {
        self.mime.starts_with("video/")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Photo {
    pub id: i64,
    pub fingerprint: String,
    pub full_hash: Option<String>,
    pub size: u64,
    pub meta: PhotoMeta,
    pub rating: u8,
    pub favorite: bool,
    pub has_thumbnail: bool,
    pub imported_at: String,
}

const PHOTO_COLS: &str = "p.id, p.fingerprint, p.full_hash, p.size, p.mime, p.width, p.height, \
     p.orientation, p.taken_at, p.camera_make, p.camera_model, p.lens, p.gps_lat, p.gps_lon, \
     p.rating, p.favorite, p.has_thumbnail, p.imported_at, p.duration_ms";

impl Photo {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Photo {
            id: r.get(0)?,
            fingerprint: r.get(1)?,
            full_hash: r.get(2)?,
            size: r.get::<_, i64>(3)? as u64,
            meta: PhotoMeta {
                mime: r.get(4)?,
                width: r.get(5)?,
                height: r.get(6)?,
                orientation: r.get(7)?,
                taken_at: r.get(8)?,
                camera_make: r.get(9)?,
                camera_model: r.get(10)?,
                lens: r.get(11)?,
                gps_lat: r.get(12)?,
                gps_lon: r.get(13)?,
                duration_ms: r.get::<_, Option<i64>>(18)?.map(|d| d as u64),
            },
            rating: r.get(14)?,
            favorite: r.get(15)?,
            has_thumbnail: r.get(16)?,
            imported_at: r.get(17)?,
        })
    }

    /// Landscape / portrait / square, after applying EXIF rotation.
    pub fn orientation_class(&self) -> Option<&'static str> {
        let (w, h) = (self.meta.width?, self.meta.height?);
        let (w, h) = if self.meta.orientation >= 5 { (h, w) } else { (w, h) };
        Some(match w.cmp(&h) {
            std::cmp::Ordering::Greater => "landscape",
            std::cmp::Ordering::Less => "portrait",
            std::cmp::Ordering::Equal => "square",
        })
    }
}

/// The last-known state of a file on a volume, used to skip unchanged files.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub photo_id: i64,
    pub size: u64,
    pub modified: i64,
}

/// Where a photo can be found, and whether it can be opened right now.
#[derive(Debug, Clone, Serialize)]
pub struct Location {
    pub volume_id: i64,
    pub volume_label: String,
    pub volume_kind: VolumeKind,
    pub is_home_library: bool,
    pub rel_path: String,
    pub mount: Option<String>,
    pub online: bool,
    pub missing: bool,
}

impl Location {
    /// Absolute path if the volume is online and the file was present at the last scan.
    pub fn current_path(&self) -> Option<std::path::PathBuf> {
        if !self.online || self.missing {
            return None;
        }
        let mut p = std::path::PathBuf::from(self.mount.as_ref()?);
        for part in self.rel_path.split('/') {
            p.push(part);
        }
        Some(p)
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Stats {
    pub photos: u64,
    pub files: u64,
    pub volumes: u64,
    pub online_volumes: u64,
    pub backed_up: u64,
    pub without_thumbnail: u64,
}

// ---------------------------------------------------------------- catalog ---

pub struct Catalog {
    conn: Connection,
}

impl Catalog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(dir) = path.as_ref().parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
        let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let known = schema::MIGRATIONS.len() as u32;
        if version > known {
            return Err(Error::TooNew { found: version, known });
        }
        for (i, sql) in schema::MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        Ok(Catalog { conn })
    }

    pub fn schema_version(&self) -> Result<u32> {
        Ok(self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// Run `f` atomically (a savepoint, so batches can nest). Much faster for bulk scans.
    pub fn batch<T, E>(
        &self,
        f: impl FnOnce(&Catalog) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E>
    where
        E: From<Error>,
    {
        self.conn.execute_batch("SAVEPOINT zd_batch").map_err(Error::from)?;
        match f(self) {
            Ok(out) => {
                self.conn.execute_batch("RELEASE zd_batch").map_err(Error::from)?;
                Ok(out)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK TO zd_batch; RELEASE zd_batch");
                Err(e)
            }
        }
    }

    // ---- volumes

    /// Record that a volume is mounted right now. Creates it on first sight;
    /// afterwards keeps her chosen label and updates where it is mounted.
    pub fn volume_seen(&self, s: &VolumeSighting) -> Result<Volume> {
        let ts = now();
        self.conn.execute(
            "INSERT INTO volumes (uuid, label, kind, fs_id, network_path, last_mount, last_seen, online)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
             ON CONFLICT(uuid) DO UPDATE SET
                kind = excluded.kind,
                fs_id = COALESCE(excluded.fs_id, volumes.fs_id),
                network_path = COALESCE(excluded.network_path, volumes.network_path),
                last_mount = excluded.last_mount,
                last_seen = excluded.last_seen,
                online = 1",
            params![s.uuid, s.label, s.kind.as_str(), s.fs_id, s.network_path, s.mount, ts],
        )?;
        self.volume_by_uuid(&s.uuid)?.ok_or_else(|| Error::NotFound(format!("volume {}", s.uuid)))
    }

    /// Mark every volume offline. Call before re-applying the current set of sightings.
    pub fn mark_all_volumes_offline(&self) -> Result<()> {
        self.conn.execute("UPDATE volumes SET online = 0", [])?;
        Ok(())
    }

    pub fn set_volume_online(&self, volume_id: i64, online: bool) -> Result<()> {
        self.conn.execute("UPDATE volumes SET online = ?2 WHERE id = ?1", params![volume_id, online])?;
        Ok(())
    }

    pub fn rename_volume(&self, volume_id: i64, label: &str) -> Result<()> {
        self.conn.execute("UPDATE volumes SET label = ?2 WHERE id = ?1", params![volume_id, label])?;
        Ok(())
    }

    pub fn volumes(&self) -> Result<Vec<Volume>> {
        let mut st = self.conn.prepare("SELECT * FROM volumes ORDER BY is_home_library DESC, label")?;
        let rows = st.query_map([], Volume::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn volume(&self, id: i64) -> Result<Option<Volume>> {
        Ok(self.conn.query_row("SELECT * FROM volumes WHERE id = ?1", [id], Volume::from_row).optional()?)
    }

    pub fn volume_by_uuid(&self, uuid: &str) -> Result<Option<Volume>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM volumes WHERE uuid = ?1", [uuid], Volume::from_row)
            .optional()?)
    }

    /// Exactly one volume can be the Home Library.
    pub fn set_home_library(&self, volume_id: i64) -> Result<()> {
        self.batch(|c| {
            c.conn.execute("UPDATE volumes SET is_home_library = 0", [])?;
            let n = c.conn.execute("UPDATE volumes SET is_home_library = 1 WHERE id = ?1", [volume_id])?;
            if n == 0 {
                return Err(Error::NotFound(format!("volume {volume_id}")));
            }
            Ok(())
        })
    }

    pub fn home_library(&self) -> Result<Option<Volume>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM volumes WHERE is_home_library = 1", [], Volume::from_row)
            .optional()?)
    }

    // ---- photos and files

    pub fn file_record(&self, volume_id: i64, rel_path: &str) -> Result<Option<FileRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT photo_id, size, modified FROM photo_files WHERE volume_id = ?1 AND rel_path = ?2",
                params![volume_id, rel_path],
                |r| {
                    Ok(FileRecord {
                        photo_id: r.get(0)?,
                        size: r.get::<_, i64>(1)? as u64,
                        modified: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Mark an unchanged file as seen in this scan without re-reading it.
    pub fn touch_file(&self, volume_id: i64, rel_path: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE photo_files SET last_seen = ?3, missing = 0 WHERE volume_id = ?1 AND rel_path = ?2",
            params![volume_id, rel_path, now()],
        )?;
        Ok(())
    }

    pub fn photo_by_fingerprint(&self, fingerprint: &str) -> Result<Option<Photo>> {
        Ok(self
            .conn
            .query_row(
                &format!("SELECT {PHOTO_COLS} FROM photos p WHERE p.fingerprint = ?1"),
                [fingerprint],
                Photo::from_row,
            )
            .optional()?)
    }

    pub fn photo(&self, id: i64) -> Result<Option<Photo>> {
        Ok(self
            .conn
            .query_row(&format!("SELECT {PHOTO_COLS} FROM photos p WHERE p.id = ?1"), [id], Photo::from_row)
            .optional()?)
    }

    /// Insert a photo, or return the existing one with the same fingerprint.
    /// Returns `(photo_id, newly_created)`.
    pub fn add_photo(&self, fingerprint: &str, size: u64, meta: &PhotoMeta) -> Result<(i64, bool)> {
        if let Some(p) = self.photo_by_fingerprint(fingerprint)? {
            return Ok((p.id, false));
        }
        self.conn.execute(
            "INSERT INTO photos (fingerprint, size, mime, width, height, orientation, taken_at,
                camera_make, camera_model, lens, gps_lat, gps_lon, imported_at, duration_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                fingerprint,
                size as i64,
                meta.mime,
                meta.width,
                meta.height,
                meta.orientation.max(1),
                meta.taken_at,
                meta.camera_make,
                meta.camera_model,
                meta.lens,
                meta.gps_lat,
                meta.gps_lon,
                now(),
                meta.duration_ms.map(|d| d as i64)
            ],
        )?;
        Ok((self.conn.last_insert_rowid(), true))
    }

    /// Record (or move) a file location for a photo.
    pub fn put_file(
        &self,
        photo_id: i64,
        volume_id: i64,
        rel_path: &str,
        size: u64,
        modified: i64,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO photo_files (photo_id, volume_id, rel_path, size, modified, last_seen, missing)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)
             ON CONFLICT(volume_id, rel_path) DO UPDATE SET
                photo_id = excluded.photo_id, size = excluded.size, modified = excluded.modified,
                last_seen = excluded.last_seen, missing = 0",
            params![photo_id, volume_id, rel_path, size as i64, modified, now()],
        )?;
        Ok(())
    }

    /// After scanning a volume (or one folder of it, `prefix` = `"2026/May"`, `""` = whole
    /// volume), flag every file under it not seen since `scan_started` as missing.
    /// Returns how many were flagged. Photos are never deleted: their tags and edits stay.
    pub fn mark_unseen_missing(&self, volume_id: i64, prefix: &str, scan_started: &str) -> Result<usize> {
        let prefix = prefix.trim_matches('/');
        let pattern = format!("{}/%", prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        Ok(self.conn.execute(
            "UPDATE photo_files SET missing = 1
             WHERE volume_id = ?1 AND last_seen < ?2 AND missing = 0
               AND (?3 = '' OR rel_path LIKE ?4 ESCAPE '\\')",
            params![volume_id, scan_started, prefix, pattern],
        )?)
    }

    pub fn set_full_hash(&self, photo_id: i64, hash: &str) -> Result<()> {
        self.conn.execute("UPDATE photos SET full_hash = ?2 WHERE id = ?1", params![photo_id, hash])?;
        Ok(())
    }

    pub fn set_has_thumbnail(&self, photo_id: i64, has: bool) -> Result<()> {
        self.conn.execute("UPDATE photos SET has_thumbnail = ?2 WHERE id = ?1", params![photo_id, has])?;
        Ok(())
    }

    pub fn set_rating(&self, photo_id: i64, rating: u8) -> Result<()> {
        self.conn.execute("UPDATE photos SET rating = ?2 WHERE id = ?1", params![photo_id, rating.min(5)])?;
        Ok(())
    }

    /// Photos newest first.
    pub fn photos(&self, limit: u32, offset: u32) -> Result<Vec<Photo>> {
        let mut st = self.conn.prepare(&format!(
            "SELECT {PHOTO_COLS} FROM photos p
             ORDER BY p.taken_at IS NULL, p.taken_at DESC, p.id DESC LIMIT ?1 OFFSET ?2"
        ))?;
        let rows = st.query_map(params![limit, offset], Photo::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn photos_without_thumbnail(&self, limit: u32) -> Result<Vec<Photo>> {
        let mut st = self
            .conn
            .prepare(&format!("SELECT {PHOTO_COLS} FROM photos p WHERE p.has_thumbnail = 0 LIMIT ?1"))?;
        let rows = st.query_map([limit], Photo::from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn locations(&self, photo_id: i64) -> Result<Vec<Location>> {
        let mut st = self.conn.prepare(
            "SELECT v.id, v.label, v.kind, v.is_home_library, f.rel_path, v.last_mount, v.online, f.missing
             FROM photo_files f JOIN volumes v ON v.id = f.volume_id
             WHERE f.photo_id = ?1
             ORDER BY v.online DESC, f.missing, v.is_home_library",
        )?;
        let rows = st.query_map([photo_id], |r| {
            Ok(Location {
                volume_id: r.get(0)?,
                volume_label: r.get(1)?,
                volume_kind: VolumeKind::parse(&r.get::<_, String>(2)?),
                is_home_library: r.get(3)?,
                rel_path: r.get(4)?,
                mount: r.get(5)?,
                online: r.get(6)?,
                missing: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// True if a verified copy exists on the Home Library.
    pub fn is_backed_up(&self, photo_id: i64) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM photo_files f JOIN volumes v ON v.id = f.volume_id
                           WHERE f.photo_id = ?1 AND v.is_home_library = 1 AND f.missing = 0)",
            [photo_id],
            |r| r.get(0),
        )?)
    }

    pub fn stats(&self) -> Result<Stats> {
        let q =
            |sql: &str| -> Result<u64> { Ok(self.conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as u64) };
        Ok(Stats {
            photos: q("SELECT COUNT(*) FROM photos")?,
            files: q("SELECT COUNT(*) FROM photo_files WHERE missing = 0")?,
            volumes: q("SELECT COUNT(*) FROM volumes")?,
            online_volumes: q("SELECT COUNT(*) FROM volumes WHERE online = 1")?,
            backed_up: q("SELECT COUNT(DISTINCT f.photo_id) FROM photo_files f
                          JOIN volumes v ON v.id = f.volume_id
                          WHERE v.is_home_library = 1 AND f.missing = 0")?,
            without_thumbnail: q("SELECT COUNT(*) FROM photos WHERE has_thumbnail = 0")?,
        })
    }

    // ---- tags

    /// Get or create a hierarchical tag from a path like `"Birds/Herons"`. Returns the leaf id.
    pub fn tag_path(&self, path: &str) -> Result<i64> {
        let mut parent: Option<i64> = None;
        let mut id = None;
        for name in path.split('/').map(str::trim).filter(|s| !s.is_empty()) {
            let existing: Option<i64> = self
                .conn
                .query_row(
                    "SELECT id FROM tags WHERE name = ?1 AND parent_id IS ?2",
                    params![name, parent],
                    |r| r.get(0),
                )
                .optional()?;
            let this = match existing {
                Some(t) => t,
                None => {
                    self.conn.execute(
                        "INSERT INTO tags (name, parent_id) VALUES (?1, ?2)",
                        params![name, parent],
                    )?;
                    self.conn.last_insert_rowid()
                }
            };
            parent = Some(this);
            id = Some(this);
        }
        id.ok_or_else(|| Error::NotFound("empty tag path".into()))
    }

    pub fn tag_photo(&self, photo_id: i64, tag_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO photo_tags (photo_id, tag_id) VALUES (?1, ?2)",
            params![photo_id, tag_id],
        )?;
        Ok(())
    }

    pub fn untag_photo(&self, photo_id: i64, tag_id: i64) -> Result<()> {
        self.conn.execute(
            "DELETE FROM photo_tags WHERE photo_id = ?1 AND tag_id = ?2",
            params![photo_id, tag_id],
        )?;
        Ok(())
    }

    /// Tags on a photo, as full paths ("Birds/Herons").
    pub fn photo_tags(&self, photo_id: i64) -> Result<Vec<String>> {
        let mut st = self.conn.prepare(
            "WITH RECURSIVE up(leaf, id, name, parent_id, depth) AS (
                 SELECT t.id, t.id, t.name, t.parent_id, 0 FROM tags t
                 JOIN photo_tags pt ON pt.tag_id = t.id WHERE pt.photo_id = ?1
                 UNION ALL
                 SELECT up.leaf, t.id, t.name, t.parent_id, up.depth + 1
                 FROM tags t JOIN up ON t.id = up.parent_id
             )
             SELECT leaf, group_concat(name, '/') FROM (SELECT * FROM up ORDER BY leaf, depth DESC)
             GROUP BY leaf ORDER BY 2",
        )?;
        let rows = st.query_map([photo_id], |r| r.get::<_, String>(1))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ---- settings

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
