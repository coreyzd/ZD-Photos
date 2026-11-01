use super::*;

fn sighting(uuid: &str, mount: &str) -> VolumeSighting {
    VolumeSighting {
        uuid: uuid.into(),
        label: "Blue WD drive".into(),
        kind: VolumeKind::Removable,
        fs_id: Some("ABCD-1234".into()),
        network_path: None,
        mount: mount.into(),
    }
}

fn meta() -> PhotoMeta {
    PhotoMeta {
        mime: "image/jpeg".into(),
        width: Some(6960),
        height: Some(4640),
        orientation: 1,
        taken_at: Some("2026-05-04T07:12:00".into()),
        camera_make: Some("Canon".into()),
        camera_model: Some("Canon EOS R7".into()),
        ..Default::default()
    }
}

#[test]
fn migrates_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("catalog.sqlite");
    {
        let c = Catalog::open(&path).unwrap();
        assert_eq!(c.schema_version().unwrap(), schema::MIGRATIONS.len() as u32);
        c.set_setting("x", "1").unwrap();
    }
    let c = Catalog::open(&path).unwrap();
    assert_eq!(c.setting("x").unwrap().as_deref(), Some("1"));
}

#[test]
fn volume_keeps_label_when_remounted_elsewhere() {
    let c = Catalog::open_in_memory().unwrap();
    let v = c.volume_seen(&sighting("vol-1", "E:\\")).unwrap();
    c.rename_volume(v.id, "Birds 2025").unwrap();
    c.mark_all_volumes_offline().unwrap();
    assert!(!c.volume(v.id).unwrap().unwrap().online);

    // Plugged back in, but Windows gave it a different letter.
    let v2 = c.volume_seen(&sighting("vol-1", "F:\\")).unwrap();
    assert_eq!(v2.id, v.id);
    assert_eq!(v2.label, "Birds 2025");
    assert_eq!(v2.last_mount.as_deref(), Some("F:\\"));
    assert!(v2.online);
}

#[test]
fn same_photo_in_two_places_is_one_photo() {
    let c = Catalog::open_in_memory().unwrap();
    let drive = c.volume_seen(&sighting("drive", "/media/drive")).unwrap();
    let mut home = sighting("home", "/mnt/photos");
    home.kind = VolumeKind::Network;
    let home = c.volume_seen(&home).unwrap();
    c.set_home_library(home.id).unwrap();

    let (a, created) = c.add_photo("fp1", 100, &meta()).unwrap();
    assert!(created);
    c.put_file(a, drive.id, "2026/IMG_0001.JPG", 100, 1).unwrap();
    assert!(!c.is_backed_up(a).unwrap());

    let (b, created) = c.add_photo("fp1", 100, &meta()).unwrap();
    assert!(!created);
    assert_eq!(a, b);
    c.put_file(b, home.id, "Backup/IMG_0001.JPG", 100, 1).unwrap();
    assert!(c.is_backed_up(a).unwrap());

    let locs = c.locations(a).unwrap();
    assert_eq!(locs.len(), 2);
    let s = c.stats().unwrap();
    assert_eq!((s.photos, s.files, s.backed_up), (1, 2, 1));
}

#[test]
fn unseen_files_marked_missing_but_photo_kept() {
    let c = Catalog::open_in_memory().unwrap();
    let v = c.volume_seen(&sighting("drive", "/media/drive")).unwrap();
    let (p, _) = c.add_photo("fp1", 100, &meta()).unwrap();
    c.put_file(p, v.id, "a.jpg", 100, 1).unwrap();
    let (q, _) = c.add_photo("fp2", 100, &meta()).unwrap();
    c.put_file(q, v.id, "b.jpg", 100, 1).unwrap();
    let tag = c.tag_path("Birds/Herons").unwrap();
    c.tag_photo(q, tag).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(2));
    let started = timestamp();
    c.touch_file(v.id, "a.jpg").unwrap();
    assert_eq!(c.mark_unseen_missing(v.id, "", &started).unwrap(), 1);

    let locs = c.locations(q).unwrap();
    assert!(locs[0].missing);
    assert!(locs[0].current_path().is_none());
    assert_eq!(c.photo_tags(q).unwrap(), vec!["Birds/Herons".to_string()]);
    assert!(c.locations(p).unwrap()[0].current_path().is_some());
}

