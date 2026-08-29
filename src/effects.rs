//! Per-clip effects — and the one rule that makes them trustworthy: the
//! preview and the render must produce the same picture.
//!
//! That is enforced by defining each adjustment ONCE here, as plain maths on
//! sRGB-encoded values in 0..1, and then:
//!   * `video.wgsl` applies exactly this formula on the GPU for the preview
//!     (converting linear→sRGB→linear around it, since the frame texture is
//!     sRGB and sampling hands back linear values), and
//!   * `filters()` emits the ffmpeg chain that performs the same maths on
//!     the same encoding at render time.
//!
//! `apply_reference()` is the executable statement of that formula, and a
//! test drives real ffmpeg with these filters and compares its output pixels
//! against it — so drift between the two paths fails the build rather than
//! surprising someone after a two-hour export.

use serde::{Deserialize, Serialize};

/// Colour/exposure adjustments and fades for one clip. Defaults are identity,
/// and every field is `serde(default)` so older `.reel` documents keep loading.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effects {
    /// Linear gain on the encoded value. 1.0 = untouched.
    #[serde(default = "one")]
    pub exposure: f32,
    /// Contrast around mid-grey. 1.0 = untouched.
    #[serde(default = "one")]
    pub contrast: f32,
    /// 0 = greyscale, 1 = untouched, >1 = punchier.
    #[serde(default = "one")]
    pub saturation: f32,
    /// Seconds of fade from black at the clip's start.
    #[serde(default)]
    pub fade_in: f64,
    /// Seconds of fade to black at the clip's end.
    #[serde(default)]
    pub fade_out: f64,
    /// Reframing: zoom into the fitted frame (1.0 = whole frame).
    #[serde(default = "one")]
    pub zoom: f32,
    /// Where the zoomed window sits, -1..1 (0 = centred). Only meaningful
    /// when `zoom` > 1 — at 1.0 there is nothing to pan within.
    #[serde(default)]
    pub pan_x: f32,
    #[serde(default)]
    pub pan_y: f32,
    /// Chroma key: knock out this colour (sRGB 0..1) so what's underneath
    /// shows through. `None` = no keying. Applied on the GPU in both the
    /// preview and the frame-server render — the graph fallback cannot key.
    #[serde(default)]
    pub key_color: Option<[f32; 3]>,
    /// How far from the key colour still counts as background (0..1).
    #[serde(default = "default_key_similarity")]
    pub key_similarity: f32,
    /// Width of the soft edge beyond `similarity` (0..1).
    #[serde(default = "default_key_softness")]
    pub key_softness: f32,
    /// Index into the project's LUT table (`Project.luts`) — an index, not
    /// a path, so Effects stays `Copy` and identical LUTs are shared. The
    /// LUT applies (on encoded values) before the trims above: conform the
    /// look first, adjust after.
    #[serde(default)]
    pub lut: Option<u32>,
    /// A power window: the colour grade (LUT + trims) applies only inside
    /// this shape (or outside, inverted). Everything is frame fractions.
    #[serde(default)]
    pub mask: Option<Mask>,
}

/// The grade-limiting window. `w`/`h` are HALF-extents from the centre.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Mask {
    pub shape: MaskShape,
    pub cx: f32,
    pub cy: f32,
    pub w: f32,
    pub h: f32,
    /// Soft edge width, as a fraction of the frame.
    #[serde(default = "default_feather")]
    pub feather: f32,
    /// Grade OUTSIDE the shape instead of inside.
    #[serde(default)]
    pub invert: bool,
}

fn default_feather() -> f32 {
    0.05
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            shape: MaskShape::Ellipse,
            cx: 0.5,
            cy: 0.5,
            w: 0.25,
            h: 0.25,
            feather: default_feather(),
            invert: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskShape {
    Rect,
    Ellipse,
}

fn default_key_similarity() -> f32 {
    0.30
}

fn default_key_softness() -> f32 {
    0.10
}

fn one() -> f32 {
    1.0
}

impl Default for Effects {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            contrast: 1.0,
            saturation: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            key_color: None,
            key_similarity: default_key_similarity(),
            key_softness: default_key_softness(),
            lut: None,
            mask: None,
        }
    }
}

