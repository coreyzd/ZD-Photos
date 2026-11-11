//! The `Library` object QML talks to.
//!
//! * Owns a catalog connection for the UI thread and hands the grid what it needs
//!   per photo (thumbnail, caption, video length, whether its drive is plugged in),
//!   loaded with a single query.
//! * Scans run on a worker thread with their own catalog connection (SQLite WAL lets
//!   the UI keep reading), report progress back through Qt's event loop, and can be
//!   stopped.
//! * Exposes the drives sidebar: Home Library first, then her drives with photo
//!   counts; picking one filters the grid; drives can be renamed.
//! * Feeds the full-screen viewer: previews are fetched (local cache → Home Library's
//!   shared cache → original file) on worker threads and announced with `previewReady`.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    #[auto_cxx_name]
    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        #[qproperty(QString, status)]
        #[qproperty(bool, scanning)]
        #[qproperty(i32, progress_done)]
        #[qproperty(i32, progress_total)]
        #[qproperty(QString, progress_file)]
        /// finding | checking | adding
        #[qproperty(QString, progress_phase)]
        #[qproperty(i32, volume_count)]
        #[qproperty(i32, selected_volume)]
        #[qproperty(QString, view_title)]
        /// Bumped on every reload so the grid rebuilds even if the count is unchanged.
        #[qproperty(i32, revision)]
        type Library = super::LibraryRust;

        /// Re-detect drives and reload everything.
        #[qinvokable]
        fn refresh(self: Pin<&mut Library>);

        /// Catalogue a folder chosen in the folder dialog (a `file://` URL), in the background.
        #[qinvokable]
        fn scan_folder(self: Pin<&mut Library>, url: &QString);

        /// Catalogue a whole drive (sidebar "Scan this drive"), in the background.
        #[qinvokable]
        fn scan_volume(self: Pin<&mut Library>, volume_id: i32);

        /// Stop the running scan after the current batch.
        #[qinvokable]
        fn cancel_scan(self: Pin<&mut Library>);

        // ---- grid (index = position in the current view)
        #[qinvokable]
        fn thumb_url(self: &Library, index: i32) -> QString;
        #[qinvokable]
        fn caption(self: &Library, index: i32) -> QString;
        /// "1:15" for videos, empty for photos.
        #[qinvokable]
        fn duration(self: &Library, index: i32) -> QString;
        /// No thumbnail could be made yet: the grid shows a placeholder with this file name.
        #[qinvokable]
        fn placeholder(self: &Library, index: i32) -> QString;
        #[qinvokable]
        fn is_offline(self: &Library, index: i32) -> bool;

        // ---- sidebar (index = row in the drives list)
        #[qinvokable]
        fn volume_id(self: &Library, index: i32) -> i32;
        #[qinvokable]
        fn volume_label(self: &Library, index: i32) -> QString;
        /// home | internal | removable | network | card | cloud
        #[qinvokable]
        fn volume_kind(self: &Library, index: i32) -> QString;
        #[qinvokable]
        fn volume_online(self: &Library, index: i32) -> bool;
        #[qinvokable]
        fn volume_photos(self: &Library, index: i32) -> i32;

        /// Show only photos on this drive; -1 = all photos.
        #[qinvokable]
        fn select_volume(self: Pin<&mut Library>, volume_id: i32);
        #[qinvokable]
        fn rename_volume(self: Pin<&mut Library>, volume_id: i32, name: &QString);
        /// "D:" on Windows for a plugged-in drive, else empty.
        #[qinvokable]
        fn volume_letter(self: &Library, index: i32) -> QString;

        // ---- viewer (index = position in the current view)
        #[qinvokable]
        fn photo_id(self: &Library, index: i32) -> i32;
        /// Where a photo is in the current view now (views shift while a scan adds photos).
        #[qinvokable]
        fn index_of(self: &Library, photo_id: i32) -> i32;
        #[qinvokable]
        fn is_video(self: &Library, index: i32) -> bool;
        /// The 2560 px preview if it's already cached; otherwise empty, and it is fetched in
        /// the background and announced with `previewReady`. Also used to prefetch neighbours.
        #[qinvokable]
        fn preview_url(self: Pin<&mut Library>, index: i32) -> QString;
        /// The original file, for zooming past the preview, when its drive is plugged in and
        /// Qt can show it directly (JPEG, PNG). Empty otherwise.
        #[qinvokable]
        fn original_url(self: &Library, index: i32) -> QString;
        /// The video file, when its drive is plugged in. Empty otherwise.
        #[qinvokable]
        fn video_url(self: &Library, index: i32) -> QString;
        /// Everything for the info panel, as JSON.
        #[qinvokable]
        fn info_json(self: &Library, index: i32) -> QString;

        /// A preview fetched by `previewUrl` is ready (`url`), or couldn't be made (`error`).
        #[qsignal]
        fn preview_ready(self: Pin<&mut Library>, photo_id: i32, url: &QString, error: &QString);
    }

    impl cxx_qt::Threading for Library {}
}

