use super::testutil::{drive, jpeg};
use super::*;
use zd_catalog::VolumeKind;

fn noop(_: &Progress) {}

#[test]
fn scan_then_rescan_is_incremental() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("drive");
    jpeg(&root.join("2026/a.jpg"), 800, 600, 1);
    jpeg(&root.join("2026/b.jpg"), 600, 800, 2);
    jpeg(&root.join(".Trash-1000/junk.jpg"), 10, 10, 3);
    std::fs::write(root.join("notes.txt"), "hi").unwrap();

    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &root, "DRIVE-1", VolumeKind::Removable);

    let r = scan(&c, &cache, &root, &mut noop).unwrap();
    assert_eq!((r.files_found, r.new_photos, r.thumbnails_made), (2, 2, 2));
    assert!(r.failed.is_empty(), "{:?}", r.failed);

    let r = scan(&c, &cache, &root, &mut noop).unwrap();
    assert_eq!((r.unchanged, r.new_photos, r.missing), (2, 0, 0));

    let photos = c.photos(10, 0).unwrap();
    assert_eq!(photos.len(), 2);
    assert!(photos.iter().all(|p| p.has_thumbnail && p.meta.width.is_some()));
}

#[test]
fn moved_file_keeps_its_tags() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("drive");
    jpeg(&root.join("inbox/heron.jpg"), 400, 300, 7);
    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &root, "DRIVE-1", VolumeKind::Removable);
    scan(&c, &cache, &root, &mut noop).unwrap();

    let id = c.photos(1, 0).unwrap()[0].id;
    let tag = c.tag_path("Birds/Great Blue Heron").unwrap();
    c.tag_photo(id, tag).unwrap();

    std::fs::create_dir_all(root.join("Birds")).unwrap();
    std::fs::rename(root.join("inbox/heron.jpg"), root.join("Birds/GBH.jpg")).unwrap();
    let r = scan(&c, &cache, &root, &mut noop).unwrap();
    assert_eq!((r.new_photos, r.new_locations, r.missing), (0, 1, 1));

    let locs = c.locations(id).unwrap();
    let live: Vec<_> = locs.iter().filter(|l| !l.missing).map(|l| l.rel_path.as_str()).collect();
    assert_eq!(live, vec!["Birds/GBH.jpg"]);
    assert_eq!(c.photo_tags(id).unwrap(), vec!["Birds/Great Blue Heron"]);
}

#[test]
fn unplugged_drive_still_browsable_from_cache() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("drive");
    jpeg(&root.join("a.jpg"), 300, 200, 1);
    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &root, "DRIVE-1", VolumeKind::Removable);
    scan(&c, &cache, &root, &mut noop).unwrap();

    zd_volumes::refresh(&c, &[]).unwrap(); // unplugged
    let p = &c.photos(1, 0).unwrap()[0];
    assert!(c.locations(p.id).unwrap()[0].current_path().is_none());
    assert!(cache.path(&p.fingerprint, Size::Thumb).is_file());
    assert!(cache.path(&p.fingerprint, Size::Preview).is_file());
}

#[test]
fn scanning_a_subfolder_only_flags_that_folder() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("drive");
    jpeg(&root.join("2025/a.jpg"), 100, 100, 1);
    jpeg(&root.join("2026/b.jpg"), 100, 100, 2);
    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &root, "DRIVE-1", VolumeKind::Removable);
    scan(&c, &cache, &root, &mut noop).unwrap();

    let r = scan(&c, &cache, &root.join("2026"), &mut noop).unwrap();
    assert_eq!((r.files_found, r.unchanged, r.missing), (1, 1, 0));
}

