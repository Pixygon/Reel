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
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "bmp", "webp", "tif", "tiff", "ico", "qoi", "tga"];

pub fn is_image_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .map(|e| {
            let e = e.to_string_lossy().to_lowercase();
            IMAGE_EXTS.contains(&e.as_str())
        })
        .unwrap_or(false)
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
        let img = image::open(path).map_err(|e| anyhow!("could not read image {path}: {e}"))?;
        let rgba = img.to_rgba8();
        let (width, height) = rgba.dimensions();
        Ok(Self { path: path.to_string(), width, height, data: rgba.into_raw() })
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
        assert!(!is_image_path("movie.mp4"));
        assert!(!is_image_path("song.flac"));
        assert!(!is_image_path("noext"));
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
