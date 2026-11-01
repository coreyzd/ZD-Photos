use super::*;

fn mount(path: &Path, kind: VolumeKind) -> MountInfo {
    MountInfo {
        mount: path.to_path_buf(),
        label: Some("My Passport".into()),
        fs_id: Some("1234-ABCD".into()),
        network_path: None,
        kind,
        device: None,
    }
}

#[test]
fn share_paths_normalise_across_platforms() {
    let want = "//nobara-zd.local/photos";
    assert_eq!(normalize_share("smb://nobara-zd.local/Photos/"), want);
    assert_eq!(normalize_share(r"\\nobara-zd.local\Photos"), want);
    assert_eq!(normalize_share("//NOBARA-ZD.local/photos"), want);
}

#[test]
fn removable_drive_gets_marker_and_keeps_identity_when_moved() {
    let a = tempfile::tempdir().unwrap();
    let c = Catalog::open_in_memory().unwrap();

    let v1 = refresh(&c, &[mount(a.path(), VolumeKind::Removable)]).unwrap();
    assert!(a.path().join(MARKER_NAME).exists());
    let uuid = v1[0].uuid.clone();

    // Unplugged: nothing mounted.
    refresh(&c, &[]).unwrap();
    assert!(!c.volumes().unwrap()[0].online);

    // Replugged at a different mount point (new drive letter): same volume.
    let b = tempfile::tempdir().unwrap();
    std::fs::copy(a.path().join(MARKER_NAME), b.path().join(MARKER_NAME)).unwrap();
    let v2 = refresh(&c, &[mount(b.path(), VolumeKind::Removable)]).unwrap();
    assert_eq!(v2[0].uuid, uuid);
    assert_eq!(c.volumes().unwrap().len(), 1);
    assert!(v2[0].online);
}

#[test]
fn camera_card_is_never_written_to() {
    let card = tempfile::tempdir().unwrap();
    std::fs::create_dir(card.path().join("DCIM")).unwrap();
    let c = Catalog::open_in_memory().unwrap();
    let v = refresh(&c, &[mount(card.path(), VolumeKind::Removable)]).unwrap();
    assert_eq!(v[0].kind, VolumeKind::Card);
    assert_eq!(v[0].uuid, "fs:1234-ABCD");
    assert!(!card.path().join(MARKER_NAME).exists());
}

#[test]
fn internal_disk_identified_by_fs_id_without_marker() {
    let d = tempfile::tempdir().unwrap();
    let c = Catalog::open_in_memory().unwrap();
    let v = refresh(&c, &[mount(d.path(), VolumeKind::Internal)]).unwrap();
    assert_eq!(v[0].uuid, "fs:1234-ABCD");
    assert!(!d.path().join(MARKER_NAME).exists());
}

#[test]
fn lost_marker_falls_back_to_known_fs_id() {
    let d = tempfile::tempdir().unwrap();
    let c = Catalog::open_in_memory().unwrap();
    let first = refresh(&c, &[mount(d.path(), VolumeKind::Removable)]).unwrap()[0].uuid.clone();
    std::fs::remove_file(d.path().join(MARKER_NAME)).unwrap();
    let again = refresh(&c, &[mount(d.path(), VolumeKind::Removable)]).unwrap()[0].uuid.clone();
    assert_eq!(first, again);
}

#[test]
fn locate_picks_deepest_volume() {
    let root = tempfile::tempdir().unwrap();
    let inner = root.path().join("drive");
    std::fs::create_dir_all(inner.join("2026/May")).unwrap();
    let file = inner.join("2026/May/IMG_0001.JPG");
    std::fs::write(&file, b"x").unwrap();

    let c = Catalog::open_in_memory().unwrap();
    let mut outer = mount(root.path(), VolumeKind::Internal);
    outer.fs_id = Some("outer".into());
    let mut drive = mount(&inner, VolumeKind::Removable);
    drive.fs_id = Some("inner".into());
    let vols = refresh(&c, &[outer, drive]).unwrap();

    let (v, rel) = locate(&file, &vols).unwrap();
    assert_eq!(v.kind, VolumeKind::Removable);
    assert_eq!(rel, "2026/May/IMG_0001.JPG");
}

#[test]
fn home_library_found_by_share_and_flagged() {
    let share_dir = tempfile::tempdir().unwrap();
    let c = Catalog::open_in_memory().unwrap();
    home::configure(&c, "smb://nobara-zd.local/Photos/").unwrap();

    // Not mounted anywhere (and not Windows): not found, nothing breaks.
    #[cfg(not(windows))]
    assert!(home::connect(&c, &[]).unwrap().is_none());

    let m = MountInfo {
        mount: share_dir.path().to_path_buf(),
        label: Some("photos".into()),
        fs_id: None,
        network_path: Some(normalize_share(r"\\nobara-zd.local\Photos")),
        kind: VolumeKind::Network,
        device: None,
    };
    let v = home::connect(&c, &[m]).unwrap().unwrap();
    assert!(v.is_home_library);
    assert_eq!(v.label, "Home Library");
    assert!(share_dir.path().join(MARKER_NAME).exists());
    assert_eq!(c.home_library().unwrap().unwrap().id, v.id);
}
