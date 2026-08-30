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
    /// Tone curves — master and per-channel, five points each at fixed
    /// inputs (0, ¼, ½, ¾, 1). Baked with the LUT into one lattice, so the
    /// GPU cost of a full grade is one texture sample.
    #[serde(default)]
    pub curves: Option<Curves>,
    /// Levels: input black point (0 = untouched). Values at or below it
    /// become black. Baked into the grade lattice with everything below.
    #[serde(default)]
    pub levels_black: f32,
    /// Levels: input white point (1 = untouched). Values at or above it
    /// become white.
    #[serde(default = "one")]
    pub levels_white: f32,
    /// Levels: mid gamma. 1 = untouched, >1 brightens mids (Photoshop's
    /// convention: out = lin^(1/gamma)).
    #[serde(default = "one")]
    pub levels_gamma: f32,
    /// White balance temperature, -1..1. Positive warms (more red, less
    /// blue), negative cools.
    #[serde(default)]
    pub wb_temp: f32,
    /// White balance tint, -1..1. Positive shifts magenta (less green),
    /// negative shifts green.
    #[serde(default)]
    pub wb_tint: f32,
    /// HSL qualifier — select a hue/saturation/lightness window, then push
    /// only those pixels. The classic "make the sky bluer" tool.
    #[serde(default)]
    pub hsl: Option<Hsl>,
    /// Mirror the picture left-right. With flip_v = a 180° rotation.
    #[serde(default)]
    pub flip_h: bool,
    /// Mirror the picture top-bottom.
    #[serde(default)]
    pub flip_v: bool,
}

/// An HSL secondary: the selection window and the push applied inside it.
/// Everything is baked into the grade lattice — no extra GPU cost.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Hsl {
    /// Window centre hue, degrees 0..360.
    pub hue: f32,
    /// Half-width of the hue window, degrees.
    pub hue_width: f32,
    /// Saturation window, 0..1.
    pub sat_min: f32,
    pub sat_max: f32,
    /// Lightness window, 0..1.
    pub lum_min: f32,
    pub lum_max: f32,
    /// Soft edge on every window boundary (fractional; floored at 0.02 so
    /// the 33³ lattice can represent the transition without stair-steps).
    #[serde(default = "default_hsl_soft")]
    pub soft: f32,
    /// Hue shift applied inside the window, degrees.
    #[serde(default)]
    pub push_hue: f32,
    /// Saturation multiplier inside the window. 1 = untouched.
    #[serde(default = "one")]
    pub push_sat: f32,
    /// Lightness multiplier inside the window. 1 = untouched.
    #[serde(default = "one")]
    pub push_lum: f32,
}

fn default_hsl_soft() -> f32 {
    0.1
}

impl Default for Hsl {
    fn default() -> Self {
        Self {
            hue: 210.0, // skies are the most-qualified thing in the world
            hue_width: 40.0,
            sat_min: 0.0,
            sat_max: 1.0,
            lum_min: 0.0,
            lum_max: 1.0,
            soft: default_hsl_soft(),
            push_hue: 0.0,
            push_sat: 1.0,
            push_lum: 1.0,
        }
    }
}

