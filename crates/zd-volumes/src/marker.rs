//! The marker file: a tiny JSON file at a drive's root that gives it a
//! permanent identity, whatever letter or mount point it gets.

use std::fs;
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const MARKER_NAME: &str = ".zdphotos-volume";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Marker {
    pub uuid: String,
    pub label: String,
    pub created: String,
    #[serde(default)]
    pub app: String,
}

pub fn read_marker(root: &Path) -> Option<Marker> {
    let text = fs::read_to_string(root.join(MARKER_NAME)).ok()?;
    let m: Marker = serde_json::from_str(&text).ok()?;
    (!m.uuid.trim().is_empty()).then_some(m)
}

/// Write a new marker. Fails (without side effects) if the root isn't writable.
pub fn write_marker(root: &Path, label: &str) -> std::io::Result<Marker> {
    let m = Marker {
        uuid: uuid::Uuid::new_v4().to_string(),
        label: label.to_string(),
        created: chrono::Utc::now().to_rfc3339(),
        app: "ZD-Photos".into(),
    };
    let path = root.join(MARKER_NAME);
    let tmp = root.join(format!("{MARKER_NAME}.tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(serde_json::to_string_pretty(&m).unwrap_or_default().as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &path)?;
    hide(&path);
    Ok(m)
}

#[cfg(windows)]
fn hide(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN};
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        SetFileAttributesW(wide.as_ptr(), FILE_ATTRIBUTE_HIDDEN);
    }
}

#[cfg(not(windows))]
fn hide(_path: &Path) {} // the leading dot already hides it

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_marker(d.path()).is_none());
        let m = write_marker(d.path(), "Blue drive").unwrap();
        assert_eq!(read_marker(d.path()).unwrap(), m);
        assert!(!d.path().join(format!("{MARKER_NAME}.tmp")).exists());
    }

    #[test]
    fn garbage_marker_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(MARKER_NAME), "not json").unwrap();
        assert!(read_marker(d.path()).is_none());
    }
}
