//! Drive identity for ZD-Photos.
//!
//! Finds what's mounted (internal disks, USB drives, network shares, SD cards),
//! gives each a permanent identity, and records sightings in the catalog so a
//! drive is recognised again after it's unplugged, re-lettered or remounted.
//!
//! Identity, in order of preference:
//! 1. our marker file (`.zdphotos-volume`) at the root;
//! 2. for media we must not write to (SD cards, read-only drives): the filesystem
//!    UUID / volume serial, or the network share path;
//! 3. a volume we already know with the same filesystem id (marker was deleted).

mod marker;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};

use zd_catalog::{Catalog, Volume, VolumeKind, VolumeSighting};

pub use marker::{read_marker, write_marker, Marker, MARKER_NAME};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Catalog(#[from] zd_catalog::Error),
    #[error("{0} is not inside any known drive")]
    NotOnVolume(PathBuf),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One mounted filesystem as the OS reports it.
#[derive(Debug, Clone)]
pub struct MountInfo {
    pub mount: PathBuf,
    pub label: Option<String>,
    pub fs_id: Option<String>,
    pub network_path: Option<String>,
    pub kind: VolumeKind,
    pub device: Option<String>,
}

/// Everything currently mounted that could hold photos.
pub fn list_mounts() -> Vec<MountInfo> {
    #[cfg(target_os = "linux")]
    return linux::list_mounts();
    #[cfg(windows)]
    return windows::list_mounts();
    #[allow(unreachable_code)]
    Vec::new()
}

/// A camera card has a DCIM folder at its root.
pub fn is_camera_card(root: &Path) -> bool {
    root.join("DCIM").is_dir()
}

/// Normalise `smb://Host/Share/`, `\\Host\Share` and `//host/share` to `//host/share`
/// so the same share matches across Linux and Windows.
pub fn normalize_share(s: &str) -> String {
    let s = s.trim();
    let s = s.strip_prefix("smb:").unwrap_or(s);
    let s = s.replace('\\', "/");
    let s = s.trim_start_matches('/').trim_end_matches('/');
    format!("//{}", s.to_lowercase())
}

/// Work out the identity of a mounted filesystem.
///
/// `known_by_fs_id` lets the caller re-use an existing volume when the marker has gone.
/// `allow_marker` controls whether we may create a marker file on this volume.
pub fn identify(
    m: &MountInfo,
    allow_marker: bool,
    known_by_fs_id: impl Fn(&str) -> Option<String>,
) -> VolumeSighting {
    let card = is_camera_card(&m.mount);
    let kind = if card { VolumeKind::Card } else { m.kind };
    let default_label = default_label(m, kind);

    let derived_uuid = || -> Option<String> {
        if let Some(id) = m.fs_id.as_deref() {
            if let Some(uuid) = known_by_fs_id(id) {
                return Some(uuid);
            }
        }
        if let Some(share) = &m.network_path {
            return Some(format!("net:{share}"));
        }
        m.fs_id.as_ref().map(|id| format!("fs:{id}"))
    };

    let uuid = if let Some(marker) = read_marker(&m.mount) {
        marker.uuid
    } else if let Some(uuid) = card.then(&derived_uuid).flatten() {
        uuid // never write to a camera card
    } else if let Some(uuid) = m.fs_id.as_deref().and_then(&known_by_fs_id) {
        uuid
    } else if let Some(marker) = allow_marker.then(|| write_marker(&m.mount, &default_label).ok()).flatten() {
        marker.uuid
    } else {
        derived_uuid().unwrap_or_else(|| format!("path:{}", m.mount.display()))
    };

    VolumeSighting {
        uuid,
        label: default_label,
        kind,
        fs_id: m.fs_id.clone(),
        network_path: m.network_path.clone(),
        mount: m.mount.to_string_lossy().into_owned(),
    }
}

fn default_label(m: &MountInfo, kind: VolumeKind) -> String {
    if let Some(l) = m.label.as_ref().filter(|l| !l.is_empty()) {
        return l.clone();
    }
    match kind {
        VolumeKind::Card => "SD card".into(),
        VolumeKind::Network => "Network share".into(),
        VolumeKind::Internal if m.mount == Path::new("/") => "This computer".into(),
        _ => m
            .mount
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.mount.display().to_string()),
    }
}

/// Re-detect all volumes and update the catalog: every known volume goes offline,
/// then everything currently mounted comes back online (and new ones are added).
///
/// Markers are only written on removable and network volumes; internal disks and
/// cards are identified by filesystem id.
pub fn refresh(catalog: &Catalog, mounts: &[MountInfo]) -> Result<Vec<Volume>> {
    let known = catalog.volumes()?;
    let by_fs = |id: &str| known.iter().find(|v| v.fs_id.as_deref() == Some(id)).map(|v| v.uuid.clone());
    catalog.batch(|c| {
        c.mark_all_volumes_offline()?;
        let mut seen = Vec::new();
        for m in mounts {
            let allow = matches!(m.kind, VolumeKind::Removable | VolumeKind::Network);
            let s = identify(m, allow, by_fs);
            seen.push(c.volume_seen(&s)?);
        }
        Ok::<_, Error>(seen)
    })
}

