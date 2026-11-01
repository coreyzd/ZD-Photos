//! Reading MP4 / MOV videos (Canon R7, Pixel) without decoding them.
//!
//! The container (ISO-BMFF) tells us duration, frame size and creation time. Canon
//! cameras also embed a `uuid` box holding the same EXIF blocks as a CR3 (`CMT1`..`CMT4`:
//! make, model, lens, capture time) and a small JPEG thumbnail (`CNTH`).
//!
//! Poster frames: if `ffmpeg` is on PATH we grab a real frame; otherwise we fall back to
//! the embedded thumbnail. Bundled frame extraction and playback come in Phase 1.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use image::DynamicImage;

/// Seconds between 1904-01-01 (QuickTime epoch) and 1970-01-01.
const QT_EPOCH_OFFSET: i64 = 2_082_844_800;
/// Never buffer more than this much metadata from one file.
const META_CAP: u64 = 32 * 1024 * 1024;

#[derive(Debug, Default)]
pub struct VideoInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    /// `YYYY-MM-DDTHH:MM:SS` from the container (cameras write their local clock here).
    pub created: Option<String>,
    /// Raw bytes of `moov` and top-level `uuid` boxes, for EXIF and thumbnail lookup.
    pub meta_bytes: Vec<u8>,
}

/// Walk the top-level boxes, keeping only the small metadata ones (skips `mdat`).
pub fn probe(path: &Path) -> std::io::Result<VideoInfo> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut info = VideoInfo::default();
    let mut pos = 0u64;
    let mut moov: Option<Vec<u8>> = None;

    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr)?;
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let kind = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let mut header_len = 8;
        if size == 1 {
            let mut big = [0u8; 8];
            f.read_exact(&mut big)?;
            size = u64::from_be_bytes(big);
            header_len = 16;
        } else if size == 0 {
            size = len - pos; // runs to end of file
        }
        if size < header_len {
            break; // corrupt
        }
        if (&kind == b"moov" || &kind == b"uuid") && size <= META_CAP {
            let mut body = vec![0u8; (size - header_len) as usize];
            f.read_exact(&mut body)?;
            if &kind == b"moov" {
                moov = Some(body.clone());
            }
            if info.meta_bytes.len() as u64 + body.len() as u64 <= META_CAP {
                info.meta_bytes.extend_from_slice(&body);
            }
        }
        pos = pos.saturating_add(size);
    }

    if let Some(moov) = moov {
        parse_moov(&moov, &mut info);
    }
    Ok(info)
}

/// Iterate the child boxes of a container's payload: (type, payload).
fn children(buf: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        if pos + 8 > buf.len() {
            return None;
        }
        let size = u32::from_be_bytes(buf[pos..pos + 4].try_into().ok()?) as usize;
        let kind: [u8; 4] = buf[pos + 4..pos + 8].try_into().ok()?;
        let (start, end) = match size {
            0 => (pos + 8, buf.len()),
            1 => {
                let big = u64::from_be_bytes(buf.get(pos + 8..pos + 16)?.try_into().ok()?) as usize;
                (pos + 16, pos.checked_add(big)?)
            }
            n if n >= 8 => (pos + 8, pos.checked_add(n)?),
            _ => return None,
        };
        if end > buf.len() || start > end {
            return None;
        }
        pos = end;
        Some((kind, &buf[start..end]))
    })
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn parse_moov(moov: &[u8], info: &mut VideoInfo) {
    for (kind, body) in children(moov) {
        match &kind {
            b"mvhd" => {
                let v1 = body.first() == Some(&1);
                let (created, timescale, duration) = if v1 {
                    (be64(body, 4), be32(body, 20), be64(body, 24))
                } else {
                    (be32(body, 4).map(u64::from), be32(body, 12), be32(body, 16).map(u64::from))
                };
                if let (Some(ts), Some(d)) = (timescale.filter(|t| *t > 0), duration) {
                    info.duration_ms = Some(d.saturating_mul(1000) / u64::from(ts));
                }
                info.created = created.filter(|c| *c > 0).and_then(qt_time);
            }
            b"trak" if info.width.is_none() => {
                if let Some((_, tkhd)) = children(body).find(|(k, _)| k == b"tkhd") {
                    // Width and height are the last 8 bytes, 16.16 fixed point.
                    let n = tkhd.len();
                    if n >= 8 {
                        let w = be32(tkhd, n - 8).unwrap_or(0) >> 16;
                        let h = be32(tkhd, n - 4).unwrap_or(0) >> 16;
                        if w > 0 && h > 0 {
                            info.width = Some(w);
                            info.height = Some(h);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn qt_time(secs: u64) -> Option<String> {
    let unix = i64::try_from(secs).ok()? - QT_EPOCH_OFFSET;
    let t = chrono::DateTime::from_timestamp(unix, 0)?;
    Some(t.naive_utc().format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// A poster frame for the grid and viewer.
pub fn poster(path: &Path) -> Option<DynamicImage> {
    ffmpeg_frame(path).or_else(|| {
        let info = probe(path).ok()?;
        crate::thumbs::largest_jpeg_in(&info.meta_bytes)
    })
}

/// One frame ~1 s in, via ffmpeg if installed.
fn ffmpeg_frame(path: &Path) -> Option<DynamicImage> {
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", "1", "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            "scale='min(2560,iw)':-2",
            "-f",
            "image2pipe",
            "-vcodec",
            "mjpeg",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    image::load_from_memory_with_format(&out.stdout, image::ImageFormat::Jpeg).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A minimal MP4: ftyp, a big mdat, then moov with mvhd + one video trak.
    pub fn fake_mp4(created_unix: i64, duration_s: u32, w: u32, h: u32, extra_uuid: &[u8]) -> Vec<u8> {
        let mut mvhd = vec![0u8; 100];
        mvhd[4..8].copy_from_slice(&((created_unix + QT_EPOCH_OFFSET) as u32).to_be_bytes());
        mvhd[12..16].copy_from_slice(&1000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&(duration_s * 1000).to_be_bytes());
        let mut tkhd = vec![0u8; 84];
        tkhd[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        let trak = bx(b"trak", &bx(b"tkhd", &tkhd));
        let moov = bx(b"moov", &[bx(b"mvhd", &mvhd), trak].concat());
        [bx(b"ftyp", b"mp42\0\0\0\0isom"), bx(b"uuid", extra_uuid), bx(b"mdat", &vec![0xAB; 3_000_000]), moov]
            .concat()
    }

    #[test]
    fn reads_container_metadata() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("MVI_0001.MP4");
        // 2026-05-04T07:12:00 UTC
        std::fs::write(&p, fake_mp4(1_777_878_720, 42, 3840, 2160, b"canon")).unwrap();
        let info = probe(&p).unwrap();
        assert_eq!((info.width, info.height), (Some(3840), Some(2160)));
        assert_eq!(info.duration_ms, Some(42_000));
        assert_eq!(info.created.as_deref(), Some("2026-05-04T07:12:00"));
        assert!(info.meta_bytes.len() < 1000, "mdat must not be buffered");
    }

    #[test]
    fn garbage_is_harmless() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.mp4");
        std::fs::write(&p, b"\x00\x00\x00\x04junkjunkjunk").unwrap();
        let info = probe(&p).unwrap();
        assert!(info.duration_ms.is_none());
    }
}