impl Hsl {
    /// Selection weight 0..1 for a pixel at (hue, sat, lightness): the
    /// product of three soft windows. `soft` is the edge width — sat/lum
    /// use it directly, hue scales it by the hue width so a narrow window
    /// keeps a proportionate edge.
    pub fn weight(&self, hsl: [f32; 3]) -> f32 {
        let soft = self.soft.max(0.02);
        let smooth = |edge0: f32, edge1: f32, x: f32| -> f32 {
            let t = ((x - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        // Circular hue distance from the centre, in degrees.
        let d = (hsl[0] - self.hue).rem_euclid(360.0);
        let d = d.min(360.0 - d);
        let hw = self.hue_width.max(1.0);
        let hue_w = 1.0 - smooth(hw, hw + hw.max(10.0) * soft * 4.0, d);
        let band = |lo: f32, hi: f32, x: f32| -> f32 {
            smooth(lo - soft, lo, x) * (1.0 - smooth(hi, hi + soft, x))
        };
        hue_w * band(self.sat_min, self.sat_max, hsl[1]) * band(self.lum_min, self.lum_max, hsl[2])
    }

    pub fn is_identity(&self) -> bool {
        self.push_hue.abs() < 1e-3
            && (self.push_sat - 1.0).abs() < 1e-4
            && (self.push_lum - 1.0).abs() < 1e-4
    }
}

/// sRGB 0..1 → (hue degrees 0..360, saturation 0..1, lightness 0..1).
pub fn rgb_to_hsl(rgb: [f32; 3]) -> [f32; 3] {
    let (r, g, b) = (rgb[0], rgb[1], rgb[2]);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;
    let d = max - min;
    if d < 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-6);
    let h = if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    [h, s.clamp(0.0, 1.0), l]
}

/// The inverse of `rgb_to_hsl`.
pub fn hsl_to_rgb(hsl: [f32; 3]) -> [f32; 3] {
    let (h, s, l) = (hsl[0].rem_euclid(360.0), hsl[1].clamp(0.0, 1.0), hsl[2].clamp(0.0, 1.0));
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c * 0.5;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

/// Editable tone curves. Each array holds the OUTPUT at inputs
/// 0, 0.25, 0.5, 0.75, 1 — identity is `[0, .25, .5, .75, 1]`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    pub master: [f32; 5],
    pub r: [f32; 5],
    pub g: [f32; 5],
    pub b: [f32; 5],
}

pub const CURVE_ID: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

impl Default for Curves {
    fn default() -> Self {
        Self { master: CURVE_ID, r: CURVE_ID, g: CURVE_ID, b: CURVE_ID }
    }
}

impl Curves {
    pub fn is_identity(&self) -> bool {
        let close = |a: &[f32; 5]| a.iter().zip(&CURVE_ID).all(|(x, y)| (x - y).abs() < 1e-4);
        close(&self.master) && close(&self.r) && close(&self.g) && close(&self.b)
    }

    /// One channel through master + its own curve.
    pub fn apply(&self, channel: usize, v: f32) -> f32 {
        let per = match channel {
            0 => &self.r,
            1 => &self.g,
            _ => &self.b,
        };
        curve_eval(per, curve_eval(&self.master, v))
    }
}

/// Evaluate a five-point curve at `x` — Catmull-Rom through the points with
/// clamped ends, then clamped to 0..1. Smooth enough for grading, simple
/// enough to reason about, and exactly what the lattice bake computes.
pub fn curve_eval(pts: &[f32; 5], x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0) * 4.0;
    let i = (x.floor() as usize).min(3);
    let t = x - i as f32;
    // Phantom ends MIRROR the first/last segments (2·p0 − p1), so collinear
    // points stay a straight line — clamping them instead sags the ends.
    let p = |j: isize| -> f32 {
        match j {
            -1 => 2.0 * pts[0] - pts[1],
            5 => 2.0 * pts[4] - pts[3],
            j => pts[j.clamp(0, 4) as usize],
        }
    };
    let (p0, p1, p2, p3) = (p(i as isize - 1), p(i as isize), p(i as isize + 1), p(i as isize + 2));
    let a = 2.0 * p1;
    let b = p2 - p0;
    let c = 2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3;
    let d = -p0 + 3.0 * p1 - 3.0 * p2 + p3;
    (0.5 * (a + b * t + c * t * t + d * t * t * t)).clamp(0.0, 1.0)
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
            curves: None,
            levels_black: 0.0,
            levels_white: 1.0,
            levels_gamma: 1.0,
            wb_temp: 0.0,
            wb_tint: 0.0,
            hsl: None,
            flip_h: false,
            flip_v: false,
        }
    }
}

/// Rec.709 luma weights — the same ones the shader and ffmpeg's matrix use.
pub const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

impl Effects {
    /// Does this clip grade through a lattice? LUT, curves, levels, white
    /// balance and HSL qualifiers all bake into the same 33³ texture.
    pub fn has_lattice(&self) -> bool {
        self.lut.is_some()
            || self.curves.map(|c| !c.is_identity()).unwrap_or(false)
            || self.has_grade()
    }

    /// Any of the lattice-baked colour corrections active (beyond LUT and
    /// curves, which have their own flags)?
    pub fn has_grade(&self) -> bool {
        self.levels_black.abs() > 1e-4
            || (self.levels_white - 1.0).abs() > 1e-4
            || (self.levels_gamma - 1.0).abs() > 1e-4
            || self.wb_temp.abs() > 1e-4
            || self.wb_tint.abs() > 1e-4
            || self.hsl.map(|q| !q.is_identity()).unwrap_or(false)
    }

