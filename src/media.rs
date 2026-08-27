//! One app, every medium. Reel opens video, audio and images through a single
//! `open` path; this module holds what they share — the kind, and the
//! instant-loading image document (video/audio live in `video::Player`).

use anyhow::{anyhow, Result};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

/// Extensions routed to the image loader (everything else goes to the player,
/// where mpv also happily handles animated gif/webp/avif).
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff", "ico", "qoi", "tga", "svg", "svgz"];

fn ext_of(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

pub fn is_image_path(path: &str) -> bool {
    IMAGE_EXTS.contains(&ext_of(path).as_str())
}

pub fn is_svg_path(path: &str) -> bool {
    matches!(ext_of(path).as_str(), "svg" | "svgz")
}

/// A decoded still image, RGBA8 — displayed through the same GPU texture path
/// as a video frame. Loading is synchronous and effectively instant.
pub struct ImageDoc {
    pub path: String,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl ImageDoc {
    pub fn open(path: &str) -> Result<Self> {
        if is_svg_path(path) {
            return Self::open_svg(path);
        }
        let img = image::open(path).map_err(|e| anyhow!("could not read image {path}: {e}"))?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self { path: path.to_string(), width, height, data: rgba.into_raw() })
    }

    /// Rasterize an SVG with resvg. Small intrinsic sizes are upscaled so the
    /// raster stays crisp (vector sources deserve at least ~2K pixels).
    fn open_svg(path: &str) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut opt = resvg::usvg::Options::default();
        opt.fontdb_mut().load_system_fonts();
        let tree = resvg::usvg::Tree::from_data(&bytes, &opt)
            .map_err(|e| anyhow!("could not parse SVG {path}: {e}"))?;
        let size = tree.size();
        if size.width() <= 0.0 || size.height() <= 0.0 {
            return Err(anyhow!("SVG {path} has no intrinsic size"));
        }
        let max_edge = size.width().max(size.height());
        let scale = if max_edge < 2048.0 { 2048.0 / max_edge } else { 1.0 };
        let (w, h) = (
            (size.width() * scale).round().max(1.0) as u32,
            (size.height() * scale).round().max(1.0) as u32,
        );
        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h)
            .ok_or_else(|| anyhow!("SVG raster target {w}×{h} is not allocatable"))?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::from_scale(scale, scale),
            &mut pixmap.as_mut(),
        );
        // tiny-skia stores premultiplied RGBA; the texture path wants straight.
        let mut data = pixmap.take();
        for px in data.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                px[0] = (px[0] as u32 * 255 / a).min(255) as u8;
                px[1] = (px[1] as u32 * 255 / a).min(255) as u8;
                px[2] = (px[2] as u32 * 255 / a).min(255) as u8;
            }
        }
        log::info!("rasterized {path} at {w}×{h} (scale {scale:.2})");
        Ok(Self { path: path.to_string(), width: w, height: h, data })
    }

    /// Write the decoded RGBA to a temporary PNG — the ffmpeg-facing stand-in
    /// for sources ffmpeg can't read (SVG).
    pub fn write_temp_png(&self) -> Result<std::path::PathBuf> {
        let p = std::env::temp_dir().join(format!("reel-raster-{}.png", std::process::id()));
        let img = image::RgbaImage::from_raw(self.width, self.height, self.data.clone())
            .ok_or_else(|| anyhow!("image buffer size mismatch"))?;
        img.save(&p)?;
        Ok(p)
    }

    /// Downscale (aspect-kept) so no edge exceeds `max_dim` — GPUs cap texture
    /// size, and an ultrawide screenshot can be bigger than that cap. The
    /// original file is untouched; only the displayed copy shrinks.
    pub fn clamp_to(&mut self, max_dim: u32) {
        if self.width <= max_dim && self.height <= max_dim {
            return;
        }
        let scale = (max_dim as f64 / self.width as f64).min(max_dim as f64 / self.height as f64);
        let (nw, nh) = (
            ((self.width as f64 * scale) as u32).max(1),
            ((self.height as f64 * scale) as u32).max(1),
        );
        let src = image::RgbaImage::from_raw(self.width, self.height, std::mem::take(&mut self.data))
            .expect("image buffer matches its dimensions");
        let resized = image::imageops::resize(&src, nw, nh, image::imageops::FilterType::CatmullRom);
        log::info!("image {}×{} exceeds GPU limit {max_dim}; displaying at {nw}×{nh}", self.width, self.height);
        self.width = nw;
        self.height = nh;
        self.data = resized.into_raw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_extensions_are_routed() {
        assert!(is_image_path("/x/photo.PNG"));
        assert!(is_image_path("shot.jpeg"));
        assert!(is_image_path("logo.svg"));
        assert!(!is_image_path("movie.mp4"));
        assert!(!is_image_path("song.flac"));
        assert!(!is_image_path("noext"));
        assert!(is_svg_path("a.SVG"));
        assert!(!is_svg_path("a.png"));
    }

    #[test]
    fn rasterizes_svg_upscaled_for_crispness() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("reel-svg-test-{}.svg", std::process::id()));
        std::fs::write(
            &p,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50">
                 <rect width="100" height="50" fill="#22d3ee"/></svg>"##,
        )
        .expect("write test svg");
        let doc = ImageDoc::open(&p.to_string_lossy()).expect("rasterize svg");
        // 100×50 intrinsic → upscaled so the long edge is ~2048.
        assert_eq!((doc.width, doc.height), (2048, 1024));
        assert_eq!(doc.data.len(), (2048 * 1024 * 4) as usize);
        // Center pixel is the cyan fill, straight alpha.
        let mid = ((512 * 2048 + 1024) * 4) as usize;
        assert_eq!(&doc.data[mid..mid + 4], &[0x22, 0xd3, 0xee, 0xff]);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn loads_a_generated_png() {
        let dir = std::env::temp_dir();
        let p = dir.join(format!("reel-img-test-{}.png", std::process::id()));
        image::RgbaImage::from_pixel(64, 48, image::Rgba([10, 200, 30, 255]))
            .save(&p)
            .expect("write test png");
        let doc = ImageDoc::open(&p.to_string_lossy()).expect("open png");
        assert_eq!((doc.width, doc.height), (64, 48));
        assert_eq!(doc.data.len(), 64 * 48 * 4);
        assert_eq!(&doc.data[0..4], &[10, 200, 30, 255]);
        let _ = std::fs::remove_file(&p);
    }
}