/// Rec.709 luma weights — the same ones the shader and ffmpeg's matrix use.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

impl Effects {
    pub fn is_identity(&self) -> bool {
        (self.exposure - 1.0).abs() < 1e-4
            && (self.contrast - 1.0).abs() < 1e-4
            && (self.saturation - 1.0).abs() < 1e-4
            && self.fade_in <= 0.0
            && self.fade_out <= 0.0
            && !self.has_reframe()
            && self.key_color.is_none()
            && self.lut.is_none()
            && self.mask.is_none()
    }

    pub fn has_reframe(&self) -> bool {
        self.zoom > 1.0001
    }

    /// The reframe filter, applied AFTER the frame has been fitted to the
    /// target: blow the frame up by `zoom`, then crop the window back to
    /// size, offset by the pan. `w`/`h` are the target frame.
    ///
    /// The preview shader samples with the same geometry:
    ///   uv_src = (uv - 0.5) / zoom + 0.5 + pan * (1 - 1/zoom) / 2
    /// (pan = ±1 puts the window exactly on an edge in both paths.)
    pub fn reframe_filter(&self, w: u32, h: u32) -> Option<String> {
        if !self.has_reframe() {
            return None;
        }
        let z = self.zoom.max(1.0);
        let (px, py) = (self.pan_x.clamp(-1.0, 1.0), self.pan_y.clamp(-1.0, 1.0));
        Some(format!(
            "scale=iw*{z:.6}:ih*{z:.6}:flags=lanczos,\
             crop={w}:{h}:(iw-ow)/2*(1+{px:.6}):(ih-oh)/2*(1+{py:.6})"
        ))
    }

    pub fn has_colour(&self) -> bool {
        (self.exposure - 1.0).abs() > 1e-4
            || (self.contrast - 1.0).abs() > 1e-4
            || (self.saturation - 1.0).abs() > 1e-4
    }

