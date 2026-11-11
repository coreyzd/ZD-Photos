# ZD-Photos

A photo organizer and editor for Linux and Windows, built for a hobbyist bird and nature photographer whose library is spread across external drives, a laptop and a home network share.

Design doc and roadmap: <https://claude.ai/artifact/9D3huBc87zkKkERRYMSVyi>

## Status: Phase 1 — Library (in progress)

Phase 0 (foundations) is complete and verified on her Windows laptop. Phase 1 so far: HEIC photos (libheif), background scanning with phase progress and Stop, live grid updates during scans, placeholder tiles naming any file that couldn't be previewed (`zdp thumbs` explains why and retries), a drives sidebar (photo counts, filter by drive, rename by double-click or right-click), cloud drives (Google Drive, OneDrive, Dropbox) recognised and never written to, roughly 2× faster thumbnails for large JPEGs, a shared thumbnail cache on the Home Library, and a full-screen viewer with video playback.

| Crate | What it does |
| --- | --- |
| `zd-catalog` | SQLite catalog, the single source of truth: drives, photos, file locations, tags, settings |
| `zd-volumes` | Recognises drives, network shares and SD cards across unplug/replug and drive-letter changes; finds the Home Library share |
| `zd-import` | Folder scanning, content fingerprints, EXIF (incl. Canon CR3), thumbnail/preview cache, SD card import with verified copies |
| `zd-cli` | `zdp`, a command-line tool for exercising the core |
| `zd-app` | `zd-photos`, the Qt 6 / QML desktop app: drives sidebar, resizable thumbnail grid (slider or Ctrl+wheel), background scanning, video and offline badges, full-screen viewer. For testing: `zd-photos --scan <folder>` starts a scan on launch, `zd-photos --view 0 [--info]` opens the viewer on the first photo |

CI (GitHub Actions) lints, tests and builds everything on Linux and Windows, and produces a ready-to-run Windows folder (`zd-photos-windows` artifact) with Qt bundled.

## How photos are identified

- **Photos** are keyed by a BLAKE3 fingerprint of their content (size + first and last MiB), never by path. Moving, renaming or copying a photo keeps its tags and edits.
- **Drives** get a hidden `.zdphotos-volume` file at the root holding a UUID, so `E:` becoming `F:` doesn't matter. Internal disks and SD cards are never written to; they're recognised by filesystem UUID / volume serial.
- **Paths** are stored relative to the drive root with `/` separators, so the same catalog entry works on Linux and Windows.
- **Thumbnails (512 px) and previews (2560 px)** are cached next to the catalog, so unplugged photos stay browsable.

## Building

Requires Rust 1.82+, libheif 1.17+ (for Pixel/iPhone HEIC photos) and, for the app, Qt 6.5+ (base + declarative; multimedia for video playback, which is only needed at run time; without it, videos show their poster frame and a note). Without Qt, build the core with `cargo test --workspace --exclude zd-app`; without libheif, add `--no-default-features` for `zd-import` (HEIC files then show placeholder tiles).

```sh
# Fedora / Nobara: sudo dnf install qt6-qtbase-devel qt6-qtdeclarative-devel qt6-qtmultimedia libheif-devel
#   HEIC photos are HEVC-compressed; Fedora's own libheif can't decode HEVC, the RPM Fusion
#   plugin can: sudo dnf install libheif-freeworld
cargo test --workspace
cargo run -p zd-app            # the app
cargo run -p zd-cli -- --help  # the command-line tool
```

The app and `zdp` share one catalog, so photos added with either show up in both. If cxx-qt can't find Qt, set `QMAKE` to your `qmake6` (e.g. `QMAKE=/usr/bin/qmake6`).

## Trying it with `zdp`

