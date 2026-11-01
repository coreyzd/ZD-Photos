//! Thumbnail and preview cache. These live next to the catalog on the laptop, so
//! photos on an unplugged drive can still be browsed, viewed and even edited.

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageFormat};

use crate::metadata::{is_raw, read_head};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Size {
    /// Grid thumbnail, longest edge 512 px.
    Thumb,
    /// Full-screen viewer / editor proxy for offline photos, longest edge 2560 px.
    Preview,
}

impl Size {
    pub fn max_edge(self) -> u32 {
        match self {
            Size::Thumb => 512,
            Size::Preview => 2560,
        }
    }
    fn dir(self) -> &'static str {
        match self {
            Size::Thumb => "thumbs",
            Size::Preview => "previews",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThumbCache {
    root: PathBuf,
}

impl ThumbCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ThumbCache { root: root.into() }
    }

    pub fn path(&self, fingerprint: &str, size: Size) -> PathBuf {
        let shard = fingerprint.get(..2).unwrap_or("00");
        self.root.join(size.dir()).join(shard).join(format!("{fingerprint}.jpg"))
    }

    pub fn has(&self, fingerprint: &str) -> bool {
        self.path(fingerprint, Size::Thumb).is_file() && self.path(fingerprint, Size::Preview).is_file()
    }

    /// Decode once, write both sizes. RAW files use their embedded JPEG preview for now.
    pub fn generate(
        &self,
        src: &Path,
        fingerprint: &str,
        mime: &str,
        orientation: u16,
    ) -> Result<(), String> {
        let img = decode(src, mime)?;
        let img = orient(img, orientation);
        let preview = shrink(&img, Size::Preview.max_edge());
        self.write(&preview, &self.path(fingerprint, Size::Preview))?;
        let thumb = preview.thumbnail(Size::Thumb.max_edge(), Size::Thumb.max_edge());
        self.write(&thumb, &self.path(fingerprint, Size::Thumb))
    }

    fn write(&self, img: &DynamicImage, dest: &Path) -> Result<(), String> {
        let dir = dest.parent().ok_or("bad cache path")?;
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dest.with_extension(format!("tmp{}", std::process::id()));
        {
            let f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
            let enc = JpegEncoder::new_with_quality(BufWriter::new(f), 85);
            img.to_rgb8().write_with_encoder(enc).map_err(|e| e.to_string())?;
        }
        fs::rename(&tmp, dest).map_err(|e| e.to_string())
    }
}

fn decode(src: &Path, mime: &str) -> Result<DynamicImage, String> {
    if mime.starts_with("video/") {
        return crate::video::poster(src)
            .ok_or_else(|| "no poster frame (install ffmpeg for video frames)".into());
    }
    if is_raw(mime) {
        return embedded_jpeg(src).ok_or_else(|| "no embedded preview in RAW file".to_string());
    }
    if mime == "image/heic" {
        return Err("HEIC decoding arrives with libheif in Phase 1".into());
    }
    image::ImageReader::open(src)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

/// RAW files (CR3, CR2, DNG) carry JPEG previews. Pick the largest one that decodes.
/// Full RAW development (LibRaw) comes with the editor in Phase 2.
fn embedded_jpeg(src: &Path) -> Option<DynamicImage> {
    largest_jpeg_in(&read_head(src, 64 * 1024 * 1024).ok()?)
}

/// The largest JPEG that decodes from inside a byte buffer (RAW previews, Canon video `CNTH`).
pub(crate) fn largest_jpeg_in(bytes: &[u8]) -> Option<DynamicImage> {
    let mut best: Option<DynamicImage> = None;
    let mut tries = 0;
    let mut i = 0;
    while i + 3 < bytes.len() && tries < 24 {
        if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 && bytes[i + 2] == 0xFF {
            tries += 1;
            if let Ok(img) = image::load_from_memory_with_format(&bytes[i..], ImageFormat::Jpeg) {
                if best.as_ref().is_none_or(|b| img.width() > b.width()) {
                    best = Some(img);
                }
            }
        }
        i += 1;
    }
    best
}

fn shrink(img: &DynamicImage, max: u32) -> DynamicImage {
    if img.width().max(img.height()) <= max {
        img.clone()
    } else {
        img.resize(max, max, image::imageops::FilterType::Triangle)
    }
}

/// Apply EXIF orientation so cached images are always upright.
pub fn orient(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(path: &Path, w: u32, h: u32) {
        let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
        img.save(path).unwrap();
    }

    #[test]
    fn generates_both_sizes_upright() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.jpg");
        jpeg(&src, 3000, 2000);
        let cache = ThumbCache::new(d.path().join("cache"));
        cache.generate(&src, "abcdef", "image/jpeg", 6).unwrap();
        assert!(cache.has("abcdef"));
        let t = image::open(cache.path("abcdef", Size::Thumb)).unwrap();
        let p = image::open(cache.path("abcdef", Size::Preview)).unwrap();
        assert_eq!((t.width(), t.height()), (341, 512)); // rotated to portrait
        assert_eq!((p.width(), p.height()), (1707, 2560)); // longest edge capped at 2560
    }

    #[test]
    fn raw_uses_embedded_preview() {
        let d = tempfile::tempdir().unwrap();
        let small = d.path().join("s.jpg");
        let big = d.path().join("b.jpg");
        jpeg(&small, 160, 120);
        jpeg(&big, 1620, 1080);
        // Fake RAW: header junk, a small thumbnail, more junk, a bigger preview.
        let mut raw = b"....ftypcrx junk".to_vec();
        raw.extend(std::fs::read(&small).unwrap());
        raw.extend(vec![0u8; 1000]);
        raw.extend(std::fs::read(&big).unwrap());
        let src = d.path().join("IMG_0001.CR3");
        std::fs::write(&src, raw).unwrap();

        let cache = ThumbCache::new(d.path().join("cache"));
        cache.generate(&src, "ff00", "image/x-canon-cr3", 1).unwrap();
        let p = image::open(cache.path("ff00", Size::Preview)).unwrap();
        assert_eq!(p.width(), 1620);
    }
}
