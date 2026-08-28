//! Thumbnails on timeline clips.
//!
//! Seeing the picture on the clip is how you find a shot without scrubbing
//! for it — the visual half of what waveforms do for sound.
//!
//! The trick that makes this cheap: instead of extracting N images per clip
//! and juggling N textures, one ffmpeg call renders the whole source into a
//! single **tiled sheet** (`fps=…,scale,tile=CxR`), which becomes one GPU
//! texture. Drawing a thumbnail is then just a sub-rectangle of that texture,
//! so a timeline full of clips costs one texture per source file and no
//! per-frame work at all.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};

/// Thumbnail cell size. Small enough that a whole sheet stays well inside any
/// GPU's texture limit, big enough to recognise a shot.
const CELL_W: u32 = 160;
const CELL_H: u32 = 90;
/// At most this many cells per source: a 12×10 sheet is 1920×900, which is
/// nothing, and 120 samples is finer than any timeline can show.
const COLS: u32 = 12;
const ROWS: u32 = 10;
const MAX_CELLS: u32 = COLS * ROWS;

/// Where each frame sits on the sheet. Kept apart from the texture so the
/// mapping — the part that can actually be wrong — is testable on its own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// How many cells actually hold a frame.
    pub count: u32,
    /// Seconds of source between one cell and the next.
    pub interval: f64,
}

pub struct Sheet {
    pub tex: egui::TextureHandle,
    pub layout: Layout,
}

impl Sheet {
    pub fn uv_at(&self, t: f64) -> Option<egui::Rect> {
        self.layout.uv_at(t)
    }
}

impl Layout {
    /// The sub-rectangle (in 0..1 texture coordinates) showing source time
    /// `t`, or None if this sheet has nothing for it.
    pub fn uv_at(&self, t: f64) -> Option<egui::Rect> {
        if self.count == 0 || self.interval <= 0.0 {
            return None;
        }
        let i = ((t / self.interval).floor() as i64).clamp(0, self.count as i64 - 1) as u32;
        let (col, row) = (i % COLS, i / COLS);
        let (w, h) = (1.0 / COLS as f32, 1.0 / ROWS as f32);
        Some(egui::Rect::from_min_size(
            egui::pos2(col as f32 * w, row as f32 * h),
            egui::vec2(w, h),
        ))
    }
}

/// The pixels a worker produced, before they become a texture.
struct Baked {
    source: String,
    image: Option<(Vec<u8>, u32, u32, u32, f64)>, // rgba, w, h, count, interval
}

/// Render one tiled contact sheet for `source`. Blocking; run on a worker.
fn bake(source: &str, duration: f64) -> Option<(Vec<u8>, u32, u32, u32, f64)> {
    if duration <= 0.0 {
        return None;
    }
    // One cell every `interval` seconds, never more than the sheet holds.
    let count = (duration.ceil() as u32).clamp(1, MAX_CELLS);
    let interval = duration / count as f64;
    let rate = if interval > 0.0 { 1.0 / interval } else { 1.0 };

    let vf = format!(
        "fps={rate:.6},scale={CELL_W}:{CELL_H}:force_original_aspect_ratio=increase,\
         crop={CELL_W}:{CELL_H},tile={COLS}x{ROWS}"
    );
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", source, "-vf", &vf,
            "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-",
        ])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let img = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    let (w, h) = (img.width(), img.height());
    Some((img.into_raw(), w, h, count, interval))
}

/// Contact sheets per source, baked in the background.
#[derive(Default)]
pub struct Cache {
    ready: HashMap<String, Sheet>,
    /// Sources that produced nothing (audio-only, unreadable) — remembered so
    /// we don't respawn a worker for them every frame.
    barren: HashMap<String, ()>,
    pending: HashMap<String, ()>,
    channel: Option<(Sender<Baked>, Receiver<Baked>)>,
}