```sh
# Use a scratch data folder while testing (default is the per-user app data folder)
export ZDP_DATA=~/zdp-test

zdp drives                                     # detect drives, shares, SD cards
zdp home smb://nobara-zd.local/Photos/         # set the Home Library share
zdp scan "/run/media/$USER/My Passport/Photos" # catalogue a folder
zdp photos                                     # list (✓ = backed up to Home Library)
zdp info 12                                    # metadata, locations, tags
zdp tag 12 "Birds/Great Blue Heron"
zdp card --to ~/Pictures/R7 --dry-run          # what's new on the SD card?
zdp card --to ~/Pictures/R7                    # copy + verify + catalogue
zdp stats
```

How the Home Library is found:

- **On the computer serving the share** (nobara-zd): from Samba's config, i.e. the `path =` of the `[Photos]` section in `/etc/samba/smb.conf`, or the usershare Dolphin's *Share* tab created. If that can't be read, point at the exported folder yourself: `zdp home smb://nobara-zd.local/Photos/ --local /path/to/Photos`.
- **Other Linux machines:** when the share is mounted as a real folder: a `cifs` entry in fstab, GNOME/GVfs, or KDE with `kio-fuse` installed. Dolphin's own `smb://` browsing isn't a mount other programs can use.
- **Windows:** through a mapped drive, or directly at `\\nobara-zd.local\Photos`; no mapping needed.

Either way it's the same Home Library: the hidden marker file in the share's top folder ties them together.

### The Home Library's shared cache

Making a thumbnail means reading the whole photo, which over Wi-Fi is slow (~10 MB per R7 JPEG). So the Home Library carries a shared cache in a hidden `.zdphotos` folder at the share's root: thumbnails, previews, and `index.jsonl` (each file's path, size, date, fingerprint and metadata). Samba hides dot-folders from Windows by default.

- **nobara-zd** (the computer serving the share) fills it whenever it scans the Home Library, from its local disk.
- **Every other computer** looks each file up in the index by path, size and date. On a match it copies the ~50 KB thumbnail and never opens the photo. Anything new or edited is read normally and added to the cache for next time.

Keep it current automatically on nobara-zd with the systemd timer in `packaging/linux/`:

```sh
cargo install --path crates/zd-cli            # installs zdp into ~/.cargo/bin
mkdir -p ~/.config/systemd/user
cp packaging/linux/zdphotos-share.* ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now zdphotos-share.timer
systemctl --user start zdphotos-share.service  # first run now (watch: journalctl --user -fu zdphotos-share)
```

Or by hand: `zdp scan-home`.

## The viewer

Click (or tap) a photo to open it over the whole window.

| | |
| --- | --- |
| Next / previous | → ← keys, Page Up/Down, the arrows at the sides, or swipe on the touchscreen |
| First / last | Home / End |
| Zoom | Mouse wheel, pinch, + / −; double-click or double-tap for 100%, again to fit; 0 fits |
| Move around when zoomed | Drag |
| Info panel (date, camera, lens, size, location, tags, which drives have it) | I, or the ⓘ button |
| Full screen | F or F11 |
| Play / pause a video | Space, or click it |
| Back to the grid | Esc or Backspace |

It shows the grid thumbnail instantly, then the sharp 2560 px preview: from the local cache, else the Home Library's shared cache, else made from the original in the background (the next and previous photos are fetched ahead). Zooming in past what the preview holds loads the original file, when its drive is plugged in, so feathers can be checked at full resolution. Photos on unplugged drives still open at preview size.

## Videos

Videos from the R7 (and the Pixel) are catalogued like photos: MP4, M4V and MOV, with capture date, length, frame size and camera details. The grid shows a poster frame with a ▶ length badge. Poster frames come from `ffmpeg` when it's installed (Nobara ships it); otherwise from the small thumbnail Canon embeds in each video. The viewer plays them (Qt Multimedia) with play/pause, a seek bar and mute.

## Known limits

- RAW files (CR3/CR2/DNG) use their embedded JPEG preview for thumbnails; full RAW development via LibRaw comes with the editor (Phase 2).
- Zooming past the 2560 px preview uses the original for JPEG and PNG only; HEIC and RAW photos stay at preview resolution in the viewer for now.