#[test]
fn offline_volume_has_no_current_path() {
    let c = Catalog::open_in_memory().unwrap();
    let v = c.volume_seen(&sighting("drive", "/media/drive")).unwrap();
    let (p, _) = c.add_photo("fp1", 100, &meta()).unwrap();
    c.put_file(p, v.id, "2026/a.jpg", 100, 1).unwrap();
    let loc = &c.locations(p).unwrap()[0];
    assert_eq!(loc.current_path().unwrap(), std::path::Path::new("/media/drive").join("2026").join("a.jpg"));
    c.set_volume_online(v.id, false).unwrap();
    assert!(c.locations(p).unwrap()[0].current_path().is_none());
}

#[test]
fn hierarchical_tags_are_reused() {
    let c = Catalog::open_in_memory().unwrap();
    let a = c.tag_path("Birds/Herons").unwrap();
    let b = c.tag_path(" Birds / Herons ").unwrap();
    assert_eq!(a, b);
    let warblers = c.tag_path("Birds/Warblers").unwrap();
    let (p, _) = c.add_photo("fp", 1, &meta()).unwrap();
    c.tag_photo(p, a).unwrap();
    c.tag_photo(p, warblers).unwrap();
    c.tag_photo(p, warblers).unwrap();
    assert_eq!(c.photo_tags(p).unwrap(), vec!["Birds/Herons", "Birds/Warblers"]);
}

#[test]
fn only_one_home_library() {
    let c = Catalog::open_in_memory().unwrap();
    let a = c.volume_seen(&sighting("a", "/a")).unwrap();
    let b = c.volume_seen(&sighting("b", "/b")).unwrap();
    c.set_home_library(a.id).unwrap();
    c.set_home_library(b.id).unwrap();
    assert_eq!(c.home_library().unwrap().unwrap().id, b.id);
    assert!(c.set_home_library(999).is_err());
    assert_eq!(c.home_library().unwrap().unwrap().id, b.id);
}

#[test]
fn orientation_class_respects_exif_rotation() {
    let c = Catalog::open_in_memory().unwrap();
    let mut m = meta();
    m.orientation = 6; // rotated 90°: stored landscape, displayed portrait
    let (p, _) = c.add_photo("fp", 1, &m).unwrap();
    assert_eq!(c.photo(p).unwrap().unwrap().orientation_class(), Some("portrait"));
}

#[test]
fn missing_scoped_to_scanned_folder() {
    let c = Catalog::open_in_memory().unwrap();
    let v = c.volume_seen(&sighting("drive", "/d")).unwrap();
    for (fp, path) in [("1", "2025/a.jpg"), ("2", "2026/b.jpg"), ("3", "2026_x/c.jpg")] {
        let (p, _) = c.add_photo(fp, 1, &meta()).unwrap();
        c.put_file(p, v.id, path, 1, 1).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(2));
    let started = timestamp();
    // Rescanned only the 2026 folder and found nothing there.
    assert_eq!(c.mark_unseen_missing(v.id, "2026", &started).unwrap(), 1);
    assert_eq!(c.stats().unwrap().files, 2);
}

#[test]
fn summaries_and_counts() {
    let c = Catalog::open_in_memory().unwrap();
    let a = c.volume_seen(&sighting("a", "/a")).unwrap();
    let mut bs = sighting("b", "/b");
    bs.fs_id = Some("other".into());
    let b = c.volume_seen(&bs).unwrap();
    let (p1, _) = c.add_photo("fp1", 1, &meta()).unwrap();
    c.put_file(p1, a.id, "x.jpg", 1, 1).unwrap();
    let mut vm = meta();
    vm.mime = "video/mp4".into();
    vm.duration_ms = Some(5000);
    vm.taken_at = Some("2026-06-01T00:00:00".into());
    let (p2, _) = c.add_photo("fp2", 1, &vm).unwrap();
    c.put_file(p2, b.id, "v.mp4", 1, 1).unwrap();
    c.set_volume_online(b.id, false).unwrap();

    let all = c.summaries(None).unwrap();
    assert_eq!(all.iter().map(|s| s.id).collect::<Vec<_>>(), vec![p2, p1]); // newest first
    assert!(all[0].is_video && !all[0].available && all[0].duration_ms == Some(5000));
    assert!(all[1].available);
    assert_eq!(c.summaries(Some(a.id)).unwrap().len(), 1);
    let mut counts = c.volume_photo_counts().unwrap();
    counts.sort();
    assert_eq!(counts, vec![(a.id, 1), (b.id, 1)]);
}
