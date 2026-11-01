//! The `Library` object QML talks to. It owns the catalog and hands the grid
//! what it needs per photo: thumbnail URL, caption, and whether the photo's
//! drive is plugged in.
//!
//! Phase 0 keeps this deliberately simple (index-based accessors, scans on the
//! UI thread). Phase 1 replaces it with a proper QAbstractListModel and
//! background scanning.

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
    }

    unsafe extern "RustQt" {
        #[qobject]
        #[qml_element]
        #[qproperty(i32, count)]
        #[qproperty(QString, status)]
        #[qproperty(QString, drives)]
        type Library = super::LibraryRust;

        /// Re-detect drives and reload the photo list.
        #[qinvokable]
        fn refresh(self: Pin<&mut Library>);

        /// Catalogue a folder chosen in the folder dialog (a `file://` URL).
        #[qinvokable]
        #[cxx_name = "scanFolder"]
        fn scan_folder(self: Pin<&mut Library>, url: &QString);

        #[qinvokable]
        #[cxx_name = "thumbUrl"]
        fn thumb_url(self: &Library, index: i32) -> QString;

        #[qinvokable]
        fn caption(self: &Library, index: i32) -> QString;

        #[qinvokable]
        #[cxx_name = "isOffline"]
        fn is_offline(self: &Library, index: i32) -> bool;
    }
}

use std::path::PathBuf;
use std::pin::Pin;

use cxx_qt::CxxQtType;
use cxx_qt_lib::QString;
use zd_catalog::Catalog;
use zd_import::{Size, ThumbCache};

struct Item {
    thumb: String,
    caption: String,
    offline: bool,
}

pub struct LibraryRust {
    count: i32,
    status: QString,
    drives: QString,
    catalog: Option<Catalog>,
    cache: ThumbCache,
    items: Vec<Item>,
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
            drives: QString::default(),
            catalog,
            cache: ThumbCache::new(data.join("cache")),
            items: Vec::new(),
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

impl qobject::Library {
    fn reload(mut self: Pin<&mut Self>) {
        let rust = self.as_mut().rust_mut();
        let rust = rust.get_mut();
        let Some(catalog) = rust.catalog.as_ref() else { return };

        let mut items = Vec::new();
        let mut online = 0;
        let mut drives = Vec::new();
        if let Ok(vols) = catalog.volumes() {
            for v in vols.iter().filter(|v| v.online) {
                online += 1;
                if v.is_home_library {
                    drives.insert(0, format!("⌂ {}", v.label));
                } else if v.kind != zd_catalog::VolumeKind::Internal {
                    drives.push(v.label.clone());
                }
            }
        }
        if let Ok(photos) = catalog.photos(100_000, 0) {
            for p in photos {
                let offline = catalog
                    .locations(p.id)
                    .map(|ls| !ls.iter().any(|l| l.current_path().is_some()))
                    .unwrap_or(true);
                let date = p.meta.taken_at.as_deref().map(pretty_date).unwrap_or_default();
                let camera = p.meta.camera_model.clone().unwrap_or_default();
                let caption = [date, camera].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>();
                items.push(Item {
                    thumb: file_url(&rust.cache.path(&p.fingerprint, Size::Thumb)),
                    caption: caption.join(" · "),
                    offline,
                });
            }
        }
        let n = items.len() as i32;
        let offline = items.iter().filter(|i| i.offline).count();
        rust.items = items;

        let drives_text =
            if drives.is_empty() { format!("{online} drives online") } else { drives.join("  ·  ") };
        let status = match (n, offline) {
            (0, _) => "No photos yet — choose Add folder to catalogue some.".to_string(),
            (n, 0) => format!("{n} photos"),
            (n, off) => format!("{n} photos · {off} on drives that aren't plugged in"),
        };
        self.as_mut().set_drives(QString::from(&drives_text));
        self.as_mut().set_status(QString::from(&status));
        self.as_mut().set_count(n);
    }

    pub fn refresh(mut self: Pin<&mut Self>) {
        if let Some(catalog) = self.as_ref().rust().catalog.as_ref() {
            let mounts = zd_volumes::list_mounts();
            let _ = zd_volumes::refresh(catalog, &mounts);
            let _ = zd_volumes::home::connect(catalog, &mounts);
        }
        self.as_mut().reload();
    }

    pub fn scan_folder(mut self: Pin<&mut Self>, url: &QString) {
        let url = url.to_string();
        let path = url.strip_prefix("file://").unwrap_or(&url);
        // file:///C:/x -> C:/x on Windows
        let path = if cfg!(windows) { path.trim_start_matches('/') } else { path };
        let path = PathBuf::from(percent_decode(path));

        let result = {
            let rust = self.as_ref();
            let rust = rust.rust();
            match rust.catalog.as_ref() {
                Some(catalog) => {
                    let mounts = zd_volumes::list_mounts();
                    let _ = zd_volumes::refresh(catalog, &mounts);
                    zd_import::scan(catalog, &rust.cache, &path, &mut |_| {}).map_err(|e| e.to_string())
                }
                None => Err("no catalog".into()),
            }
        };
        self.as_mut().reload();
        let msg = match result {
            Ok(r) => format!(
                "Added {} new photos from {} ({} already known, {} problems)",
                r.new_photos,
                path.display(),
                r.unchanged + r.new_locations,
                r.failed.len()
            ),
            Err(e) => format!("Couldn't scan {}: {e}", path.display()),
        };
        self.as_mut().set_status(QString::from(&msg));
    }

    pub fn thumb_url(&self, index: i32) -> QString {
        self.item(index).map(|i| QString::from(&i.thumb)).unwrap_or_default()
    }

    pub fn caption(&self, index: i32) -> QString {
        self.item(index).map(|i| QString::from(&i.caption)).unwrap_or_default()
    }

    pub fn is_offline(&self, index: i32) -> bool {
        self.item(index).is_some_and(|i| i.offline)
    }

    fn item(&self, index: i32) -> Option<&Item> {
        usize::try_from(index).ok().and_then(|i| self.rust().items.get(i))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(pretty_date("2026-05-04T07:12:00"), "May 4, 2026");
        assert_eq!(pretty_date("junk"), "");
        assert_eq!(percent_decode("/run/media/c/My%20Passport"), "/run/media/c/My Passport");
        assert_eq!(file_url(std::path::Path::new("/a/b.jpg")), "file:///a/b.jpg");
    }
}
