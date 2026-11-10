//! Reading what a photo knows about itself: capture time, camera, lens, GPS, size.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use exif::{In, Reader, Tag, Value};
use zd_catalog::PhotoMeta;

/// File types we catalogue, with their MIME type.
pub fn media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "tif" | "tiff" => "image/tiff",
        "heic" | "heif" => "image/heic",
        "cr3" => "image/x-canon-cr3",
        "cr2" => "image/x-canon-cr2",
        "dng" => "image/x-adobe-dng",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        _ => return None,
    })
}

pub fn is_raw(mime: &str) -> bool {
    mime.starts_with("image/x-")
}

/// Read metadata. Never fails: missing fields stay `None`, and capture time falls
/// back to the file's modification time.
pub fn read_meta(path: &Path, mime: &str) -> PhotoMeta {
    read_meta_impl(path, mime, None)
}

/// [`read_meta`] for plain images (JPEG, PNG, WebP, TIFF) using bytes already in memory
/// (the start of the file is enough: EXIF and the image header live there), so nothing
/// is re-read from a slow network share.
pub fn read_meta_from(path: &Path, mime: &str, bytes: &[u8]) -> PhotoMeta {
    read_meta_impl(path, mime, Some(bytes))
}

fn read_meta_impl(path: &Path, mime: &str, bytes: Option<&[u8]>) -> PhotoMeta {
    let mut meta = PhotoMeta { mime: mime.to_string(), orientation: 1, ..Default::default() };

    let video = mime.starts_with("video/").then(|| crate::video::probe(path).unwrap_or_default());
    let exif = if let Some(v) = &video {
        canon_exif(&v.meta_bytes)
    } else if mime == "image/x-canon-cr3" {
        read_head(path, 512 * 1024).map(|b| canon_exif(&b)).unwrap_or_default()
    } else if let Some(b) = bytes {
        Reader::new().read_from_container(&mut std::io::Cursor::new(b)).ok().into_iter().collect()
    } else {
        File::open(path)
            .ok()
            .and_then(|f| Reader::new().read_from_container(&mut BufReader::new(f)).ok())
            .map(|e| vec![e])
            .unwrap_or_default()
    };

    let field = |tag: Tag| exif.iter().find_map(|e| e.get_field(tag, In::PRIMARY));
    let text = |tag: Tag| -> Option<String> {
        match &field(tag)?.value {
            Value::Ascii(parts) => parts
                .first()
                .map(|b| {
                    String::from_utf8_lossy(b)
                        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
                        .to_string()
                })
                .filter(|s| !s.is_empty()),
            _ => None,
        }
    };
    let uint = |tag: Tag| field(tag).and_then(|f| f.value.get_uint(0));

    meta.taken_at = text(Tag::DateTimeOriginal)
        .or_else(|| text(Tag::DateTimeDigitized))
        .or_else(|| text(Tag::DateTime))
        .and_then(|s| exif_date(&s))
        .or_else(|| video.as_ref().and_then(|v| v.created.clone()))
        // With bytes in hand the caller fills this in from metadata it already has, rather
        // than another round trip to a network share.
        .or_else(|| if bytes.is_some() { None } else { file_time(path) });
    meta.camera_make = text(Tag::Make);
    meta.camera_model = text(Tag::Model);
    meta.lens = text(Tag::LensModel);
    meta.orientation = uint(Tag::Orientation).filter(|o| (1..=8).contains(o)).unwrap_or(1) as u16;
    meta.width = uint(Tag::PixelXDimension).or_else(|| uint(Tag::ImageWidth));
    meta.height = uint(Tag::PixelYDimension).or_else(|| uint(Tag::ImageLength));
    if let Some(v) = &video {
        // The container's frame size and length are the truth for videos.
        meta.width = v.width.or(meta.width);
        meta.height = v.height.or(meta.height);
        meta.duration_ms = v.duration_ms;
    } else if !is_raw(mime) {
        // The image header is the truth for dimensions; EXIF can be stale after edits.
        let dims = match bytes {
            Some(b) => image::ImageReader::new(std::io::Cursor::new(b))
                .with_guessed_format()
                .map_err(image::ImageError::IoError)
                .and_then(|r| r.into_dimensions()),
            None => image::image_dimensions(path),
        };
        if let Ok((w, h)) = dims {
            meta.width = Some(w);
            meta.height = Some(h);
        }
    }

    let coord = |tag: Tag, reft: Tag, neg: &str| -> Option<f64> {
        let Value::Rational(r) = &field(tag)?.value else { return None };
        if r.len() < 3 || r.iter().any(|x| x.denom == 0) {
            return None;
        }
        let v = r[0].to_f64() + r[1].to_f64() / 60.0 + r[2].to_f64() / 3600.0;
        Some(if text(reft).is_some_and(|s| s.eq_ignore_ascii_case(neg)) { -v } else { v })
    };
    meta.gps_lat = coord(Tag::GPSLatitude, Tag::GPSLatitudeRef, "S");
    meta.gps_lon = coord(Tag::GPSLongitude, Tag::GPSLongitudeRef, "W");
    if meta.gps_lat == Some(0.0) && meta.gps_lon == Some(0.0) {
        meta.gps_lat = None; // cameras without a fix write 0,0
        meta.gps_lon = None;
    }
    meta
}

