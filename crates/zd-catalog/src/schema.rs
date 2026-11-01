//! Schema migrations. Each entry upgrades the database by one version;
//! `PRAGMA user_version` records how many have been applied.

pub(crate) const MIGRATIONS: &[&str] = &[
    // v1 — Phase 0 foundations
    r#"
    CREATE TABLE volumes (
        id              INTEGER PRIMARY KEY,
        uuid            TEXT NOT NULL UNIQUE,     -- our marker-file UUID, or a derived id for read-only media
        label           TEXT NOT NULL,            -- friendly name shown to her ("Blue WD drive", "Home Library")
        kind            TEXT NOT NULL,            -- internal | removable | network | card
        fs_id           TEXT,                     -- filesystem UUID (Linux) / volume serial (Windows)
        network_path    TEXT,                     -- \\server\share or smb://server/share
        is_home_library INTEGER NOT NULL DEFAULT 0,
        last_mount      TEXT,                     -- where it was last seen mounted (informational only)
        last_seen       TEXT,
        online          INTEGER NOT NULL DEFAULT 0
    );

    CREATE TABLE photos (
        id            INTEGER PRIMARY KEY,
        fingerprint   TEXT NOT NULL UNIQUE,       -- BLAKE3(size + first MiB + last MiB)
        full_hash     TEXT,                       -- BLAKE3 of the whole file, filled lazily
        size          INTEGER NOT NULL,
        mime          TEXT NOT NULL,
        width         INTEGER,
        height        INTEGER,
        orientation   INTEGER NOT NULL DEFAULT 1, -- EXIF orientation 1..8
        taken_at      TEXT,                       -- ISO-8601 local capture time
        camera_make   TEXT,
        camera_model  TEXT,
        lens          TEXT,
        gps_lat       REAL,
        gps_lon       REAL,
        rating        INTEGER NOT NULL DEFAULT 0,
        favorite      INTEGER NOT NULL DEFAULT 0,
        has_thumbnail INTEGER NOT NULL DEFAULT 0,
        imported_at   TEXT NOT NULL
    );
    CREATE INDEX photos_taken_at ON photos(taken_at);
    CREATE INDEX photos_camera   ON photos(camera_model);

    -- One photo can live in several places (original drive + Home Library backup).
    CREATE TABLE photo_files (
        id          INTEGER PRIMARY KEY,
        photo_id    INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
        volume_id   INTEGER NOT NULL REFERENCES volumes(id) ON DELETE CASCADE,
        rel_path    TEXT NOT NULL,                -- '/'-separated, relative to the volume root
        size        INTEGER NOT NULL,
        modified    INTEGER NOT NULL,             -- unix seconds, used to skip unchanged files on rescan
        last_seen   TEXT NOT NULL,
        missing     INTEGER NOT NULL DEFAULT 0,   -- volume was online but the file wasn't there
        UNIQUE(volume_id, rel_path)
    );
    CREATE INDEX photo_files_photo ON photo_files(photo_id);

    CREATE TABLE tags (
        id        INTEGER PRIMARY KEY,
        name      TEXT NOT NULL,
        parent_id INTEGER REFERENCES tags(id) ON DELETE CASCADE,
        UNIQUE(name, parent_id)
    );

    CREATE TABLE photo_tags (
        photo_id INTEGER NOT NULL REFERENCES photos(id) ON DELETE CASCADE,
        tag_id   INTEGER NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
        PRIMARY KEY (photo_id, tag_id)
    );

    CREATE TABLE settings (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
    "#,
    // v2 — videos (Canon R7 MP4s): duration; `mime` tells photo from video
    r#"
    ALTER TABLE photos ADD COLUMN duration_ms INTEGER;
    "#,
];