impl Cache {
    /// The sheet for `source`, starting a bake if this is the first ask.
    /// Returns None until it's ready; the timeline just draws plain until then.
    pub fn get(&mut self, ctx: &egui::Context, source: &str, duration: f64) -> Option<&Sheet> {
        self.drain(ctx);
        if self.ready.contains_key(source) {
            return self.ready.get(source);
        }
        if self.barren.contains_key(source) || self.pending.contains_key(source) {
            return None;
        }
        let (tx, _) = self.channel.get_or_insert_with(mpsc::channel);
        let (tx, src) = (tx.clone(), source.to_string());
        self.pending.insert(source.to_string(), ());
        std::thread::spawn(move || {
            let image = bake(&src, duration);
            let _ = tx.send(Baked { source: src, image });
        });
        None
    }

    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }

    fn drain(&mut self, ctx: &egui::Context) {
        let Some((_, rx)) = &self.channel else { return };
        let mut done = Vec::new();
        while let Ok(b) = rx.try_recv() {
            done.push(b);
        }
        for b in done {
            self.pending.remove(&b.source);
            match b.image {
                Some((rgba, w, h, count, interval)) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                    let tex = ctx.load_texture(
                        format!("thumbs:{}", b.source),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    self.ready.insert(b.source, Sheet { tex, layout: Layout { count, interval } });
                }
                None => {
                    self.barren.insert(b.source, ());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sheet has to map a source time to the right cell, because that
    /// mapping is the whole feature: the thumbnail under your cursor should
    /// be the frame that is actually there.
    #[test]
    fn source_time_lands_on_the_right_cell() {
        let sheet = Layout { count: 24, interval: 0.5 };
        let cell = 1.0 / COLS as f32;
        let row = 1.0 / ROWS as f32;

        // t=0 is the first cell, top-left.
        let uv = sheet.uv_at(0.0).unwrap();
        assert!((uv.min.x - 0.0).abs() < 1e-6 && (uv.min.y - 0.0).abs() < 1e-6);
        assert!((uv.width() - cell).abs() < 1e-6 && (uv.height() - row).abs() < 1e-6);

        // Cell 13 of a 12-wide grid wraps to row 1, column 1.
        let uv = sheet.uv_at(0.5 * 13.0).unwrap();
        assert!((uv.min.x - cell).abs() < 1e-6, "column wrong: {uv:?}");
        assert!((uv.min.y - row).abs() < 1e-6, "row wrong: {uv:?}");

        // Past the end clamps to the last real cell rather than reading a
        // blank part of the sheet.
        assert_eq!(sheet.uv_at(9999.0), sheet.uv_at(0.5 * 23.0));

        // Degenerate sheets answer nothing instead of dividing by zero.
        assert!(Layout { count: 0, interval: 0.5 }.uv_at(1.0).is_none());
        assert!(Layout { count: 10, interval: 0.0 }.uv_at(1.0).is_none());
    }

    /// The real thing: one ffmpeg call turns a video into a tiled sheet.
    #[test]
    fn a_sheet_is_baked_from_a_real_video() {
        let fixture = format!("{}/tests/fixture.mp4", env!("CARGO_MANIFEST_DIR"));
        let (rgba, w, h, count, interval) = bake(&fixture, 2.0).expect("bake a sheet");
        assert_eq!((w, h), (COLS * CELL_W, ROWS * CELL_H), "sheet is the tiled grid");
        assert_eq!(rgba.len(), (w * h * 4) as usize);
        assert!(count >= 1 && count <= MAX_CELLS);
        assert!((interval - 2.0 / count as f64).abs() < 1e-6);

        // The first cell must hold actual picture, not a blank tile.
        let mut lit = 0;
        for y in 0..CELL_H {
            for x in 0..CELL_W {
                let i = ((y * w + x) * 4) as usize;
                if rgba[i] as u32 + rgba[i + 1] as u32 + rgba[i + 2] as u32 > 40 {
                    lit += 1;
                }
            }
        }
        assert!(lit > 500, "the first thumbnail looks empty ({lit} lit pixels)");
    }

    #[test]
    fn a_file_with_no_video_bakes_nothing_rather_than_hanging() {
        let wav = std::env::temp_dir().join(format!("reel-thumb-{}.wav", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
                   &wav.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        assert!(bake(&wav.to_string_lossy(), 1.0).is_none());
        let _ = std::fs::remove_file(&wav);
        assert!(bake("/definitely/not/here.mp4", 5.0).is_none());
    }
}