use std::collections::HashSet;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::QString;
use zd_catalog::{Catalog, Location, Photo, VolumeKind};
use zd_import::{Size, ThumbCache};

struct Item {
    id: i64,
    video: bool,
    thumb: String,
    /// Empty when there's a thumbnail; otherwise the file name to show instead.
    placeholder: String,
    caption: String,
    duration: String,
    offline: bool,
}

struct Drive {
    id: i64,
    label: String,
    kind: &'static str,
    online: bool,
    photos: u64,
    /// "D:" on Windows while plugged in.
    letter: String,
}

pub struct LibraryRust {
    count: i32,
    status: QString,
    scanning: bool,
    progress_done: i32,
    progress_total: i32,
    progress_file: QString,
    progress_phase: QString,
    volume_count: i32,
    selected_volume: i32,
    view_title: QString,
    revision: i32,

    data_dir: PathBuf,
    catalog: Option<Catalog>,
    cache: ThumbCache,
    items: Vec<Item>,
    drives: Vec<Drive>,
    cancel: Arc<AtomicBool>,
    /// Photos whose preview is being fetched right now.
    fetching: Arc<Mutex<HashSet<i64>>>,
}

/// Same data folder the `zdp` tool uses, so photos imported there show up here.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ZDP_DATA") {
        return d.into();
    }
    directories::ProjectDirs::from("com", "ZD", "ZD-Photos")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".zdphotos-data"))
}

impl Default for LibraryRust {
    fn default() -> Self {
        let data = data_dir();
        let (catalog, status) = match Catalog::open(data.join("catalog.sqlite")) {
            Ok(c) => (Some(c), String::new()),
            Err(e) => (None, format!("Couldn't open the catalog: {e}")),
        };
        LibraryRust {
            count: 0,
            status: QString::from(&status),
            scanning: false,
            progress_done: 0,
            progress_total: 0,
            progress_file: QString::default(),
            progress_phase: QString::default(),
            volume_count: 0,
            selected_volume: -1,
            view_title: QString::from("All photos"),
            revision: 0,
            cache: ThumbCache::new(data.join("cache")),
            data_dir: data,
            catalog,
            items: Vec::new(),
            drives: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            fetching: Arc::default(),
        }
    }
}

fn file_url(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}") // Windows: file:///C:/Users/...
    }
}

/// `2026-05-04T07:12:00` -> `May 4, 2026`
fn pretty_date(iso: &str) -> String {
    const MONTHS: [&str; 12] =
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (Some(y), Some(m), Some(d)) = (iso.get(0..4), iso.get(5..7), iso.get(8..10)) else {
        return String::new();
    };
    match (m.parse::<usize>(), d.parse::<u32>()) {
        (Ok(m @ 1..=12), Ok(d)) => format!("{} {d}, {y}", MONTHS[m - 1]),
        _ => String::new(),
    }
}

/// 75_000 ms -> "1:15"; 3_725_000 -> "1:02:05"
fn clock(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

fn kind_name(kind: VolumeKind, home: bool) -> &'static str {
    if home {
        return "home";
    }
    match kind {
        VolumeKind::Internal => "internal",
        VolumeKind::Removable => "removable",
        VolumeKind::Network => "network",
        VolumeKind::Card => "card",
        VolumeKind::Cloud => "cloud",
    }
}

/// Folder dialogs hand back URLs with %20 for spaces ("My Passport").
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn url_to_path(url: &str) -> PathBuf {
    let path = url.strip_prefix("file://").unwrap_or(url);
    // file:///C:/x -> C:/x on Windows
    let path = if cfg!(windows) { path.trim_start_matches('/') } else { path };
    PathBuf::from(percent_decode(path))
}

