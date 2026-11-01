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
        // libheif already applies the HEIC file's own rotation; don't rotate twice.
        let img = if mime == "image/heic" { img } else { orient(img, orientation) };
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
        return crate::video::poster(src);
    }
    if is_raw(mime) {
        return embedded_jpeg(src).ok_or_else(|| "no embedded preview in RAW file".to_string());
    }
    if mime == "image/heic" {
        return decode_heic(src);
    }
    if mime == "image/jpeg" {
        if let Some(img) = jpeg_scaled(src, Size::Preview.max_edge()) {
            return Ok(img);
        }
    }
    image::ImageReader::open(src)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| e.to_string())?
        .decode()
        .map_err(|e| e.to_string())
}

/// HEIC (Pixel, iPhone) through libheif, already rotated/cropped as the file specifies.
#[cfg(feature = "heic")]
fn decode_heic(src: &Path) -> Result<DynamicImage, String> {
    use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
    let path = src.to_str().ok_or("path is not valid UTF-8")?;
    let ctx = HeifContext::read_from_file(path).map_err(|e| e.to_string())?;
    let handle = ctx.primary_image_handle().map_err(|e| e.to_string())?;
    let img =
        LibHeif::new().decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None).map_err(|e| e.to_string())?;
    let plane = img.planes().interleaved.ok_or("no RGB data")?;
    let (w, h, stride) = (plane.width, plane.height, plane.stride);
    let row = w as usize * 3;
    let mut rgb = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        rgb.extend_from_slice(&plane.data[y * stride..y * stride + row]);
    }
    image::RgbImage::from_raw(w, h, rgb)
        .map(DynamicImage::ImageRgb8)
        .ok_or_else(|| "bad HEIC image size".into())
}

#[cfg(not(feature = "heic"))]
fn decode_heic(_src: &Path) -> Result<DynamicImage, String> {
    Err("this build has no HEIC support (built without the `heic` feature)".into())
}

/// Decode a JPEG at 1/2, 1/4 or 1/8 size directly (the DCT can skip detail), choosing the
/// smallest scale whose longest edge is still at least `max_edge`. For a 32 MP R7 photo
/// that's a quarter of the pixels and several times faster than a full decode.
fn jpeg_scaled(src: &Path, max_edge: u32) -> Option<DynamicImage> {
    let f = std::fs::File::open(src).ok()?;
    let mut d = jpeg_decoder::Decoder::new(std::io::BufReader::new(f));
    d.read_info().ok()?;
    let info = d.info()?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    let (rw, rh) = if w >= h {
        (max_edge.min(w), (u64::from(max_edge.min(w)) * u64::from(h)).div_ceil(u64::from(w)) as u32)
    } else {
        ((u64::from(max_edge.min(h)) * u64::from(w)).div_ceil(u64::from(h)) as u32, max_edge.min(h))
    };
    d.scale(rw.min(u32::from(u16::MAX)) as u16, rh.min(u32::from(u16::MAX)) as u16).ok()?;
    let pixels = d.decode().ok()?;
    let info = d.info()?;
    let (w, h) = (u32::from(info.width), u32::from(info.height));
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => {
            image::RgbImage::from_raw(w, h, pixels).map(DynamicImage::ImageRgb8)
        }
        jpeg_decoder::PixelFormat::L8 => {
            image::GrayImage::from_raw(w, h, pixels).map(DynamicImage::ImageLuma8)
        }
        _ => None, // CMYK and 16-bit: let the general decoder handle it
    }
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
        // Area averaging: about twice as fast as a triangle filter, and just as good for shrinking.
        img.thumbnail(max, max)
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
    fn big_jpegs_decode_scaled_but_not_below_preview_size() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("r7.jpg");
        jpeg(&src, 6960, 4640);
        let img = jpeg_scaled(&src, 2560).unwrap();
        assert_eq!((img.width(), img.height()), (3480, 2320)); // half size: still >= 2560 wide
        let small = d.path().join("s.jpg");
        jpeg(&small, 800, 600);
        let img = jpeg_scaled(&small, 2560).unwrap();
        assert_eq!((img.width(), img.height()), (800, 600));
    }

    #[cfg(feature = "heic")]
    #[test]
    fn heic_thumbnails() {
        // A real 640x480 HEIC made with libheif's heif-enc (checked in as a fixture).
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/small-heic.bin");
        let d = tempfile::tempdir().unwrap();
        let cache = ThumbCache::new(d.path().join("cache"));
        cache.generate(&src, "beef", "image/heic", 6).unwrap(); // EXIF orientation must be ignored
        let t = image::open(cache.path("beef", Size::Thumb)).unwrap();
        assert_eq!((t.width(), t.height()), (512, 384));
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