    /// THE formula. sRGB-encoded rgb in 0..1 → adjusted rgb in 0..1.
    /// Order matters and is mirrored exactly in video.wgsl and `filters()`:
    /// exposure (gain), then contrast about 0.5, then saturation about luma.
    /// Only the parity test calls this — it exists as the executable
    /// statement of what `video.wgsl` and `filters()` must both do.
    #[allow(dead_code)]
    pub fn apply_reference(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut c = [
            rgb[0] * self.exposure,
            rgb[1] * self.exposure,
            rgb[2] * self.exposure,
        ];
        c = [
            (c[0] - 0.5) * self.contrast + 0.5,
            (c[1] - 0.5) * self.contrast + 0.5,
            (c[2] - 0.5) * self.contrast + 0.5,
        ];
        let luma = c[0] * LUMA[0] + c[1] * LUMA[1] + c[2] * LUMA[2];
        c = [
            luma + (c[0] - luma) * self.saturation,
            luma + (c[1] - luma) * self.saturation,
            luma + (c[2] - luma) * self.saturation,
        ];
        [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0)]
    }

    /// The ffmpeg filters implementing the same maths, in the same order.
    /// `clip_len` scopes the fades to this segment.
    pub fn filters(&self, clip_len: f64) -> Vec<String> {
        let mut f = Vec::new();
        if (self.exposure - 1.0).abs() > 1e-4 || (self.contrast - 1.0).abs() > 1e-4 {
            // One LUT does gain and contrast together, on encoded values —
            // exactly `(v*exposure - 0.5)*contrast + 0.5` in 0..255 terms.
            let expr = |ch: &str| {
                format!(
                    "{ch}='clip((val/255*{e:.6}-0.5)*{c:.6}*255+127.5,0,255)'",
                    e = self.exposure,
                    c = self.contrast
                )
            };
            f.push(format!("lutrgb={}:{}:{}", expr("r"), expr("g"), expr("b")));
        }
        if (self.saturation - 1.0).abs() > 1e-4 {
            // Saturation as an exact 3×3 matrix about Rec.709 luma — the same
            // mix() the shader does, written out for colorchannelmixer.
            let s = self.saturation;
            let m = |i: usize, j: usize| {
                let base = LUMA[j] * (1.0 - s);
                if i == j { base + s } else { base }
            };
            f.push(format!(
                "colorchannelmixer=rr={:.6}:rg={:.6}:rb={:.6}:gr={:.6}:gg={:.6}:gb={:.6}:br={:.6}:bg={:.6}:bb={:.6}",
                m(0, 0), m(0, 1), m(0, 2),
                m(1, 0), m(1, 1), m(1, 2),
                m(2, 0), m(2, 1), m(2, 2)
            ));
        }
        if self.fade_in > 0.0 {
            f.push(format!("fade=t=in:st=0:d={:.4}", self.fade_in.min(clip_len)));
        }
        if self.fade_out > 0.0 {
            let d = self.fade_out.min(clip_len);
            f.push(format!("fade=t=out:st={:.4}:d={d:.4}", (clip_len - d).max(0.0)));
        }
        f
    }

    /// Fade multiplier at `t` seconds into a clip of `clip_len` — the preview's
    /// counterpart to the `fade` filters above.
    pub fn fade_alpha(&self, t: f64, clip_len: f64) -> f32 {
        let mut a: f64 = 1.0;
        if self.fade_in > 0.0 && t < self.fade_in {
            a = a.min((t / self.fade_in).clamp(0.0, 1.0));
        }
        if self.fade_out > 0.0 {
            let start = (clip_len - self.fade_out).max(0.0);
            if t > start {
                a = a.min((1.0 - (t - start) / self.fade_out).clamp(0.0, 1.0));
            }
        }
        a as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_pixel_through_ffmpeg(fx: &Effects, rgb: [u8; 3]) -> [u8; 3] {
        let dir = std::env::temp_dir();
        let out = dir.join(format!("reel-fx-{}-{}.png", std::process::id(), rgb[0]));
        let _ = std::fs::remove_file(&out);
        let filters = fx.filters(1.0).join(",");
        let source = format!("color=c=0x{:02X}{:02X}{:02X}:size=32x32:rate=1", rgb[0], rgb[1], rgb[2]);
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-f", "lavfi", "-i", &source,
                "-vf", &filters, "-frames:v", "1", &out.to_string_lossy(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "ffmpeg failed for filters: {filters}");
        let img = image::open(&out).expect("read filtered png").to_rgb8();
        let px = img.get_pixel(16, 16).0;
        let _ = std::fs::remove_file(&out);
        px
    }

    /// The contract: what the preview shader computes (mirrored by
    /// `apply_reference`) is what ffmpeg actually renders. If these drift,
    /// the editor is lying about the output.
    #[test]
    fn ffmpeg_matches_the_reference_formula() {
        let cases = [
            Effects { exposure: 1.25, ..Default::default() },
            Effects { contrast: 1.4, ..Default::default() },
            Effects { saturation: 0.0, ..Default::default() },
            Effects { saturation: 1.8, ..Default::default() },
            Effects { exposure: 0.85, contrast: 1.2, saturation: 1.3, ..Default::default() },
        ];
        let colours: [[u8; 3]; 4] = [[128, 64, 32], [200, 200, 200], [40, 90, 160], [255, 0, 0]];
        for fx in cases {
            for rgb in colours {
                let expected_f = fx.apply_reference([
                    rgb[0] as f32 / 255.0,
                    rgb[1] as f32 / 255.0,
                    rgb[2] as f32 / 255.0,
                ]);
                let expected = [
                    (expected_f[0] * 255.0).round() as i32,
                    (expected_f[1] * 255.0).round() as i32,
                    (expected_f[2] * 255.0).round() as i32,
                ];
                let got = probe_pixel_through_ffmpeg(&fx, rgb);
                for c in 0..3 {
                    let delta = (got[c] as i32 - expected[c]).abs();
                    assert!(
                        delta <= 3,
                        "channel {c} drifted for {fx:?} on {rgb:?}: ffmpeg {} vs reference {} (Δ{delta})",
                        got[c], expected[c]
                    );
                }
            }
        }
    }

    /// Reframe geometry: at 2× zoom panned fully right, the visible window is
    /// the source's right half — and the shader's UV maths agrees with the
    /// crop ffmpeg performs.
    #[test]
    fn reframe_pans_to_the_expected_window() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-reframe-src-{}.png", std::process::id()));
        let out = dir.join(format!("reel-reframe-out-{}.png", std::process::id()));
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
        // Left half red, right half blue.
        let mut img = image::RgbImage::new(200, 100);
        for (x, _y, px) in img.enumerate_pixels_mut() {
            *px = if x < 100 { image::Rgb([255, 0, 0]) } else { image::Rgb([0, 0, 255]) };
        }
        img.save(&src).expect("write split fixture");

        let fx = Effects { zoom: 2.0, pan_x: 1.0, ..Default::default() };
        let filter = fx.reframe_filter(200, 100).expect("reframe filter");
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-i", &src.to_string_lossy(), "-vf", &filter,
                   "-frames:v", "1", &out.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false), "ffmpeg reframe failed: {filter}");

        let got = image::open(&out).expect("read reframed").to_rgb8();
        assert_eq!(got.dimensions(), (200, 100), "reframe keeps the target frame");
        // Fully panned right at 2× → every pixel comes from the blue half.
        for probe_x in [5u32, 100, 195] {
            let px = got.get_pixel(probe_x, 50).0;
            assert!(px[2] > 200 && px[0] < 60, "expected blue at x={probe_x}, got {px:?}");
        }

        // The shader samples with the same geometry — check the centre and
        // both edges land inside the source's right half (u >= 0.5).
        let (z, pan) = (fx.zoom, fx.pan_x);
        for uv_x in [0.0f32, 0.5, 1.0] {
            let u_src = (uv_x - 0.5) / z + 0.5 + pan * (1.0 - 1.0 / z) * 0.5;
            assert!(
                (0.499..=1.001).contains(&u_src),
                "shader UV {u_src} should sit in the right half for uv {uv_x}"
            );
        }
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn identity_effects_emit_no_filters() {
        let fx = Effects::default();
        assert!(fx.is_identity());
        assert!(fx.filters(5.0).is_empty(), "identity must not touch the picture");
        assert_eq!(fx.fade_alpha(2.0, 5.0), 1.0);
    }

    #[test]
    fn fades_ramp_and_are_scoped_to_the_clip() {
        let fx = Effects { fade_in: 1.0, fade_out: 2.0, ..Default::default() };
        assert_eq!(fx.fade_alpha(0.0, 10.0), 0.0);
        assert!((fx.fade_alpha(0.5, 10.0) - 0.5).abs() < 1e-6);
        assert_eq!(fx.fade_alpha(5.0, 10.0), 1.0);
        assert!((fx.fade_alpha(9.0, 10.0) - 0.5).abs() < 1e-6);
        assert_eq!(fx.fade_alpha(10.0, 10.0), 0.0);
        let f = fx.filters(10.0).join(",");
        assert!(f.contains("fade=t=in:st=0:d=1.0000"), "{f}");
        assert!(f.contains("fade=t=out:st=8.0000:d=2.0000"), "{f}");
        // A fade longer than the clip is clamped, never negative.
        let long = Effects { fade_out: 30.0, ..Default::default() };
        assert!(long.filters(2.0).join(",").contains("fade=t=out:st=0.0000:d=2.0000"));
    }
}