/// `D:\` -> `D:` (Windows drive roots only; Linux mounts and UNC shares get nothing).
fn drive_letter(mount: &str) -> String {
    let b = mount.as_bytes();
    let is_root = b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && mount[2..].trim_matches(['\\', '/']).is_empty();
    if is_root {
        mount[..2].to_ascii_uppercase()
    } else {
        String::new()
    }
}

/// `2026-05-04T07:12:00` -> `May 4, 2026 · 7:12 AM`
fn pretty_datetime(iso: &str) -> String {
    let date = pretty_date(iso);
    let (Some(h), Some(m)) = (iso.get(11..13).and_then(|h| h.parse::<u32>().ok()), iso.get(14..16)) else {
        return date;
    };
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{date} \u{b7} {h12}:{m} {ampm}")
}

fn type_name(mime: &str) -> &'static str {
    match mime {
        "image/jpeg" => "JPEG photo",
        "image/heic" => "HEIC photo",
        "image/png" => "PNG image",
        "image/webp" => "WebP image",
        "image/tiff" => "TIFF image",
        "image/x-canon-cr3" => "Canon RAW (CR3)",
        "image/x-canon-cr2" => "Canon RAW (CR2)",
        "image/x-adobe-dng" => "RAW (DNG)",
        "video/mp4" => "MP4 video",
        "video/quicktime" => "MOV video",
        _ => "File",
    }
}

fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// "Canon EOS R7", not "Canon Canon EOS R7".
fn camera_name(make: Option<&str>, model: Option<&str>) -> String {
    match (make.map(str::trim), model.map(str::trim)) {
        (Some(make), Some(model)) => {
            let first = make.split_whitespace().next().unwrap_or(make);
            if model.to_lowercase().starts_with(&first.to_lowercase()) {
                model.to_string()
            } else {
                format!("{make} {model}")
            }
        }
        (None, Some(m)) | (Some(m), None) => m.to_string(),
        (None, None) => String::new(),
    }
}

/// Info panel contents for one photo.
fn info_value(
    photo: &Photo,
    locations: &[Location],
    tags: Vec<String>,
    backed_up: bool,
) -> serde_json::Value {
    let m = &photo.meta;
    let mut rows: Vec<(&str, String)> = Vec::new();
    if let Some(t) = m.taken_at.as_deref() {
        rows.push(("Date", pretty_datetime(t)));
    }
    let camera = camera_name(m.camera_make.as_deref(), m.camera_model.as_deref());
    if !camera.is_empty() {
        rows.push(("Camera", camera));
    }
    if let Some(lens) = m.lens.as_deref().filter(|l| !l.trim().is_empty()) {
        rows.push(("Lens", lens.trim().to_string()));
    }
    let mut size = Vec::new();
    // As she sees it: a portrait shot is stored sideways with a "rotate" flag.
    let (width, height) = match (m.width, m.height) {
        (Some(w), Some(h)) if (5..=8).contains(&m.orientation) && m.mime != "image/heic" => {
            (Some(h), Some(w))
        }
        dims => dims,
    };
    if let (Some(w), Some(h)) = (width, height) {
        size.push(format!("{w} \u{d7} {h}"));
        if !m.is_video() {
            size.push(format!("{:.1} MP", f64::from(w) * f64::from(h) / 1e6));
        }
    }
    size.push(megabytes(photo.size));
    rows.push(("Size", size.join(" \u{b7} ")));
    if let Some(ms) = m.duration_ms.filter(|_| m.is_video()) {
        rows.push(("Length", clock(ms)));
    }
    rows.push(("Type", type_name(&m.mime).to_string()));

    let (place, map) = match (m.gps_lat, m.gps_lon) {
        (Some(lat), Some(lon)) => (
            format!("{lat:.5}, {lon:.5}"),
            format!("https://www.openstreetmap.org/?mlat={lat:.6}&mlon={lon:.6}#map=15/{lat:.6}/{lon:.6}"),
        ),
        _ => (String::new(), String::new()),
    };

    let files: Vec<serde_json::Value> = locations
        .iter()
        .map(|l| {
            let state = if l.missing {
                "missing"
            } else if l.online {
                "online"
            } else {
                "offline"
            };
            serde_json::json!({
                "drive": l.volume_label,
                "home": l.is_home_library,
                "path": l.rel_path,
                "state": state,
            })
        })
        .collect();

    let name = locations
        .first()
        .map(|l| l.rel_path.rsplit('/').next().unwrap_or(&l.rel_path).to_string())
        .unwrap_or_default();
    serde_json::json!({
        "name": name,
        "title": m.taken_at.as_deref().map(pretty_datetime).unwrap_or_else(|| name.clone()),
        "rows": rows.iter().map(|(k, v)| serde_json::json!({"label": k, "value": v})).collect::<Vec<_>>(),
        "place": place,
        "map": map,
        "files": files,
        "tags": tags,
        "backedUp": backed_up,
        "width": width,
        "height": height,
    })
}