#[test]
fn card_import_copies_verifies_and_skips_known() {
    let d = tempfile::tempdir().unwrap();
    let card = d.path().join("card");
    let laptop = d.path().join("laptop");
    jpeg(&card.join("DCIM/100CANON/IMG_0001.JPG"), 300, 200, 1);
    jpeg(&card.join("DCIM/100CANON/IMG_0002.JPG"), 300, 200, 2);
    std::fs::create_dir_all(&laptop).unwrap();

    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &laptop, "LAPTOP", VolumeKind::Removable);
    drive(&c, &card, "CARD", VolumeKind::Removable);

    let plan = card::plan(&c, &card).unwrap();
    assert_eq!(plan.len(), 2);
    assert!(plan.iter().all(|i| !i.already_imported));

    let dest = laptop.join("Pictures");
    let rep = card::import(&c, &cache, &plan, &dest, &mut noop).unwrap();
    assert_eq!(rep.copied.len(), 2, "{:?}", rep.failed);
    for (_, copy) in &rep.copied {
        assert!(copy.starts_with(&dest));
        assert!(copy.is_file());
    }
    assert_eq!(rep.catalog.new_photos, 2);
    let p = &c.photos(10, 0).unwrap()[0];
    assert!(c.photo(p.id).unwrap().unwrap().full_hash.is_some());

    // Plug the card in again: nothing new.
    let plan = card::plan(&c, &card).unwrap();
    assert!(plan.iter().all(|i| i.already_imported));
    let rep2 = card::import(&c, &cache, &plan, &dest, &mut noop).unwrap();
    assert_eq!((rep2.copied.len(), rep2.skipped_already_imported), (0, 2));

    // Clearing deletes only verified files.
    assert_eq!(card::clear_verified(&rep), 2);
    assert!(card::plan(&c, &card).unwrap().is_empty());
}

#[test]
fn dated_folders() {
    assert_eq!(card::dated_folder(Some("2026-05-04T07:12:00")), Path::new("2026").join("2026-05-04"));
    assert_eq!(card::dated_folder(None), Path::new("Undated"));
    assert_eq!(card::dated_folder(Some("bad")), Path::new("Undated"));
}

#[test]
fn videos_are_catalogued_with_duration_and_poster() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("drive");
    std::fs::create_dir_all(&root).unwrap();
    // Canon puts a small JPEG thumbnail in a uuid box; use a real one so the fallback works.
    let thm = d.path().join("thm.jpg");
    jpeg(&thm, 160, 90, 5);
    let mp4 = video::tests::fake_mp4(1_777_878_720, 75, 3840, 2160, &std::fs::read(&thm).unwrap());
    std::fs::write(root.join("MVI_8486.MP4"), mp4).unwrap();

    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &root, "DRIVE-1", VolumeKind::Removable);
    let r = scan(&c, &cache, &root, &mut noop).unwrap();
    assert_eq!(r.new_photos, 1);
    assert!(r.failed.is_empty(), "{:?}", r.failed);

    let v = &c.photos(1, 0).unwrap()[0];
    assert!(v.meta.is_video());
    assert_eq!(v.meta.duration_ms, Some(75_000));
    assert_eq!((v.meta.width, v.meta.height), (Some(3840), Some(2160)));
    assert_eq!(v.meta.taken_at.as_deref(), Some("2026-05-04T07:12:00"));
    assert!(v.has_thumbnail);
}

#[test]
fn cancelled_scan_marks_nothing_missing() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("drive");
    jpeg(&root.join("a.jpg"), 64, 64, 1);
    let c = Catalog::open_in_memory().unwrap();
    let cache = ThumbCache::new(d.path().join("cache"));
    drive(&c, &root, "DRIVE-1", VolumeKind::Removable);
    scan(&c, &cache, &root, &mut noop).unwrap();

    std::fs::remove_file(root.join("a.jpg")).unwrap();
    jpeg(&root.join("b.jpg"), 64, 64, 2);
    let stop = std::sync::atomic::AtomicBool::new(true);
    let r = scan_cancellable(&c, &cache, &root, &mut noop, &stop).unwrap();
    assert!(r.cancelled);
    assert_eq!((r.new_photos, r.missing), (0, 0));
    assert_eq!(c.stats().unwrap().files, 1); // a.jpg not flagged by an incomplete scan
}
