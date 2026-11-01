# ZD-Photos

A photo organizer and editor for Linux and Windows, built for a hobbyist bird and nature photographer whose library is spread across external drives, a laptop and a home network share.

Design doc and roadmap: <https://claude.ai/artifact/9D3huBc87zkKkERRYMSVyi>

## Status: Phase 0 — Foundations

| Crate | What it does |
| --- | --- |
| `zd-catalog` | SQLite catalog, the single source of truth: drives, photos, file locations, tags, settings |
| `zd-volumes` | Recognises drives, network shares and SD cards across unplug/replug and drive-letter changes; finds the Home Library share |
| `zd-import` | Folder scanning, content fingerprints, EXIF (incl. Canon CR3), thumbnail/preview cache, SD card import with verified copies |
| `zd-cli` | `zdp`, a command-line tool for exercising the core |
| `zd-app` | `zd-photos`, the Qt 6 / QML desktop app. Phase 0 shell: dark library grid, resizable thumbnails (slider or Ctrl+wheel), drive status, "Add folder…", offline badges |

CI (GitHub Actions) lints, tests and builds everything on Linux and Windows, and produces a ready-to-run Windows folder (`zd-photos-windows` artifact) with Qt bundled.

## How photos are identified

- **Photos** are keyed by a BLAKE3 fingerprint of their content (size + first and last MiB), never by path. Moving, renaming or copying a photo keeps its tags and edits.
- **Drives** get a hidden `.zdphotos-volume` file at the root holding a UUID, so `E:` becoming `F:` doesn't matter. Internal disks and SD cards are never written to; they're recognised by filesystem UUID / volume serial.
- **Paths** are stored relative to the drive root with `/` separators, so the same catalog entry works on Linux and Windows.
- **Thumbnails (512 px) and previews (2560 px)** are cached next to the catalog, so unplugged photos stay browsable.

## Building

Requires Rust 1.82+ and, for the app, Qt 6 (base + declarative). Without Qt, build the core with `cargo test --workspace --exclude zd-app`.

```sh
# Fedora / Nobara: sudo dnf install qt6-qtbase-devel qt6-qtdeclarative-devel
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

## Videos

Videos from the R7 (and the Pixel) are catalogued like photos: MP4, M4V and MOV, with capture date, length, frame size and camera details. The grid shows a poster frame with a ▶ length badge. Poster frames come from `ffmpeg` when it's installed (Nobara ships it); otherwise from the small thumbnail Canon embeds in each video. Playback arrives in Phase 1.

## Known limits in Phase 0

- HEIC thumbnails wait for libheif (Phase 1).
- RAW files (CR3/CR2/DNG) use their embedded JPEG preview for thumbnails; full RAW development via LibRaw comes with the editor (Phase 2).