    /// The lattice-baked colour correction: white balance, then levels,
    /// then the HSL qualifier — on sRGB-encoded values, like everything in
    /// this module. ONE formula: `bake_grade` samples it into the lattice
    /// both pipelines draw with, and the fallback's lut3d export carries
    /// the identical numbers.
    pub fn grade_reference(&self, rgb: [f32; 3]) -> [f32; 3] {
        let mut c = rgb;
        // White balance: opposing channel gains. ±1 temp swings red/blue by
        // 25% — enough to rescue tungsten, small enough to stay linear-ish.
        if self.wb_temp.abs() > 1e-4 || self.wb_tint.abs() > 1e-4 {
            let t = self.wb_temp.clamp(-1.0, 1.0);
            let g = self.wb_tint.clamp(-1.0, 1.0);
            c[0] *= 1.0 + 0.25 * t;
            c[2] *= 1.0 - 0.25 * t;
            c[1] *= 1.0 - 0.15 * g;
        }
        // Levels: remap black..white to 0..1, then the gamma mid slider.
        let black = self.levels_black.clamp(0.0, 0.95);
        let white = self.levels_white.clamp(black + 0.05, 2.0);
        let gamma = self.levels_gamma.clamp(0.1, 10.0);
        for v in &mut c {
            let lin = ((*v - black) / (white - black)).clamp(0.0, 1.0);
            *v = lin.powf(1.0 / gamma);
        }
        // HSL qualifier: weight by the window, push inside it.
        if let Some(q) = self.hsl {
            if !q.is_identity() {
                let hsl = rgb_to_hsl([c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0)]);
                let w = q.weight(hsl);
                if w > 1e-4 {
                    let pushed = hsl_to_rgb([
                        hsl[0] + q.push_hue * w,
                        hsl[1] * (1.0 + (q.push_sat - 1.0) * w),
                        hsl[2] * (1.0 + (q.push_lum - 1.0) * w),
                    ]);
                    c = pushed;
                }
            }
        }
        [c[0].clamp(0.0, 1.0), c[1].clamp(0.0, 1.0), c[2].clamp(0.0, 1.0)]
    }

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
            && self.curves.map(|c| c.is_identity()).unwrap_or(true)
            && !self.has_grade()
            && !self.flip_h
            && !self.flip_v
    }

    /// Take another clip's GRADE — the colour work only. Fades, reframe,
    /// keying and the power window stay as they were: those are per-shot
    /// decisions, a grade is a look.
    pub fn copy_grade_from(&mut self, other: &Effects) {
        self.exposure = other.exposure;
        self.contrast = other.contrast;
        self.saturation = other.saturation;
        self.levels_black = other.levels_black;
        self.levels_white = other.levels_white;
        self.levels_gamma = other.levels_gamma;
        self.wb_temp = other.wb_temp;
        self.wb_tint = other.wb_tint;
        self.hsl = other.hsl;
        self.lut = other.lut;
        self.curves = other.curves;
    }

    /// Compose a stack for the shader's SCALAR path: the base clip's
    /// spatial settings (reframe, key, mask, fades) with every layer's
    /// trims multiplied together. The lattice half of the stack is baked
    /// separately (`lut::bake_stack`) — this covers what rides uniforms.
    pub fn compose_stack(stack: &[&Effects]) -> Effects {
        let mut out = *stack[0];
        for fx in &stack[1..] {
            out.exposure *= fx.exposure;
            out.contrast *= fx.contrast;
            out.saturation *= fx.saturation;
        }
        // The lattice half is baked from the stack and bound separately;
        // the enable flag follows the binding, not this struct.
        out
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
        // Flips first: geometry before colour, matching where the shaders
        // mirror the sampling coordinates.
        if self.flip_h {
            f.push("hflip".into());
        }
        if self.flip_v {
            f.push("vflip".into());
        }
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
mod grade_tests {
    use super::*;

    #[test]
    fn hsl_round_trips_and_matches_known_colours() {
        for rgb in [
            [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0],
            [0.3, 0.6, 0.9], [0.5, 0.5, 0.5], [0.9, 0.1, 0.4],
        ] {
            let back = hsl_to_rgb(rgb_to_hsl(rgb));
            for c in 0..3 {
                assert!((back[c] - rgb[c]).abs() < 1e-4, "{rgb:?} → {back:?}");
            }
        }
        assert!((rgb_to_hsl([1.0, 0.0, 0.0])[0]).abs() < 0.5, "pure red is hue 0");
        assert!((rgb_to_hsl([0.0, 0.0, 1.0])[0] - 240.0).abs() < 0.5, "pure blue is hue 240");
    }

    #[test]
    fn grade_reference_moves_in_the_stated_directions() {
        let id = Effects::default();
        assert!(!id.has_grade());
        let probe = [0.4, 0.5, 0.6];
        assert_eq!(id.grade_reference(probe), probe, "identity passes through exactly");

        // Warm temperature: red up, blue down, green untouched.
        let warm = Effects { wb_temp: 0.5, ..Default::default() };
        let w = warm.grade_reference(probe);
        assert!(w[0] > probe[0] && w[2] < probe[2] && (w[1] - probe[1]).abs() < 1e-5, "{w:?}");
        // Green tint: green up.
        let green = Effects { wb_tint: -0.5, ..Default::default() };
        assert!(green.grade_reference(probe)[1] > probe[1]);

        // Levels: a raised black point crushes shadows to zero and keeps
        // white at white; gamma > 1 brightens the mids.
        let lv = Effects { levels_black: 0.2, ..Default::default() };
        assert_eq!(lv.grade_reference([0.1, 0.1, 0.1]), [0.0, 0.0, 0.0]);
        assert_eq!(lv.grade_reference([1.0, 1.0, 1.0]), [1.0, 1.0, 1.0]);
        let bright = Effects { levels_gamma: 2.0, ..Default::default() };
        assert!(bright.grade_reference([0.25, 0.25, 0.25])[0] > 0.4);

        // The qualifier: a blue pixel inside a blue window desaturates when
        // told to; a red pixel outside the window is untouched.
        let kill_blue = Effects {
            hsl: Some(Hsl { hue: 240.0, hue_width: 40.0, push_sat: 0.0, ..Default::default() }),
            ..Default::default()
        };
        let blue = kill_blue.grade_reference([0.2, 0.3, 0.9]);
        assert!((blue[0] - blue[2]).abs() < 0.05, "blue desaturated toward grey: {blue:?}");
        let red = kill_blue.grade_reference([0.9, 0.2, 0.2]);
        assert!(red[0] - red[2] > 0.5, "red is outside the window: {red:?}");
    }

    /// The lattice IS the formula: baking levels+WB+HSL into 33³ and
    /// sampling it trilinearly must agree with grade_reference off-node.
    #[test]
    fn the_baked_lattice_matches_the_direct_formula() {
        let fx = Effects {
            levels_black: 0.05,
            levels_gamma: 1.3,
            wb_temp: 0.3,
            hsl: Some(Hsl { hue: 240.0, hue_width: 50.0, push_sat: 0.3, ..Default::default() }),
            ..Default::default()
        };
        let lattice = crate::lut::bake_grade(None, &fx);
        for probe in [
            [0.13, 0.47, 0.81], [0.5, 0.5, 0.5], [0.02, 0.98, 0.33], [0.7, 0.7, 0.1],
        ] {
            let direct = fx.grade_reference(probe);
            let via = crate::lut::apply_reference(&lattice, probe);
            for c in 0..3 {
                assert!(
                    (direct[c] - via[c]).abs() < 0.035,
                    "lattice drifted at {probe:?}: direct {direct:?} vs lattice {via:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod curve_tests {
    use super::*;

    #[test]
    fn curves_pass_identity_through_and_bend_where_told() {
        // Identity in, identity out — everywhere.
        for x in [0.0, 0.1, 0.25, 0.5, 0.77, 1.0] {
            assert!((curve_eval(&CURVE_ID, x) - x).abs() < 1e-3, "identity broke at {x}");
        }
        // A lifted midpoint bends the middle up and leaves the ends alone.
        let lift = [0.0, 0.25, 0.65, 0.75, 1.0];
        assert!((curve_eval(&lift, 0.0)).abs() < 1e-4);
        assert!((curve_eval(&lift, 1.0) - 1.0).abs() < 1e-4);
        assert!((curve_eval(&lift, 0.5) - 0.65).abs() < 1e-4, "points are interpolated exactly");
        assert!(curve_eval(&lift, 0.4) > 0.4, "the lift spreads smoothly");
        // Output clamps.
        let wild = [0.0, -0.5, 1.5, 0.75, 1.0];
        for x in [0.1, 0.3, 0.5, 0.9] {
            let y = curve_eval(&wild, x);
            assert!((0.0..=1.0).contains(&y));
        }
        // Master then channel compose.
        let cv = Curves { master: lift, r: [0.0, 0.2, 0.4, 0.6, 1.0], ..Default::default() };
        let expect = curve_eval(&[0.0, 0.2, 0.4, 0.6, 1.0], curve_eval(&lift, 0.5));
        assert!((cv.apply(0, 0.5) - expect).abs() < 1e-6);
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