/// `2026:05:04 07:12:00` -> `2026-05-04T07:12:00`
pub fn exif_date(s: &str) -> Option<String> {
    let s = s.trim();
    let (d, t) = s.split_once(' ')?;
    let parts: Vec<&str> = d.split(':').collect();
    if parts.len() != 3 || parts[0] == "0000" || !parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let t = t.get(..8).unwrap_or(t);
    Some(format!("{}-{}-{}T{}", parts[0], parts[1], parts[2], t))
}

fn file_time(path: &Path) -> Option<String> {
    mtime_string(&std::fs::metadata(path).ok()?)
}

/// Unix seconds as `YYYY-MM-DDTHH:MM:SS` local time.
pub fn unix_to_local_string(secs: i64) -> Option<String> {
    let t = chrono::DateTime::from_timestamp(secs, 0)?.with_timezone(&chrono::Local);
    Some(t.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// A file's modification time as `YYYY-MM-DDTHH:MM:SS` local time (capture-date fallback).
pub fn mtime_string(md: &std::fs::Metadata) -> Option<String> {
    let local: chrono::DateTime<chrono::Local> = md.modified().ok()?.into();
    Some(local.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// Canon CR3 is an ISO media container; its EXIF lives in `CMT1` (main IFD: make, model)
/// and `CMT2` (Exif IFD: dates, lens) boxes, each a small TIFF.
fn canon_exif(bytes: &[u8]) -> Vec<exif::Exif> {
    let mut out = Vec::new();
    for name in [b"CMT1", b"CMT2", b"CMT4"] {
        if let Some(tiff) = find_box(bytes, name) {
            if let Ok(e) = Reader::new().read_raw(tiff.to_vec()) {
                out.push(e);
            }
        }
    }
    out
}

pub(crate) fn read_head(path: &Path, max: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::with_capacity(max);
    File::open(path)?.take(max as u64).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Locate an ISO-BMFF box by its 4-byte type and return its payload.
fn find_box<'a>(bytes: &'a [u8], name: &[u8; 4]) -> Option<&'a [u8]> {
    let pos = bytes.windows(4).position(|w| w == name)?;
    let start = pos.checked_sub(4)?;
    let size = u32::from_be_bytes(bytes[start..pos].try_into().ok()?) as usize;
    let end = start.checked_add(size)?.min(bytes.len());
    (end > pos + 4).then(|| &bytes[pos + 4..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(exif_date("2026:05:04 07:12:00").as_deref(), Some("2026-05-04T07:12:00"));
        assert_eq!(exif_date("0000:00:00 00:00:00"), None);
        assert_eq!(exif_date("garbage"), None);
    }

    #[test]
    fn media_types() {
        assert_eq!(media_type(Path::new("IMG_0001.CR3")), Some("image/x-canon-cr3"));
        assert_eq!(media_type(Path::new("a.JPG")), Some("image/jpeg"));
        assert_eq!(media_type(Path::new("notes.txt")), None);
        assert_eq!(media_type(Path::new("noext")), None);
    }

    #[test]
    fn finds_boxes() {
        let mut b = vec![0u8; 8];
        b.extend_from_slice(&12u32.to_be_bytes());
        b.extend_from_slice(b"CMT1");
        b.extend_from_slice(b"TIFF");
        b.extend_from_slice(b"rest");
        assert_eq!(find_box(&b, b"CMT1"), Some(&b"TIFF"[..]));
        assert_eq!(find_box(&b, b"CMT2"), None);
    }
}
