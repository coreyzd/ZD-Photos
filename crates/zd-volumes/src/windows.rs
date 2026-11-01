//! Drive discovery on Windows: drive letters, volume serials, mapped network drives.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW};

use zd_catalog::VolumeKind;

use crate::{normalize_share, MountInfo};

// GetDriveTypeW results (winbase.h).
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..len]).to_string_lossy().into_owned()
}

pub(crate) fn list_mounts() -> Vec<MountInfo> {
    let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into()).to_uppercase();
    let mask = unsafe { GetLogicalDrives() };
    let mut out = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = format!("{letter}:\\");
        let root_w = wide(&root);
        let dtype = unsafe { GetDriveTypeW(root_w.as_ptr()) };
        if !matches!(dtype, DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_REMOTE) {
            continue; // CD drives, RAM disks, unknown
        }

        let mut label = [0u16; 261];
        let mut fs_name = [0u16; 261];
        let mut serial = 0u32;
        let ok = unsafe {
            GetVolumeInformationW(
                root_w.as_ptr(),
                label.as_mut_ptr(),
                label.len() as u32,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            )
        };
        if ok == 0 && dtype != DRIVE_REMOTE {
            continue; // empty card reader slot, locked BitLocker drive, ...
        }
        let label = Some(from_wide(&label)).filter(|s| !s.is_empty());
        let fs_id = (ok != 0 && dtype != DRIVE_REMOTE)
            .then(|| format!("{:04X}-{:04X}", serial >> 16, serial & 0xFFFF));

        let fs_name = from_wide(&fs_name);
        let (kind, network_path) = if crate::looks_like_cloud(label.as_deref(), Some(&fs_name)) {
            (VolumeKind::Cloud, None) // Google Drive for desktop shows up as G:\
        } else if dtype == DRIVE_REMOTE {
            (VolumeKind::Network, mapped_share(letter))
        } else if dtype == DRIVE_REMOVABLE || !format!("{letter}:").eq_ignore_ascii_case(&system_drive) {
            // USB hard drives report as FIXED; anything that isn't the system drive is treated
            // as detachable, which is the safe assumption for "might go away".
            (VolumeKind::Removable, None)
        } else {
            (VolumeKind::Internal, None)
        };

        out.push(MountInfo {
            mount: PathBuf::from(&root),
            label: label
                .or_else(|| network_path.as_ref().and_then(|p| p.rsplit('/').next().map(str::to_string))),
            fs_id,
            network_path,
            kind,
            device: Some(format!("{letter}:")),
        });
    }
    out
}

/// `Z:` -> `//nobara-zd.local/photos` for a mapped network drive.
fn mapped_share(letter: char) -> Option<String> {
    let local = wide(&format!("{letter}:"));
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let rc = unsafe { WNetGetConnectionW(local.as_ptr(), buf.as_mut_ptr(), &mut len) };
    (rc == 0).then(|| normalize_share(&from_wide(&buf)))
}