/// Find which online volume a path lives on, and its path relative to that volume's root
/// (always `/`-separated, so it's the same on Linux and Windows).
pub fn locate<'a>(path: &Path, volumes: &'a [Volume]) -> Option<(&'a Volume, String)> {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    volumes
        .iter()
        .filter(|v| v.online)
        .filter_map(|v| {
            let root = PathBuf::from(v.last_mount.as_ref()?);
            let root = std::fs::canonicalize(&root).unwrap_or(root);
            let rel = path.strip_prefix(&root).ok()?;
            Some((v, root.components().count(), rel_string(rel)))
        })
        .max_by_key(|(_, depth, _)| *depth)
        .map(|(v, _, rel)| (v, rel))
}

pub fn rel_string(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// The Home Library: the family network share that backs everything up.
pub mod home {
    use super::*;

    /// Setting key holding the share path (`//nobara-zd.local/photos`).
    pub const SETTING: &str = "home_library.share";

    /// Remember which share is the Home Library.
    pub fn configure(catalog: &Catalog, share: &str) -> zd_catalog::Result<()> {
        catalog.set_setting(SETTING, &normalize_share(share))
    }

    /// Setting key for an explicit local folder that *is* the share (overrides detection).
    pub const LOCAL_SETTING: &str = "home_library.local_path";

    /// Use this folder as the Home Library on this computer (e.g. the folder Samba exports,
    /// when this computer is the server).
    pub fn set_local_path(catalog: &Catalog, path: Option<&Path>) -> zd_catalog::Result<()> {
        catalog
            .set_setting(LOCAL_SETTING, &path.map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())
    }

    /// Where the configured share can be reached right now, if anywhere, in this order:
    /// 1. a folder set explicitly with [`set_local_path`];
    /// 2. a mounted or mapped drive pointing at it (cifs/fstab, GVfs, Windows mapped drive);
    /// 3. on the server itself: the folder Samba exports for that share;
    /// 4. KDE's kio-fuse mount (what Dolphin uses when kio-fuse is installed);
    /// 5. on Windows, the UNC path itself.
    pub fn find(catalog: &Catalog, mounts: &[MountInfo]) -> zd_catalog::Result<Option<MountInfo>> {
        let Some(share) = catalog.setting(SETTING)? else { return Ok(None) };
        let as_home = |p: PathBuf| MountInfo {
            label: Some("Home Library".into()),
            mount: p,
            fs_id: None,
            network_path: Some(share.clone()),
            kind: VolumeKind::Network,
            device: None,
        };
        if let Some(local) = catalog.setting(LOCAL_SETTING)?.filter(|s| !s.is_empty()) {
            let p = PathBuf::from(local);
            return Ok(p.is_dir().then(|| as_home(p)));
        }
        if let Some(m) = mounts.iter().find(|m| m.network_path.as_deref() == Some(share.as_str())) {
            return Ok(Some(m.clone()));
        }
        Ok(candidate_paths(&share).into_iter().find(|p| p.is_dir()).map(as_home))
    }

    #[cfg(windows)]
    fn candidate_paths(share: &str) -> Vec<PathBuf> {
        vec![PathBuf::from(share.replace('/', "\\"))]
    }

    #[cfg(target_os = "linux")]
    fn candidate_paths(share: &str) -> Vec<PathBuf> {
        let mut out = crate::linux::served_locally(share);
        out.extend(crate::linux::kio_fuse_paths(share));
        out
    }

    #[cfg(not(any(windows, target_os = "linux")))]
    fn candidate_paths(_share: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    /// Detect the Home Library, record it in the catalog and flag it as home.
    pub fn connect(catalog: &Catalog, mounts: &[MountInfo]) -> Result<Option<Volume>> {
        let Some(m) = find(catalog, mounts)? else { return Ok(None) };
        let known = catalog.volumes()?;
        let mut s = identify(&m, true, |id| {
            known.iter().find(|v| v.fs_id.as_deref() == Some(id)).map(|v| v.uuid.clone())
        });
        s.label = "Home Library".into();
        let v = catalog.volume_seen(&s)?;
        if !v.is_home_library {
            // First time it becomes home: give it the friendly name she'll see in the sidebar.
            catalog.rename_volume(v.id, "Home Library")?;
            catalog.set_home_library(v.id)?;
        }
        Ok(catalog.volume(v.id)?)
    }
}

#[cfg(test)]
mod tests;
