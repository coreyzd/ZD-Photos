//! Mount discovery on Linux: /proc/self/mounts, /dev/disk/by-uuid, sysfs, and GVfs.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use zd_catalog::VolumeKind;

use crate::{normalize_share, MountInfo};

const REAL_FS: &[&str] = &[
    "ext2", "ext3", "ext4", "btrfs", "xfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "fuseblk", "hfsplus",
    "apfs", "zfs",
];
const NET_FS: &[&str] = &["cifs", "smb3", "smbfs", "nfs", "nfs4"];

pub(crate) fn list_mounts() -> Vec<MountInfo> {
    let text = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let uuids = reverse_links("/dev/disk/by-uuid");
    let labels = reverse_links("/dev/disk/by-label");
    let mut out = parse_mounts(&text, &uuids, &labels);
    out.extend(gvfs_mounts());
    // A btrfs disk can be mounted several times (subvolumes); keep the first per mount point.
    let mut seen = std::collections::HashSet::new();
    out.retain(|m| seen.insert(m.mount.clone()));
    out
}

pub(crate) fn parse_mounts(
    text: &str,
    uuids: &HashMap<PathBuf, String>,
    labels: &HashMap<PathBuf, String>,
) -> Vec<MountInfo> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut f = line.split_whitespace();
        let (Some(dev), Some(mnt), Some(fstype)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let mnt = unescape(mnt);
        let dev = unescape(dev);
        if !(REAL_FS.contains(&fstype) || NET_FS.contains(&fstype)) || skip_mount(&mnt) {
            continue;
        }
        let mount = PathBuf::from(&mnt);
        if NET_FS.contains(&fstype) {
            out.push(MountInfo {
                label: dev.rsplit('/').find(|s| !s.is_empty()).map(str::to_string),
                network_path: Some(normalize_share(&dev)),
                kind: VolumeKind::Network,
                fs_id: None,
                device: Some(dev),
                mount,
            });
            continue;
        }
        let devpath = fs::canonicalize(&dev).unwrap_or_else(|_| PathBuf::from(&dev));
        let kind = if is_usb(&devpath) || mnt.starts_with("/media/") || mnt.starts_with("/run/media/") {
            VolumeKind::Removable
        } else {
            VolumeKind::Internal
        };
        out.push(MountInfo {
            label: labels.get(&devpath).cloned(),
            fs_id: uuids.get(&devpath).cloned(),
            network_path: None,
            kind,
            device: Some(dev),
            mount,
        });
    }
    out
}

fn skip_mount(mnt: &str) -> bool {
    mnt == "/boot"
        || mnt.starts_with("/boot/")
        || mnt.starts_with("/proc")
        || mnt.starts_with("/sys")
        || mnt.starts_with("/dev")
        || mnt.starts_with("/snap/")
        || mnt.starts_with("/var/lib/")
        || (mnt.starts_with("/run/") && !mnt.starts_with("/run/media/"))
}

/// /proc/mounts escapes spaces and friends as octal (`\040`).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0');
            out.push(v);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Map resolved device path -> link name for /dev/disk/by-* directories.
/// udev escapes characters in labels as `\x20`.
fn reverse_links(dir: &str) -> HashMap<PathBuf, String> {
    let mut map = HashMap::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            if let Ok(target) = fs::canonicalize(e.path()) {
                map.insert(target, unescape_udev(&e.file_name().to_string_lossy()));
            }
        }
    }
    map
}

fn unescape_udev(s: &str) -> String {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            if let Ok(v) = u8::from_str_radix(&s[i + 2..i + 4], 16) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// True if the block device hangs off a USB bus (external drive, card reader).
fn is_usb(devpath: &Path) -> bool {
    let Some(name) = devpath.file_name() else { return false };
    let sys = Path::new("/sys/class/block").join(name);
    fs::canonicalize(sys).map(|p| p.to_string_lossy().contains("/usb")).unwrap_or(false)
}

/// Shares mounted through the file manager (Dolphin/KIO uses `smb://` URLs directly,
/// GNOME uses GVfs under the runtime dir).
fn gvfs_mounts() -> Vec<MountInfo> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let uid = fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Uid:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
            })
            .unwrap_or_else(|| "1000".into());
        PathBuf::from("/run/user").join(uid)
    });
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(runtime.join("gvfs")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(share) = parse_gvfs_name(&name) {
                out.push(MountInfo {
                    mount: e.path(),
                    label: share.rsplit('/').next().map(str::to_string),
                    network_path: Some(share),
                    kind: VolumeKind::Network,
                    fs_id: None,
                    device: None,
                });
            }
        }
    }
    out
}

/// `smb-share:server=nobara-zd.local,share=photos` -> `//nobara-zd.local/photos`
pub(crate) fn parse_gvfs_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix("smb-share:")?;
    let mut server = None;
    let mut share = None;
    for kv in rest.split(',') {
        match kv.split_once('=') {
            Some(("server", v)) => server = Some(v),
            Some(("share", v)) => share = Some(v),
            _ => {}
        }
    }
    Some(normalize_share(&format!("//{}/{}", server?, share?)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_mounts() {
        let text = "\
/dev/nvme0n1p3 / btrfs rw 0 0
proc /proc proc rw 0 0
/dev/nvme0n1p3 /home btrfs rw 0 0
/dev/sdb1 /run/media/coreyzd/My\\040Passport exfat rw 0 0
//nobara-zd.local/Photos /mnt/photos cifs rw 0 0
tmpfs /run/user/1000 tmpfs rw 0 0
";
        let mut uuids = HashMap::new();
        uuids.insert(PathBuf::from("/dev/sdb1"), "1234-ABCD".to_string());
        let m = parse_mounts(text, &uuids, &HashMap::new());
        let mounts: Vec<_> = m.iter().map(|m| m.mount.to_string_lossy().into_owned()).collect();
        assert_eq!(mounts, vec!["/", "/home", "/run/media/coreyzd/My Passport", "/mnt/photos"]);
        assert_eq!(m[2].kind, VolumeKind::Removable);
        assert_eq!(m[2].fs_id.as_deref(), Some("1234-ABCD"));
        assert_eq!(m[3].kind, VolumeKind::Network);
        assert_eq!(m[3].network_path.as_deref(), Some("//nobara-zd.local/photos"));
    }

    #[test]
    fn parses_gvfs() {
        assert_eq!(
            parse_gvfs_name("smb-share:server=nobara-zd.local,share=photos").as_deref(),
            Some("//nobara-zd.local/photos")
        );
        assert_eq!(parse_gvfs_name("mtp:host=Google_Pixel"), None);
    }

    #[test]
    fn udev_labels() {
        assert_eq!(unescape_udev("My\\x20Passport"), "My Passport");
    }
}