/// The best copy to open: plugged-in local drives before network shares.
fn best_original(locations: &[Location]) -> Option<PathBuf> {
    let local =
        locations.iter().filter(|l| l.volume_kind != VolumeKind::Network).find_map(|l| l.current_path());
    local.or_else(|| locations.iter().find_map(|l| l.current_path()))
}

impl qobject::Library {
    /// Reload drives and the current view from the catalog (UI thread, fast).
    fn reload(mut self: Pin<&mut Self>) {
        let selected = *self.selected_volume();
        let rust = self.as_mut().rust_mut();
        let rust = rust.get_mut();
        let Some(catalog) = rust.catalog.as_ref() else { return };

        // Drives: Home Library first, then by name. Internal disks only if they hold photos.
        let counts = catalog.volume_photo_counts().unwrap_or_default();
        let count_of = |id: i64| counts.iter().find(|(v, _)| *v == id).map_or(0, |(_, n)| *n);
        rust.drives = catalog
            .volumes()
            .unwrap_or_default()
            .into_iter()
            .map(|v| Drive {
                letter: if v.online {
                    drive_letter(v.last_mount.as_deref().unwrap_or(""))
                } else {
                    String::new()
                },
                id: v.id,
                kind: kind_name(v.kind, v.is_home_library),
                online: v.online,
                photos: count_of(v.id),
                label: v.label,
            })
            .filter(|d| d.kind != "internal" || d.photos > 0)
            .collect();

        let volume = (selected >= 0).then_some(i64::from(selected));
        rust.items = catalog
            .summaries(volume)
            .unwrap_or_default()
            .into_iter()
            .map(|p| {
                let date = p.taken_at.as_deref().map(pretty_date).unwrap_or_default();
                let caption: Vec<String> = [date, p.camera_model.unwrap_or_default()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect();
                Item {
                    id: p.id,
                    video: p.is_video,
                    thumb: if p.has_thumbnail {
                        file_url(&rust.cache.path(&p.fingerprint, Size::Thumb))
                    } else {
                        String::new()
                    },
                    placeholder: if p.has_thumbnail {
                        String::new()
                    } else {
                        p.file_name.clone().unwrap_or_else(|| "No preview".into())
                    },
                    caption: caption.join(" · "),
                    duration: if p.is_video { clock(p.duration_ms.unwrap_or(0)) } else { String::new() },
                    offline: !p.available,
                }
            })
            .collect();

        let n = rust.items.len() as i32;
        let offline = rust.items.iter().filter(|i| i.offline).count();
        let title = match volume.and_then(|id| rust.drives.iter().find(|d| d.id == id)) {
            Some(d) => d.label.clone(),
            None => "All photos".into(),
        };
        let drives = rust.drives.len() as i32;
        let scanning = rust.scanning;

        self.as_mut().set_view_title(QString::from(&title));
        self.as_mut().set_volume_count(drives);
        self.as_mut().set_count(n);
        let rev = *self.revision() + 1;
        self.as_mut().set_revision(rev);
        if !scanning {
            let status = match (n, offline) {
                (0, _) => {
                    "No photos here yet. Choose Add folder, or right-click a drive and pick Scan this drive."
                        .to_string()
                }
                (n, 0) => format!("{n} photos"),
                (n, off) => format!("{n} photos · {off} on drives that aren't plugged in"),
            };
            self.as_mut().set_status(QString::from(&status));
        }
    }

    pub fn refresh(mut self: Pin<&mut Self>) {
        if let Some(catalog) = self.as_ref().rust().catalog.as_ref() {
            let mounts = zd_volumes::list_mounts();
            let _ = zd_volumes::refresh(catalog, &mounts);
            let _ = zd_volumes::home::connect(catalog, &mounts);
        }
        self.as_mut().reload();
    }

    pub fn scan_folder(self: Pin<&mut Self>, url: &QString) {
        let path = url_to_path(&url.to_string());
        self.start_scan(path);
    }

    pub fn scan_volume(self: Pin<&mut Self>, volume_id: i32) {
        let mount = self
            .rust()
            .catalog
            .as_ref()
            .and_then(|c| c.volume(i64::from(volume_id)).ok().flatten())
            .filter(|v| v.online)
            .and_then(|v| v.last_mount);
        if let Some(m) = mount {
            self.start_scan(PathBuf::from(m));
        }
    }

    pub fn cancel_scan(mut self: Pin<&mut Self>) {
        self.rust().cancel.store(true, Ordering::Relaxed);
        self.as_mut().set_status(QString::from("Stopping after the current batch…"));
    }

    /// Run a scan on a worker thread. Progress and the result come back via the Qt event loop.
    fn start_scan(mut self: Pin<&mut Self>, path: PathBuf) {
        if *self.scanning() {
            return;
        }
        let cancel = self.rust().cancel.clone();
        cancel.store(false, Ordering::Relaxed);
        let data_dir = self.rust().data_dir.clone();
        let cache = self.rust().cache.clone();
        let qt = self.qt_thread();

        self.as_mut().set_scanning(true);
        self.as_mut().set_progress_phase(QString::from("finding"));
        self.as_mut().set_progress_done(0);
        self.as_mut().set_progress_total(0);
        self.as_mut().set_status(QString::from(&format!("Looking for photos in {}…", path.display())));

        std::thread::spawn(move || {
            let result = (|| -> Result<zd_import::ScanReport, String> {
                // The worker's own connection; WAL lets the UI read while we write.
                let catalog = Catalog::open(data_dir.join("catalog.sqlite")).map_err(|e| e.to_string())?;
                let mounts = zd_volumes::list_mounts();
                zd_volumes::refresh(&catalog, &mounts).map_err(|e| e.to_string())?;
                let _ = zd_volumes::home::connect(&catalog, &mounts);

                let mut last = Instant::now() - Duration::from_secs(1);
                let mut last_reload = Instant::now();
                let mut on_progress = |p: &zd_import::Progress| {
                    // Show newly added photos every few seconds, not only at the end.
                    if last_reload.elapsed() >= Duration::from_secs(3) {
                        last_reload = Instant::now();
                        let _ = qt.queue(|lib| lib.reload());
                    }
                    // ~8 updates a second is plenty and keeps the event loop free.
                    if last.elapsed() < Duration::from_millis(120) && p.done < p.total {
                        return;
                    }
                    last = Instant::now();
                    let (done, total) = (p.done as i32, p.total as i32);
                    let name =
                        p.current.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let phase = match p.phase {
                        zd_import::Phase::Finding => "finding",
                        zd_import::Phase::Checking => "checking",
                        zd_import::Phase::Adding => "adding",
                    };
                    let _ = qt.queue(move |mut lib| {
                        lib.as_mut().set_progress_phase(QString::from(phase));
                        lib.as_mut().set_progress_done(done);
                        lib.as_mut().set_progress_total(total);
                        lib.as_mut().set_progress_file(QString::from(&name));
                    });
                };
                zd_import::scan_cancellable(&catalog, &cache, &path, &mut on_progress, &cancel)
                    .map_err(|e| e.to_string())
            })();

            let _ = qt.queue(move |mut lib| {
                lib.as_mut().set_scanning(false);
                lib.as_mut().reload();
                let msg = match result {
                    Ok(r) if r.cancelled => {
                        format!("Stopped. {} new photos were added before stopping.", r.new_photos)
                    }
                    Ok(r) => {
                        let mut m = format!(
                            "Done: {} new photos, {} already known",
                            r.new_photos,
                            r.unchanged + r.new_locations
                        );
                        if r.thumbnails_from_share > 0 {
                            m += &format!(
                                " ({} previews taken from the Home Library)",
                                r.thumbnails_from_share
                            );
                        }
                        if r.missing > 0 {
                            m += &format!(", {} no longer there", r.missing);
                        }
                        if !r.failed.is_empty() {
                            m += &format!(", {} couldn't be read", r.failed.len());
                        }
                        m
                    }
                    Err(e) => format!("Couldn't scan: {e}"),
                };
                lib.as_mut().set_status(QString::from(&msg));
            });
        });
    }

    // ---- grid

    pub fn thumb_url(&self, index: i32) -> QString {
        self.item(index).map(|i| QString::from(&i.thumb)).unwrap_or_default()
    }

    pub fn caption(&self, index: i32) -> QString {
        self.item(index).map(|i| QString::from(&i.caption)).unwrap_or_default()
    }

    pub fn duration(&self, index: i32) -> QString {
        self.item(index).map(|i| QString::from(&i.duration)).unwrap_or_default()
    }

    pub fn placeholder(&self, index: i32) -> QString {
        self.item(index).map(|i| QString::from(&i.placeholder)).unwrap_or_default()
    }

    pub fn is_offline(&self, index: i32) -> bool {
        self.item(index).is_some_and(|i| i.offline)
    }

    fn item(&self, index: i32) -> Option<&Item> {
        usize::try_from(index).ok().and_then(|i| self.rust().items.get(i))
    }

    // ---- sidebar

    fn drive(&self, index: i32) -> Option<&Drive> {
        usize::try_from(index).ok().and_then(|i| self.rust().drives.get(i))
    }

    pub fn volume_id(&self, index: i32) -> i32 {
        self.drive(index).map_or(-1, |d| d.id as i32)
    }

    pub fn volume_label(&self, index: i32) -> QString {
        self.drive(index).map(|d| QString::from(&d.label)).unwrap_or_default()
    }

    pub fn volume_kind(&self, index: i32) -> QString {
        self.drive(index).map(|d| QString::from(d.kind)).unwrap_or_default()
    }

    pub fn volume_online(&self, index: i32) -> bool {
        self.drive(index).is_some_and(|d| d.online)
    }

    pub fn volume_photos(&self, index: i32) -> i32 {
        self.drive(index).map_or(0, |d| d.photos as i32)
    }

    pub fn select_volume(mut self: Pin<&mut Self>, volume_id: i32) {
        self.as_mut().set_selected_volume(volume_id);
        self.as_mut().reload();
    }

    pub fn rename_volume(mut self: Pin<&mut Self>, volume_id: i32, name: &QString) {
        let name = name.to_string();
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(c) = self.rust().catalog.as_ref() {
            let _ = c.rename_volume(i64::from(volume_id), name);
        }
        self.as_mut().reload();
    }
    pub fn volume_letter(&self, index: i32) -> QString {
        self.drive(index).map(|d| QString::from(&d.letter)).unwrap_or_default()
    }

    // ---- viewer

    pub fn photo_id(&self, index: i32) -> i32 {
        self.item(index).map_or(-1, |i| i.id as i32)
    }

    pub fn index_of(&self, photo_id: i32) -> i32 {
        let id = i64::from(photo_id);
        self.rust().items.iter().position(|i| i.id == id).map_or(-1, |p| p as i32)
    }

    pub fn is_video(&self, index: i32) -> bool {
        self.item(index).is_some_and(|i| i.video)
    }

    fn photo_and_locations(&self, index: i32) -> Option<(Photo, Vec<Location>)> {
        let id = self.item(index)?.id;
        let catalog = self.rust().catalog.as_ref()?;
        let photo = catalog.photo(id).ok().flatten()?;
        let locations = catalog.locations(id).unwrap_or_default();
        Some((photo, locations))
    }

    pub fn preview_url(self: Pin<&mut Self>, index: i32) -> QString {
        let Some((photo, locations)) = self.photo_and_locations(index) else {
            return QString::default();
        };
        let rust = self.rust();
        let cached = rust.cache.path(&photo.fingerprint, Size::Preview);
        if cached.is_file() {
            return QString::from(&file_url(&cached));
        }
        if !rust.fetching.lock().map(|mut f| f.insert(photo.id)).unwrap_or(false) {
            return QString::default(); // already on its way
        }

        let cache = rust.cache.clone();
        let fetching = rust.fetching.clone();
        let data_dir = rust.data_dir.clone();
        let share_root = rust
            .catalog
            .as_ref()
            .and_then(|c| c.home_library().ok().flatten())
            .filter(|v| v.online)
            .and_then(|v| v.last_mount)
            .map(PathBuf::from);
        let original = best_original(&locations);
        let qt = self.qt_thread();

        std::thread::spawn(move || {
            let result = zd_import::ensure_preview(
                &cache,
                &photo.fingerprint,
                &photo.meta.mime,
                photo.meta.orientation,
                original.as_deref(),
                share_root.as_deref(),
            );
            // A photo that had no thumbnail has one now.
            let fixed_thumb =
                matches!(result, Ok((_, zd_import::PreviewSource::Original))) && !photo.has_thumbnail;
            if fixed_thumb {
                if let Ok(c) = Catalog::open(data_dir.join("catalog.sqlite")) {
                    let _ = c.set_has_thumbnail(photo.id, true);
                }
            }
            if let Ok(mut f) = fetching.lock() {
                f.remove(&photo.id);
            }
            let (url, error) = match result {
                Ok((path, _)) => (file_url(&path), String::new()),
                Err(e) => (String::new(), e),
            };
            let id = photo.id as i32;
            let _ = qt.queue(move |mut lib| {
                lib.as_mut().preview_ready(id, &QString::from(&url), &QString::from(&error));
            });
        });
        QString::default()
    }

    pub fn original_url(&self, index: i32) -> QString {
        let Some((photo, locations)) = self.photo_and_locations(index) else {
            return QString::default();
        };
        if !matches!(photo.meta.mime.as_str(), "image/jpeg" | "image/png") {
            return QString::default();
        }
        best_original(&locations).map(|p| QString::from(&file_url(&p))).unwrap_or_default()
    }

    pub fn video_url(&self, index: i32) -> QString {
        let Some((_, locations)) = self.photo_and_locations(index).filter(|(p, _)| p.meta.is_video()) else {
            return QString::default();
        };
        best_original(&locations).map(|p| QString::from(&file_url(&p))).unwrap_or_default()
    }

    pub fn info_json(&self, index: i32) -> QString {
        let Some((photo, locations)) = self.photo_and_locations(index) else {
            return QString::from("{}");
        };
        let catalog = self.rust().catalog.as_ref();
        let tags = catalog.and_then(|c| c.photo_tags(photo.id).ok()).unwrap_or_default();
        let backed_up = catalog.and_then(|c| c.is_backed_up(photo.id).ok()).unwrap_or(false);
        QString::from(&info_value(&photo, &locations, tags, backed_up).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(pretty_date("2026-05-04T07:12:00"), "May 4, 2026");
        assert_eq!(pretty_date("junk"), "");
        assert_eq!(clock(75_000), "1:15");
        assert_eq!(clock(3_725_000), "1:02:05");
        assert_eq!(percent_decode("/run/media/c/My%20Passport"), "/run/media/c/My Passport");
        assert_eq!(file_url(std::path::Path::new("/a/b.jpg")), "file:///a/b.jpg");
        assert_eq!(kind_name(VolumeKind::Cloud, false), "cloud");
        assert_eq!(kind_name(VolumeKind::Network, true), "home");
        assert_eq!(drive_letter("D:\\"), "D:");
        assert_eq!(drive_letter("e:"), "E:");
        assert_eq!(drive_letter("D:\\Photos"), "");
        assert_eq!(drive_letter("\\\\nobara-zd.local\\Photos"), "");
        assert_eq!(drive_letter("/run/media/c/Passport"), "");
        assert_eq!(pretty_datetime("2026-05-04T07:12:00"), "May 4, 2026 \u{b7} 7:12 AM");
        assert_eq!(pretty_datetime("2026-05-04T00:05:00"), "May 4, 2026 \u{b7} 12:05 AM");
        assert_eq!(pretty_datetime("2026-05-04T19:30:00"), "May 4, 2026 \u{b7} 7:30 PM");
        assert_eq!(camera_name(Some("Canon"), Some("Canon EOS R7")), "Canon EOS R7");
        assert_eq!(camera_name(Some("Google"), Some("Pixel 9 Pro")), "Google Pixel 9 Pro");
        assert_eq!(megabytes(10_485_760), "10 MB");
    }
}
