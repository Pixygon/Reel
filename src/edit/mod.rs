//! The editing data model — an NLE project of tracks and clips on a timeline.
//! v0.1 defines the model and renders it (see ui::timeline); trimming, ripple,
//! effects and export are on the roadmap. Kept serde-serializable so a project
//! is a saveable `.reel` document from the start.

use crate::effects::Effects;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrackKind {
    /// The base sequence — the cut itself.
    Video,
    Audio,
    /// Picture composited ON TOP of the base sequence: a PiP window, a
    /// reaction cam, a logo. Kept a distinct kind rather than "a second video
    /// track" so the flattening that builds the cut can't accidentally splice
    /// an overlay into the main sequence.
    Overlay,
}

/// One clip placed on a track: a window `[in_point, in_point+duration)` of a
/// source media file, positioned at `start` on the timeline. All times seconds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub name: String,
    pub source: String, // media file path
    pub start: f64,     // timeline position
    pub in_point: f64,  // offset into the source
    pub duration: f64,
    /// Colour adjustments and fades for this clip. Defaults to identity, and
    /// is `serde(default)` so `.reel` files written before effects still load.
    #[serde(default)]
    pub effects: Effects,
    /// Seconds of crossfade FROM the previous clip into this one. The two
    /// clips overlap by this much, so the edit gets shorter — exactly what
    /// ffmpeg's `xfade` does at render time.
    #[serde(default)]
    pub transition_in: f64,
    /// The shape of that handover.
    #[serde(default)]
    pub transition_kind: TransitionKind,
    /// Level change for this clip's audio, in decibels. 0 is untouched.
    #[serde(default)]
    pub gain_db: f32,
    /// Playback rate. 2.0 plays twice as fast, 0.5 half. `duration` is the
    /// clip's length ON THE TIMELINE, so the window it consumes from the
    /// source is `duration * speed` — see `source_len`.
    #[serde(default = "one")]
    pub speed: f32,
    /// Where this clip sits in the frame when it's on an overlay track.
    /// Ignored elsewhere. Fractions of the frame, so a PiP placed against a
    /// 720p preview lands identically in a 4K render — same rule as titles.
    #[serde(default)]
    pub pip: Pip,
    /// Animated parameters: sorted keyframe tracks in clip-local time.
    /// Empty for every clip that never touches animation.
    #[serde(default)]
    pub keys: Vec<(Param, Vec<Keyframe>)>,
    /// Smooth this clip's camera shake at render time (vidstab two-pass).
    /// The preview shows the raw footage — stabilisation happens on export.
    #[serde(default)]
    pub stabilize: bool,
    /// Audio processing for this clip: pan, tone, dynamics, repair. ONE
    /// definition consumed by the export chain and the live mixer alike.
    #[serde(default)]
    pub audio: AudioFx,
    /// An ADJUSTMENT LAYER: this overlay clip draws nothing of its own —
    /// its Effects apply to everything beneath it for its time window.
    #[serde(default)]
    pub adjustment: bool,
    /// A COMPOUND CLIP's origin: the nested .reel this clip's source was
    /// flattened from. The source stays the flat render; this is what lets
    /// Reel notice the nested edit changed and refresh it.
    #[serde(default)]
    pub nested: Option<String>,
    /// EXPERT: a raw ffmpeg video-filter chain spliced into this clip's
    /// decode (before fit). Applied by the render and the frame export; the
    /// LIVE preview cannot run ffmpeg filters and shows the clip without
    /// it — the UI says so. Validated with a trial run when set.
    #[serde(default)]
    pub raw_filter: Option<String>,
}

/// Per-clip audio processing. Defaults are identity; every field is
/// `serde(default)` so older `.reel` documents keep loading. The export
/// renders these through ffmpeg filters and the live mixer through its own
/// DSP — behaviourally matched, measured by tests on both sides.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioFx {
    /// Stereo balance, -1 (left) .. 1 (right). Balance law: the far channel
    /// attenuates, the centre is untouched.
    #[serde(default)]
    pub pan: f32,
    /// Low shelf at 120 Hz, dB.
    #[serde(default)]
    pub eq_low: f32,
    /// Peaking bell, dB, at `eq_mid_freq`.
    #[serde(default)]
    pub eq_mid: f32,
    /// The bell's centre frequency, Hz.
    #[serde(default = "default_mid_freq")]
    pub eq_mid_freq: f32,
    /// High shelf at 8 kHz, dB.
    #[serde(default)]
    pub eq_high: f32,
    /// Compressor on?
    #[serde(default)]
    pub comp: bool,
    /// Compressor threshold, dBFS.
    #[serde(default = "default_comp_thresh")]
    pub comp_thresh: f32,
    /// Compressor ratio (N:1).
    #[serde(default = "default_comp_ratio")]
    pub comp_ratio: f32,
    /// "Fix voice": the repair chain (high-pass, denoise, de-click) at
    /// render time. The preview stays raw — repair is a real FFT pass.
    #[serde(default)]
    pub voice_fix: bool,
    /// The shape of this clip's audio fades (video fades stay linear —
    /// light and loudness read differently).
    #[serde(default)]
    pub fade_curve: FadeCurve,
    /// De-esser intensity 0..1 (0 = off). Render-time, like the repair
    /// chain — sibilance detection is a real filter pass.
    #[serde(default)]
    pub deess: f32,
}

/// Audio fade shapes, matched between the live mixer and ffmpeg's afade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FadeCurve {
    /// Straight line (afade `tri`).
    #[default]
    Linear,
    /// Equal-power-ish sine — the natural-sounding default for music
    /// (afade `qsin`).
    Smooth,
    /// Exponential — fast drop, long tail (afade `exp`).
    Exp,
}

impl FadeCurve {
    pub fn afade_name(self) -> &'static str {
        match self {
            FadeCurve::Linear => "tri",
            FadeCurve::Smooth => "qsin",
            FadeCurve::Exp => "exp",
        }
    }

    /// The gain multiplier at fade progress `p` (0 = silent end, 1 = full)
    /// — the SAME curves afade applies, for the live mixer.
    pub fn shape(self, p: f32) -> f32 {
        let p = p.clamp(0.0, 1.0);
        match self {
            FadeCurve::Linear => p,
            FadeCurve::Smooth => (p * std::f32::consts::FRAC_PI_2).sin(),
            FadeCurve::Exp => {
                // ffmpeg's exp curve: p^3-ish rise. exp in afade is
                // defined as pow(0.1, (1-p)*5) ≈ -100 dB..0 linear-in-dB.
                (0.1f32).powf((1.0 - p) * 5.0)
            }
        }
    }
}

fn default_mid_freq() -> f32 {
    1000.0
}
fn default_comp_thresh() -> f32 {
    -18.0
}
fn default_comp_ratio() -> f32 {
    3.0
}

impl Default for AudioFx {
    fn default() -> Self {
        Self {
            pan: 0.0,
            eq_low: 0.0,
            eq_mid: 0.0,
            eq_mid_freq: default_mid_freq(),
            eq_high: 0.0,
            comp: false,
            comp_thresh: default_comp_thresh(),
            comp_ratio: default_comp_ratio(),
            voice_fix: false,
            fade_curve: FadeCurve::default(),
            deess: 0.0,
        }
    }
}

impl AudioFx {
    pub fn is_identity(&self) -> bool {
        self.pan.abs() < 1e-4 && !self.has_tone() && !self.comp && !self.voice_fix
    }

    pub fn has_tone(&self) -> bool {
        self.eq_low.abs() > 0.01 || self.eq_mid.abs() > 0.01 || self.eq_high.abs() > 0.01
    }

    /// Balance-law channel gains: centre (1, 1); full right (0, 1).
    pub fn pan_gains(&self) -> (f32, f32) {
        let p = self.pan.clamp(-1.0, 1.0);
        ((1.0 - p.max(0.0)), (1.0 + p.min(0.0)))
    }

    /// Fold a track-level pan into this clip's (clamped sum — panning the
    /// track right moves every clip right).
    pub fn with_track_pan(mut self, track_pan: f32) -> Self {
        self.pan = (self.pan + track_pan).clamp(-1.0, 1.0);
        self
    }
}

/// A parameter that can be animated. The address half of the keyframe
/// system: (clip id, Param) names every animatable number in a project,
/// which is also how the CLI reads and writes them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Param {
    Exposure,
    Contrast,
    Saturation,
    Zoom,
    PanX,
    PanY,
    /// Whole-clip opacity, multiplied into fades.
    Opacity,
    PipX,
    PipY,
    PipScale,
    /// Playback rate over CLIP-LOCAL OUTPUT time — a speed RAMP. The clip's
    /// slot on the timeline stays fixed; how much source it eats becomes the
    /// integral of this curve.
    Speed,
    /// The power window's centre and half-extents — an animated mask is a
    /// hand-tracked one.
    MaskX,
    MaskY,
    MaskW,
    MaskH,
    /// The effect plugin's four parameter sliders — animatable, so a
    /// vignette can breathe or a glitch can pulse.
    Plugin1,
    Plugin2,
    Plugin3,
    Plugin4,
}

impl Param {
    /// The value range the curve editor displays for this parameter.
    pub fn range(self) -> (f32, f32) {
        match self {
            Param::Exposure | Param::Contrast => (0.2, 2.5),
            Param::Saturation => (0.0, 2.5),
            Param::Zoom => (1.0, 3.0),
            Param::PanX | Param::PanY => (-1.0, 1.0),
            Param::Opacity => (0.0, 1.0),
            Param::PipX | Param::PipY => (0.0, 1.0),
            Param::PipScale => (0.05, 1.0),
            Param::Speed => (0.25, 4.0),
            Param::MaskX | Param::MaskY => (0.0, 1.0),
            Param::MaskW | Param::MaskH => (0.02, 0.8),
            // Plugins declare their own ranges; the lane shows a generous
            // window and the plugin clamps as it sees fit.
            Param::Plugin1 | Param::Plugin2 | Param::Plugin3 | Param::Plugin4 => (0.0, 2.0),
        }
    }

    pub const ALL: [Param; 19] = [
        Param::Exposure,
        Param::Contrast,
        Param::Saturation,
        Param::Zoom,
        Param::PanX,
        Param::PanY,
        Param::Opacity,
        Param::PipX,
        Param::PipY,
        Param::PipScale,
        Param::Speed,
        Param::MaskX,
        Param::MaskY,
        Param::MaskW,
        Param::MaskH,
        Param::Plugin1,
        Param::Plugin2,
        Param::Plugin3,
        Param::Plugin4,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Param::Exposure => "exposure",
            Param::Contrast => "contrast",
            Param::Saturation => "saturation",
            Param::Zoom => "zoom",
            Param::PanX => "pan-x",
            Param::PanY => "pan-y",
            Param::Opacity => "opacity",
            Param::PipX => "pip-x",
            Param::PipY => "pip-y",
            Param::PipScale => "pip-scale",
            Param::Speed => "speed",
            Param::MaskX => "mask-x",
            Param::MaskY => "mask-y",
            Param::MaskW => "mask-w",
            Param::MaskH => "mask-h",
            Param::Plugin1 => "plugin-1",
            Param::Plugin2 => "plugin-2",
            Param::Plugin3 => "plugin-3",
            Param::Plugin4 => "plugin-4",
        }
    }

    pub fn parse(s: &str) -> Option<Param> {
        Param::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// How one clip hands over to the next when they overlap.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub enum TransitionKind {
    /// Crossfade: the incoming picture blends over the outgoing.
    #[default]
    Fade,
    /// Dip to black: out fades down, then in fades up.
    DipToBlack,
    /// The incoming picture is revealed by a travelling edge.
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    /// The incoming picture slides in over the outgoing.
    SlideLeft,
    SlideRight,
}

impl TransitionKind {
    pub const ALL: [TransitionKind; 8] = [
        TransitionKind::Fade,
        TransitionKind::DipToBlack,
        TransitionKind::WipeLeft,
        TransitionKind::WipeRight,
        TransitionKind::WipeUp,
        TransitionKind::WipeDown,
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Fade => "Crossfade",
            TransitionKind::DipToBlack => "Dip to black",
            TransitionKind::WipeLeft => "Wipe left",
            TransitionKind::WipeRight => "Wipe right",
            TransitionKind::WipeUp => "Wipe up",
            TransitionKind::WipeDown => "Wipe down",
            TransitionKind::SlideLeft => "Slide left",
            TransitionKind::SlideRight => "Slide right",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TransitionKind::Fade => "fade",
            TransitionKind::DipToBlack => "dip",
            TransitionKind::WipeLeft => "wipe-left",
            TransitionKind::WipeRight => "wipe-right",
            TransitionKind::WipeUp => "wipe-up",
            TransitionKind::WipeDown => "wipe-down",
            TransitionKind::SlideLeft => "slide-left",
            TransitionKind::SlideRight => "slide-right",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == s)
    }

    /// The equivalent ffmpeg `xfade` transition — the graph fallback's map.
    pub fn xfade_name(self) -> &'static str {
        match self {
            TransitionKind::Fade => "fade",
            TransitionKind::DipToBlack => "fadeblack",
            TransitionKind::WipeLeft => "wipeleft",
            TransitionKind::WipeRight => "wiperight",
            TransitionKind::WipeUp => "wipeup",
            TransitionKind::WipeDown => "wipedown",
            TransitionKind::SlideLeft => "slideleft",
            TransitionKind::SlideRight => "slideright",
        }
    }
}

/// How a keyframe reaches the next one.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum Interp {
    Linear,
    /// Step: hold this value until the next keyframe.
    Hold,
    /// Smooth in and out (smoothstep) — the "just make it nice" curve.
    Ease,
    /// A cubic-bezier ease with editable handles, CSS-style: control
    /// points in normalised segment space. (x1,y1) shapes the way OUT of
    /// this key, (x2,y2) the way INTO the next.
    Bezier { x1: f32, y1: f32, x2: f32, y2: f32 },
}

impl Interp {
    /// A pleasant default bezier (ease-in-out) for freshly-added handles.
    pub fn bezier_default() -> Self {
        Interp::Bezier { x1: 0.42, y1: 0.0, x2: 0.58, y2: 1.0 }
    }
}

/// Eased progress through a cubic bezier: solve x(u) = p for u (the curve
/// is monotone in x when handles stay in 0..1), then return y(u).
pub fn bezier_ease(x1: f32, y1: f32, x2: f32, y2: f32, p: f32) -> f32 {
    let (x1, x2) = (x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0));
    let cubic = |a: f32, b: f32, u: f32| -> f32 {
        // Bernstein form with P0=0, P3=1.
        let v = 1.0 - u;
        3.0 * v * v * u * a + 3.0 * v * u * u * b + u * u * u
    };
    // Bisection on x — monotone, 24 steps ≈ 6e-8 precision.
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let mid = (lo + hi) * 0.5;
        if cubic(x1, x2, mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    cubic(y1, y2, (lo + hi) * 0.5)
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Keyframe {
    /// Clip-local timeline seconds (0 = the clip's start on screen).
    pub t: f64,
    pub value: f32,
    pub interp: Interp,
}

/// Evaluate a sorted keyframe track at clip-local time `t`. Before the first
/// key it holds the first value; after the last, the last — an animation
/// never extrapolates into nonsense.
pub fn eval_keys(keys: &[Keyframe], t: f64) -> Option<f32> {
    let first = keys.first()?;
    if t <= first.t {
        return Some(first.value);
    }
    let last = keys.last()?;
    if t >= last.t {
        return Some(last.value);
    }
    let idx = keys.iter().rposition(|k| k.t <= t)?;
    let a = &keys[idx];
    let b = &keys[idx + 1];
    let span = (b.t - a.t).max(1e-9);
    let p = ((t - a.t) / span).clamp(0.0, 1.0) as f32;
    let p = match a.interp {
        Interp::Hold => 0.0,
        Interp::Linear => p,
        Interp::Ease => p * p * (3.0 - 2.0 * p),
        Interp::Bezier { x1, y1, x2, y2 } => bezier_ease(x1, y1, x2, y2, p),
    };
    Some(a.value + (b.value - a.value) * p)
}

/// Seconds of SOURCE consumed by a speed curve over output time `0..t`.
///
/// Piecewise analytic: within one interval the mean of a linear ramp is
/// (a+b)/2 — and the mean of a smoothstep ease is ALSO (a+b)/2 (its integral
/// over 0..1 is exactly ½) — while a hold contributes its own value. The
/// audio path leans on the same identity, which is what keeps a ramped
/// clip's sound the same length as its picture.
pub fn speed_integral(keys: &[Keyframe], base: f32, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if keys.is_empty() {
        return base.max(0.01) as f64 * t;
    }
    let mut acc = 0.0f64;
    let mut cursor = 0.0f64;
    // Before the first key: held at the first key's value.
    let first = &keys[0];
    if cursor < first.t {
        let span = first.t.min(t) - cursor;
        acc += first.value.max(0.01) as f64 * span;
        cursor += span;
        if cursor >= t {
            return acc;
        }
    }
    for w in keys.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if cursor >= t {
            break;
        }
        let seg_end = b.t.min(t);
        if seg_end <= cursor {
            continue;
        }
        let span_full = (b.t - a.t).max(1e-9);
        let (va, vb) = (a.value.max(0.01) as f64, b.value.max(0.01) as f64);
        // Integrate from `cursor` to `seg_end` inside this interval.
        let p0 = ((cursor - a.t) / span_full).clamp(0.0, 1.0);
        let p1 = ((seg_end - a.t) / span_full).clamp(0.0, 1.0);
        let anti = |p: f64| -> f64 {
            // Antiderivative of speed(p) in interval-normalised time.
            match a.interp {
                Interp::Hold => va * p,
                Interp::Linear => va * p + (vb - va) * p * p / 2.0,
                // smoothstep: ∫(va + (vb-va)(3p²-2p³))dp
                Interp::Ease => va * p + (vb - va) * (p.powi(3) - p.powi(4) / 2.0),
                // A bezier has no tidy antiderivative in x — integrate
                // numerically. Deterministic (fixed step), and the same
                // function feeds picture, sound and the inverse map, so
                // they can't disagree.
                Interp::Bezier { x1, y1, x2, y2 } => {
                    const N: usize = 64;
                    let mut sum = 0.0f64;
                    for i in 0..N {
                        let u = (i as f64 + 0.5) / N as f64 * p;
                        let e = bezier_ease(x1, y1, x2, y2, u as f32) as f64;
                        sum += va + (vb - va) * e;
                    }
                    sum * p / N as f64
                }
            }
        };
        acc += (anti(p1) - anti(p0)) * span_full;
        cursor = seg_end;
    }
    // After the last key: held at the last key's value.
    if cursor < t {
        acc += keys.last().unwrap().value.max(0.01) as f64 * (t - cursor);
    }
    acc
}

/// Output time at which a speed curve has consumed `src` seconds of source —
/// the inverse of `speed_integral`, found by bisection (the integral is
/// strictly increasing, so this is safe and exact enough for mapping).
pub fn speed_integral_invert(keys: &[Keyframe], base: f32, src: f64, max_t: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, max_t.max(0.0));
    for _ in 0..48 {
        let mid = (lo + hi) / 2.0;
        if speed_integral(keys, base, mid) < src {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    (lo + hi) / 2.0
}

/// Placement of an overlay clip within the frame, in fractions.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pip {
    /// Centre of the inset.
    pub x: f32,
    pub y: f32,
    /// Width of the inset as a fraction of the frame width.
    pub scale: f32,
}

impl Default for Pip {
    fn default() -> Self {
        // Bottom-right quarter — where a webcam inset conventionally goes.
        Self { x: 0.76, y: 0.74, scale: 0.30 }
    }
}

impl Clip {
    /// The keyframe track for one parameter, if any.
    pub fn key_track(&self, p: Param) -> Option<&[Keyframe]> {
        self.keys.iter().find(|(q, _)| *q == p).map(|(_, k)| k.as_slice())
    }

    /// This clip's animatable values at clip-local time `t`: base values
    /// overridden by whatever is keyframed. ONE evaluation used by the
    /// preview, the frame server and the CLI — that single call site is what
    /// makes "the preview never lies" survive animation.
    pub fn animated(&self, t: f64) -> (Effects, Pip, f32) {
        let mut fx = self.effects;
        let mut pip = self.pip;
        let mut opacity = 1.0f32;
        for (p, keys) in &self.keys {
            let Some(v) = eval_keys(keys, t) else { continue };
            match p {
                Param::Exposure => fx.exposure = v,
                Param::Contrast => fx.contrast = v,
                Param::Saturation => fx.saturation = v,
                Param::Zoom => fx.zoom = v.max(1.0),
                Param::PanX => fx.pan_x = v.clamp(-1.0, 1.0),
                Param::PanY => fx.pan_y = v.clamp(-1.0, 1.0),
                Param::Opacity => opacity = v.clamp(0.0, 1.0),
                Param::PipX => pip.x = v,
                Param::PipY => pip.y = v,
                Param::PipScale => pip.scale = v.clamp(0.02, 1.0),
                // Speed is a time-warp, not a per-instant look — the mapping
                // functions (source_offset_at) own it.
                Param::Speed => {}
                Param::MaskX => {
                    if let Some(m) = &mut fx.mask {
                        m.cx = v;
                    }
                }
                Param::MaskY => {
                    if let Some(m) = &mut fx.mask {
                        m.cy = v;
                    }
                }
                Param::MaskW => {
                    if let Some(m) = &mut fx.mask {
                        m.w = v;
                    }
                }
                Param::MaskH => {
                    if let Some(m) = &mut fx.mask {
                        m.h = v;
                    }
                }
                Param::Plugin1 => fx.plugin_params[0] = v,
                Param::Plugin2 => fx.plugin_params[1] = v,
                Param::Plugin3 => fx.plugin_params[2] = v,
                Param::Plugin4 => fx.plugin_params[3] = v,
            }
        }
        (fx, pip, opacity)
    }

    /// Insert (or replace) a keyframe, keeping the track sorted.
    pub fn set_key(&mut self, p: Param, t: f64, value: f32, interp: Interp) {
        let track = match self.keys.iter_mut().find(|(q, _)| *q == p) {
            Some((_, k)) => k,
            None => {
                self.keys.push((p, Vec::new()));
                &mut self.keys.last_mut().unwrap().1
            }
        };
        track.retain(|k| (k.t - t).abs() > 1e-4);
        track.push(Keyframe { t, value, interp });
        track.sort_by(|a, b| a.t.total_cmp(&b.t));
    }

    /// Remove the keyframe nearest `t` (within tolerance). True if one went.
    pub fn clear_key(&mut self, p: Param, t: f64) -> bool {
        let Some((_, track)) = self.keys.iter_mut().find(|(q, _)| *q == p) else {
            return false;
        };
        let before = track.len();
        if let Some(i) = track
            .iter()
            .enumerate()
            .filter(|(_, k)| (k.t - t).abs() < 0.1)
            .min_by(|a, b| (a.1.t - t).abs().total_cmp(&(b.1.t - t).abs()))
            .map(|(i, _)| i)
        {
            track.remove(i);
        }
        let gone = track.len() < before;
        self.keys.retain(|(_, k)| !k.is_empty());
        gone
    }

    /// How much of the SOURCE this clip consumes. Equal to `duration` at
    /// normal speed; twice that at 2×. Every place that reads a window out of
    /// the source file — trimming, waveforms, thumbnails, caption mapping —
    /// must use this rather than `duration`, or the picture and the sound
    /// drift apart the moment a clip is sped up.
    pub fn source_len(&self) -> f64 {
        match self.key_track(Param::Speed) {
            Some(keys) => speed_integral(keys, self.speed, self.duration),
            None => self.duration * self.speed.max(0.01) as f64,
        }
    }

    /// Source-time offset consumed after `t` seconds of output — linear for
    /// a constant speed, the curve's integral for a ramp.
    pub fn source_offset_at(&self, t: f64) -> f64 {
        match self.key_track(Param::Speed) {
            Some(keys) => speed_integral(keys, self.speed, t),
            None => t * self.speed.max(0.01) as f64,
        }
    }

    /// Output time at which this clip has consumed `src` seconds of source.
    pub fn output_time_for_source(&self, src: f64) -> f64 {
        match self.key_track(Param::Speed) {
            Some(keys) => speed_integral_invert(keys, self.speed, src, self.duration),
            None => src / self.speed.max(0.01) as f64,
        }
    }

    pub fn end(&self) -> f64 {
        self.start + self.duration
    }
}

/// Write a document without ever leaving a half-written file behind: write a
/// sibling temp file, then rename over the target (rename is atomic on the
/// same filesystem). Autosave runs constantly, so a crash mid-write must not
/// be able to destroy someone's project.
pub fn write_atomic(path: &str, contents: &str) -> anyhow::Result<()> {
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// How long the flattened edit actually renders to: crossfades overlap their
/// two clips, so each one shortens the result by its own length. The export
/// dialog and the progress bar both use this, so the duration a user sees is
/// the duration they get.
pub fn render_duration(segments: &[Segment]) -> f64 {
    let total: f64 = segments.iter().map(|s| s.duration).sum();
    let overlap: f64 = segments
        .iter()
        .enumerate()
        .map(|(i, s)| if i == 0 { 0.0 } else { s.transition_in.min(segments[i - 1].duration).min(s.duration) })
        .sum();
    (total - overlap).max(0.0)
}

/// One piece of the flattened edit, ready to render: a window of a source
/// file plus the effects that piece carries.
#[derive(Clone, Debug)]
pub struct Segment {
    pub source: String,
    pub in_point: f64,
    pub duration: f64,
    pub effects: Effects,
    /// Crossfade from the previous segment, in seconds (0 = hard cut).
    pub transition_in: f64,
    pub transition_kind: TransitionKind,
    /// Level change for this clip's own audio, in decibels.
    pub gain_db: f32,
    /// Playback rate; `duration` is already the sped-up length.
    pub speed: f32,
    /// Animated parameters, clip-local time — evaluated per frame by the
    /// frame server.
    pub keys: Vec<(Param, Vec<Keyframe>)>,
    pub stabilize: bool,
    /// Audio processing (pan/EQ/compressor/repair), track pan folded in.
    pub audio: AudioFx,
    /// Expert raw ffmpeg filter, spliced into the decode. See Clip.
    pub raw_filter: Option<String>,
}

impl Segment {
    /// Does this segment's playback rate change over time?
    pub fn has_ramp(&self) -> bool {
        self.keys
            .iter()
            .any(|(p, k)| *p == Param::Speed && !k.is_empty())
    }

    /// Seconds of source consumed after `t` seconds of output.
    pub fn source_offset_at(&self, t: f64) -> f64 {
        match self.keys.iter().find(|(p, _)| *p == Param::Speed) {
            Some((_, keys)) if !keys.is_empty() => speed_integral(keys, self.speed, t),
            _ => t * self.speed.max(0.01) as f64,
        }
    }

    /// Total source this segment consumes over its whole duration.
    pub fn source_len(&self) -> f64 {
        self.source_offset_at(self.duration)
    }

    /// Effects and opacity at segment-local time `t`, keyframes applied.
    pub fn animated(&self, t: f64) -> (Effects, f32) {
        let mut fx = self.effects;
        let mut opacity = 1.0f32;
        for (p, keys) in &self.keys {
            let Some(v) = eval_keys(keys, t) else { continue };
            match p {
                Param::Exposure => fx.exposure = v,
                Param::Contrast => fx.contrast = v,
                Param::Saturation => fx.saturation = v,
                Param::Zoom => fx.zoom = v.max(1.0),
                Param::PanX => fx.pan_x = v.clamp(-1.0, 1.0),
                Param::PanY => fx.pan_y = v.clamp(-1.0, 1.0),
                Param::Opacity => opacity = v.clamp(0.0, 1.0),
                Param::MaskX => {
                    if let Some(m) = &mut fx.mask {
                        m.cx = v;
                    }
                }
                Param::MaskY => {
                    if let Some(m) = &mut fx.mask {
                        m.cy = v;
                    }
                }
                Param::MaskW => {
                    if let Some(m) = &mut fx.mask {
                        m.w = v;
                    }
                }
                Param::MaskH => {
                    if let Some(m) = &mut fx.mask {
                        m.h = v;
                    }
                }
                Param::Plugin1 => fx.plugin_params[0] = v,
                Param::Plugin2 => fx.plugin_params[1] = v,
                Param::Plugin3 => fx.plugin_params[2] = v,
                Param::Plugin4 => fx.plugin_params[3] = v,
                _ => {}
            }
        }
        (fx, opacity)
    }
}

/// A music bed laid under the edit.
///
/// Kept deliberately small: one track, a level, and ducking. Music under
/// speech is the thing editors actually do, and the thing they most often
/// get wrong by hand — so Reel does the level-riding itself rather than
/// making you draw a volume curve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Music {
    pub source: String,
    /// Where it starts on the timeline, in seconds.
    #[serde(default)]
    pub start: f64,
    /// Level, in decibels.
    pub gain_db: f32,
    /// Pull the music down automatically whenever the edit's own audio
    /// speaks over it.
    pub duck: bool,
    /// Fade the bed in and out, in seconds (0 = hard start/stop).
    #[serde(default)]
    pub fade: f64,
    /// Stretch the whole track to end exactly with the edit —
    /// pitch-preserved at render time (rubberband); the live preview
    /// approximates with a rate change (slightly pitched, like speed
    /// previews — documented).
    #[serde(default)]
    pub fit: bool,
}

impl Default for Music {
    fn default() -> Self {
        Self { source: String::new(), start: 0.0, gain_db: -12.0, duck: true, fade: 1.0, fit: false }
    }
}

/// One V1 clip's coordinates in both clocks — see `Project::edit_spans`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditSpan {
    pub clip: u64,
    /// Timeline window.
    pub t0: f64,
    pub t1: f64,
    /// Edit-time window (overlaps its predecessor by `fade_in`).
    pub e0: f64,
    pub e1: f64,
    pub fade_in: f64,
}

/// One audio-track (or overlay) clip, flattened for the export mix.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioClip {
    pub source: String,
    /// TIMELINE position.
    pub at: f64,
    pub in_point: f64,
    pub duration: f64,
    pub gain_db: f32,
    pub fade_in: f64,
    pub fade_out: f64,
    pub speed: f32,
    pub audio: AudioFx,
}

/// One overlay placement, flattened for rendering.
#[derive(Clone, Debug, PartialEq)]
pub struct OverlaySegment {
    pub source: String,
    pub in_point: f64,
    pub duration: f64,
    /// Where it appears on the TIMELINE.
    pub at: f64,
    pub pip: Pip,
    pub gain_db: f32,
    /// The clip's effect stack — chroma key included, which is how a
    /// green-screen inset composites over the cut.
    pub effects: Effects,
    /// Animated parameters (PipX/PipY/PipScale/Opacity), clip-local time.
    pub keys: Vec<(Param, Vec<Keyframe>)>,
    /// Adjustment layer: apply `effects` to what's below, draw nothing.
    pub adjustment: bool,
}

impl OverlaySegment {
    /// Placement and opacity at overlay-local time `t`.
    pub fn animated(&self, t: f64) -> (Pip, f32) {
        let mut pip = self.pip;
        let mut opacity = 1.0f32;
        for (p, keys) in &self.keys {
            let Some(v) = eval_keys(keys, t) else { continue };
            match p {
                Param::PipX => pip.x = v,
                Param::PipY => pip.y = v,
                Param::PipScale => pip.scale = v.clamp(0.02, 1.0),
                Param::Opacity => opacity = v.clamp(0.0, 1.0),
                _ => {}
            }
        }
        (pip, opacity)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub name: String,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
    pub muted: bool,
    /// Track-level level trim, dB — applies to every clip on the track, in
    /// the live mix and the export alike.
    #[serde(default)]
    pub gain_db: f32,
    /// Solo: when ANY track is soloed, only soloed tracks sound.
    #[serde(default)]
    pub solo: bool,
    /// Track-level stereo balance, folded into every clip's pan.
    #[serde(default)]
    pub pan: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub tracks: Vec<Track>,
    /// Captions for the edit, in TIMELINE time. Generated locally; burned in
    /// at export. `serde(default)` so older documents still load.
    #[serde(default)]
    pub captions: Vec<crate::captions::Cue>,
    #[serde(default = "default_caption_size")]
    pub caption_size: u32,
    /// Text placed on the picture by hand, in TIMELINE time.
    #[serde(default)]
    pub titles: Vec<crate::titles::Title>,
    /// An optional music bed under the whole edit.
    #[serde(default)]
    pub music: Option<Music>,
    /// Room tone: a sampled slice of the recording's own silence, looped
    /// quietly under the whole edit so cuts never drop to digital black.
    #[serde(default)]
    pub roomtone: Option<Roomtone>,
    /// Timeline positions flagged by the user. Part of the document: a
    /// marker you dropped yesterday should still be there today.
    #[serde(default)]
    pub markers: Vec<f64>,
    /// Names for markers, attached by TIME (not index — markers get
    /// sorted and appended to; an index pairing would silently shuffle).
    /// A marker without an entry here is just a flag.
    #[serde(default)]
    pub marker_labels: Vec<(f64, String)>,
    /// The .cube files this project grades with; clips reference them by
    /// index (`Effects.lut`).
    #[serde(default)]
    pub luts: Vec<String>,
    /// WGSL effect-plugin files this project uses; clips reference them by
    /// index (`Effects.plugin`), like LUTs.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// The media pool: everything gathered for this edit, on the timeline
    /// or not, organised into bins.
    #[serde(default)]
    pub pool: Vec<PoolItem>,
    /// Where the user WAS: playhead, zoom, selection — written on every
    /// autosave, restored on open, so a crash (or just closing the lid)
    /// costs nothing. Part of the document, like every real NLE.
    #[serde(default)]
    pub session: Session,
    /// Multicam angles: sources aligned against the timeline. `offset` is
    /// the TIMELINE time at which the angle's source t=0 falls, so the
    /// source time under playhead t is `t - offset`.
    #[serde(default)]
    pub multicam: Vec<Angle>,
    #[serde(skip)]
    next_id: u64,
}

/// One gathered piece of media.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PoolItem {
    pub path: String,
    /// Bin name; empty = the top level.
    #[serde(default)]
    pub bin: String,
}

/// The editor's resume point, saved with the project.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub playhead: f64,
    #[serde(default)]
    pub zoom: f32,
    #[serde(default)]
    pub scroll_x: f32,
    #[serde(default)]
    pub selected: Option<u64>,
}

/// The room-tone bed: a wav sampled from the quietest stretch of the
/// footage itself (never synthetic noise), looped under everything.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Roomtone {
    pub source: String,
    /// Level, dB. Room tone sits far below dialogue by nature; the default
    /// plays it exactly as sampled (0 = as recorded).
    #[serde(default)]
    pub gain_db: f32,
}

/// One multicam angle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Angle {
    pub source: String,
    /// Timeline time where this angle's source starts (t=0). Negative when
    /// the angle started rolling before the timeline's zero.
    pub offset: f64,
}

fn one() -> f32 {
    1.0
}

fn default_caption_size() -> u32 {
    20
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled".into(),
            fps: 30.0,
            width: 1920,
            height: 1080,
            tracks: vec![
                Track { id: 1, name: "V1".into(), kind: TrackKind::Video, clips: vec![], muted: false, gain_db: 0.0, solo: false, pan: 0.0 },
                Track { id: 2, name: "A1".into(), kind: TrackKind::Audio, clips: vec![], muted: false, gain_db: 0.0, solo: false, pan: 0.0 },
            ],
            captions: Vec::new(),
            caption_size: default_caption_size(),
            titles: Vec::new(),
            music: None,
            roomtone: None,
            markers: Vec::new(),
            marker_labels: Vec::new(),
            luts: Vec::new(),
            plugins: Vec::new(),
            pool: Vec::new(),
            session: Session::default(),
            multicam: Vec::new(),
            next_id: 100,
        }
    }
}

impl Project {
    /// Total timeline length = the furthest clip end across all tracks.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end())
            .fold(0.0, f64::max)
    }

    /// Append a clip to the end of the first video track (v0.1 "add to timeline").
    pub fn append_video(&mut self, name: &str, source: &str, duration: f64) {
        self.append(TrackKind::Video, name, source, duration);
    }

    /// Append a clip to the end of the first audio track.
    pub fn append_audio(&mut self, name: &str, source: &str, duration: f64) {
        self.append(TrackKind::Audio, name, source, duration);
    }

    fn append(&mut self, kind: TrackKind, name: &str, source: &str, duration: f64) {
        let at = self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end())
            .fold(0.0, f64::max);
        let id = self.add_clip(source, kind, at, 0.0, duration);
        if let Some(c) = self.clip_mut(id) {
            c.name = name.into();
        }
    }

    /// Place a window of a source file on a track at an exact position, and
    /// return the new clip's id. The general form the CLI drives; `append`
    /// is the "put it after the last one" convenience over it.
    pub fn add_clip(
        &mut self,
        source: &str,
        kind: TrackKind,
        at: f64,
        in_point: f64,
        duration: f64,
    ) -> u64 {
        let name = std::path::Path::new(source)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| source.to_string());
        let id = self.next_id;
        self.next_id += 1;
        self.ensure_track(kind);
        if let Some(track) = self.tracks.iter_mut().find(|t| t.kind == kind) {
            track.clips.push(Clip {
                id,
                name,
                source: source.into(),
                start: at.max(0.0),
                in_point: in_point.max(0.0),
                duration,
                effects: Default::default(),
                transition_in: 0.0,
                transition_kind: TransitionKind::default(),
                gain_db: 0.0,
                speed: 1.0,
                pip: Pip::default(),
                keys: Vec::new(),
                audio: AudioFx::default(),
                adjustment: false,
                nested: None,
                raw_filter: None,
                stabilize: false,
            });
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        id
    }

    /// The clip immediately BEFORE `id` on its own track, if they touch
    /// (within tolerance) — the adjacency roll and slide need.
    fn touching_prev(&self, id: u64) -> Option<u64> {
        let (c, kind) = self.clip_with_kind(id)?;
        self.tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| t.clips.iter())
            .find(|p| (p.end() - c.start).abs() < 0.02 && p.id != id)
            .map(|p| p.id)
    }

    fn touching_next(&self, id: u64) -> Option<u64> {
        let (c, kind) = self.clip_with_kind(id)?;
        self.tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| t.clips.iter())
            .find(|n| (n.start - c.end()).abs() < 0.02 && n.id != id)
            .map(|n| n.id)
    }

    /// SLIP: move the clip's window through its SOURCE without moving the
    /// clip on the timeline. The cut points stay; what plays between them
    /// changes. Returns the amount actually slipped (clamped at the
    /// source's start).
    pub fn slip(&mut self, id: u64, by: f64) -> f64 {
        let Some(c) = self.clip_mut(id) else { return 0.0 };
        let applied = if c.in_point + by < 0.0 { -c.in_point } else { by };
        c.in_point += applied;
        applied
    }

    /// ROLL: move the cut between this clip and the one it touches on its
    /// left. One gets longer, the other shorter; the total length of the
    /// timeline does not change. Returns the amount actually rolled.
    pub fn roll(&mut self, id: u64, by: f64) -> f64 {
        const MIN: f64 = 0.05;
        let Some(prev_id) = self.touching_prev(id) else { return 0.0 };
        let (c, _) = self.clip_with_kind(id).unwrap();
        let (p, _) = self.clip_with_kind(prev_id).unwrap();
        // Clamp: neither side may vanish, and this clip's in_point can't go
        // below the source's start.
        let lo = (-(p.duration - MIN)).max(-c.in_point / c.speed.max(0.01) as f64);
        let hi = c.duration - MIN;
        let by = by.clamp(lo, hi);
        if by.abs() < 1e-9 {
            return 0.0;
        }
        let rate = c.speed.max(0.01) as f64;
        if let Some(p) = self.clip_mut(prev_id) {
            p.duration += by;
        }
        if let Some(c) = self.clip_mut(id) {
            c.start += by;
            c.in_point += by * rate;
            c.duration -= by;
        }
        by
    }

    /// SLIDE: move this clip along the timeline while its neighbours absorb
    /// the motion — the previous clip stretches, the next one trims from its
    /// head. The three clips' combined span is unchanged. Returns the amount
    /// actually slid.
    pub fn slide(&mut self, id: u64, by: f64) -> f64 {
        const MIN: f64 = 0.05;
        let (Some(prev_id), Some(next_id)) = (self.touching_prev(id), self.touching_next(id))
        else {
            return 0.0;
        };
        let (p, _) = self.clip_with_kind(prev_id).unwrap();
        let (n, _) = self.clip_with_kind(next_id).unwrap();
        let lo = (-(p.duration - MIN)).max(-self.clip(id).map(|c| c.start).unwrap_or(0.0));
        let hi = (n.duration - MIN).min(
            // The next clip's in_point can't go below its source start when
            // sliding left... (sliding RIGHT consumes the next clip's head).
            f64::INFINITY,
        );
        let by = by.clamp(lo, hi);
        if by.abs() < 1e-9 {
            return 0.0;
        }
        let n_rate = self.clip(next_id).map(|c| c.speed.max(0.01) as f64).unwrap_or(1.0);
        if let Some(p) = self.clip_mut(prev_id) {
            p.duration += by;
        }
        if let Some(c) = self.clip_mut(id) {
            c.start += by;
        }
        if let Some(n) = self.clip_mut(next_id) {
            n.start += by;
            n.in_point += by * n_rate;
            n.duration -= by;
        }
        by
    }

    /// Remove the quiet air from the edit: every span where the source's
    /// audio envelope stays below `threshold` (a fraction of that source's
    /// own peak) for at least `min_gap` seconds is cut out — keeping `pad`
    /// seconds on each side so words never clip — and the timeline closes
    /// up behind it. The podcast jump-cut, in one call.
    ///
    /// `peaks_for` supplies each source's envelope (waveform buckets/sec is
    /// the caller's contract); returns (cuts made, seconds removed).
    pub fn tighten(
        &mut self,
        peaks_for: &mut dyn FnMut(&str) -> Option<(Vec<f32>, f64)>,
        threshold: f32,
        min_gap: f64,
        pad: f64,
    ) -> (usize, f64) {
        // Collect the TIMELINE windows to remove, clip by clip, before
        // touching anything — cutting while scanning would shift the map.
        let mut holes: Vec<(f64, f64)> = Vec::new();
        let clips: Vec<Clip> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| t.clips.iter().cloned())
            .collect();
        for c in &clips {
            let Some((peaks, per_sec)) = peaks_for(&c.source) else { continue };
            if peaks.is_empty() {
                continue;
            }
            let bucket = 1.0 / per_sec;
            let mut run_start: Option<f64> = None;
            let src_from = c.in_point;
            let src_to = c.in_point + c.source_len();
            let i0 = (src_from * per_sec) as usize;
            let i1 = ((src_to * per_sec) as usize).min(peaks.len());
            for i in i0..=i1 {
                let quiet = i < i1 && peaks.get(i).map(|p| *p < threshold).unwrap_or(false);
                match (quiet, run_start) {
                    (true, None) => run_start = Some(i as f64 * bucket),
                    (false, Some(s0)) => {
                        let s1 = i as f64 * bucket;
                        if s1 - s0 >= min_gap + 2.0 * pad {
                            // Trim the pad off each side, map to timeline.
                            let (a, b) = (s0 + pad, s1 - pad);
                            let t0 = c.start + c.output_time_for_source((a - c.in_point).max(0.0));
                            let t1 = c.start + c.output_time_for_source((b - c.in_point).max(0.0));
                            if t1 - t0 > 0.05 {
                                holes.push((t0, t1));
                            }
                        }
                        run_start = None;
                    }
                    _ => {}
                }
            }
        }
        self.cut_holes(holes)
    }

    /// Remove a set of TIMELINE windows and close up — the machinery behind
    /// `tighten` and filler-word removal. Cuts from the END backwards so
    /// earlier hole positions stay valid. Returns (cuts, seconds removed).
    pub fn cut_holes(&mut self, mut holes: Vec<(f64, f64)>) -> (usize, f64) {
        if holes.is_empty() {
            return (0, 0.0);
        }
        holes.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Merge overlapping holes so a window is never cut twice.
        let mut merged: Vec<(f64, f64)> = Vec::new();
        for (a, b) in holes {
            match merged.last_mut() {
                Some((_, e)) if a <= *e + 0.005 => *e = e.max(b),
                _ => merged.push((a, b)),
            }
        }
        // Cut from the END backwards so earlier hole positions stay valid.
        let mut removed = 0.0;
        let mut cuts = 0;
        for (t0, t1) in merged.iter().rev() {
            self.drop_annotations_in(*t0, *t1);
            self.split_at(*t0);
            self.split_at(*t1);
            // Everything fully inside [t0, t1] goes; the ripple closes up.
            let doomed: Vec<u64> = self
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .filter(|c| c.start >= t0 - 0.01 && c.end() <= t1 + 0.01)
                .map(|c| c.id)
                .collect();
            for id in doomed {
                self.delete_clip(id);
            }
            // Close the hole on every track — annotations ride along.
            let gap = t1 - t0;
            for track in &mut self.tracks {
                for c in &mut track.clips {
                    if c.start >= t1 - 0.01 {
                        c.start -= gap;
                    }
                }
                track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
            }
            self.shift_annotations(*t1, -gap);
            removed += gap;
            cuts += 1;
        }
        (cuts, removed)
    }

    /// The timeline windows occupied by filler words, ready for
    /// `cut_holes`. `cues` are WORD-level cues in SOURCE time of `source`;
    /// `words` is the filler list (lowercase). Pure — the whisper half
    /// lives in captions; this half is unit-tested.
    pub fn filler_holes(
        &self,
        source: &str,
        cues: &[crate::captions::Cue],
        words: &[String],
        pad: f64,
    ) -> Vec<(f64, f64)> {
        let mut holes = Vec::new();
        for cue in cues {
            let word: String = cue
                .text
                .trim()
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .collect();
            if word.is_empty() || !words.iter().any(|w| *w == word) {
                continue;
            }
            for (t0, t1) in
                self.map_source_window(source, (cue.start - pad).max(0.0), cue.end + pad)
            {
                if t1 - t0 > 0.02 {
                    holes.push((t0, t1));
                }
            }
        }
        holes
    }

    /// Put a file in the media pool (deduplicated by path).
    pub fn pool_add(&mut self, path: &str, bin: &str) {
        if path.is_empty() {
            return; // adjustment layers have no media
        }
        if let Some(item) = self.pool.iter_mut().find(|i| i.path == path) {
            if !bin.is_empty() {
                item.bin = bin.to_string();
            }
            return;
        }
        self.pool.push(PoolItem { path: path.to_string(), bin: bin.to_string() });
    }

    /// Every clip source lands in the pool automatically — the pool is the
    /// union of what was gathered and what is used.
    pub fn absorb_sources_into_pool(&mut self) {
        let sources: Vec<String> = self
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.source.clone())
            .chain(self.music.iter().map(|m| m.source.clone()))
            .collect();
        for s in sources {
            self.pool_add(&s, "");
        }
    }

    /// Repoint every reference from `from` to `to` — clips, pool, music,
    /// multicam. `from` may be a file OR a directory prefix, so a whole
    /// moved folder relinks in one call. Returns how many references moved.
    pub fn relink(&mut self, from: &str, to: &str) -> usize {
        let map = |p: &mut String| -> bool {
            if p == from {
                *p = to.to_string();
                return true;
            }
            // Directory move: prefix swap, but only at a path boundary.
            let prefix = format!("{}/", from.trim_end_matches('/'));
            if let Some(rest) = p.strip_prefix(&prefix) {
                *p = format!("{}/{}", to.trim_end_matches('/'), rest);
                return true;
            }
            false
        };
        let mut n = 0;
        for t in &mut self.tracks {
            for c in &mut t.clips {
                if map(&mut c.source) {
                    n += 1;
                }
            }
        }
        for item in &mut self.pool {
            if map(&mut item.path) {
                n += 1;
            }
        }
        if let Some(m) = &mut self.music {
            if map(&mut m.source) {
                n += 1;
            }
        }
        for a in &mut self.multicam {
            if map(&mut a.source) {
                n += 1;
            }
        }
        n
    }

    /// Multicam: cut to `angle` (index) at timeline time `t`. Splits V1 at
    /// the playhead and repoints everything AFTER the cut (up to the next
    /// clip boundary) at the angle's source, keeping timeline time
    /// continuous — the classic switcher cut. No-op when t misses V1 or the
    /// angle's source has nothing under t.
    pub fn cut_to_angle(&mut self, t: f64, angle: usize) -> bool {
        let Some(a) = self.multicam.get(angle).cloned() else { return false };
        let src_t = t - a.offset;
        if src_t < 0.0 {
            return false;
        }
        // Which V1 clip is under the playhead?
        let Some(cur) = self.clip_at(TrackKind::Video, t).map(|c| c.id) else { return false };
        // Already on this angle with matching timing? Nothing to do.
        if let Some(c) = self.clip(cur) {
            let cur_src_at_t = c.in_point + c.source_offset_at(t - c.start);
            if c.source == a.source && (cur_src_at_t - src_t).abs() < 0.02 {
                return false;
            }
        }
        self.split_at(t);
        // The clip now STARTING at t (the right half) switches source.
        let Some(next) = self
            .tracks
            .iter_mut()
            .filter(|tr| tr.kind == TrackKind::Video)
            .flat_map(|tr| tr.clips.iter_mut())
            .find(|c| (c.start - t).abs() < 0.005)
        else {
            return false;
        };
        next.source = a.source.clone();
        next.in_point = src_t;
        next.name = std::path::Path::new(&a.source)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| a.source.clone());
        true
    }

    /// The name attached to the marker at `t`, if any.
    pub fn marker_label(&self, t: f64) -> Option<&str> {
        self.marker_labels
            .iter()
            .find(|(lt, _)| (lt - t).abs() < 0.01)
            .map(|(_, l)| l.as_str())
    }

    /// Name (or rename) the marker at `t`.
    pub fn set_marker_label(&mut self, t: f64, label: &str) {
        if let Some(e) = self.marker_labels.iter_mut().find(|(lt, _)| (lt - t).abs() < 0.01) {
            e.1 = label.to_string();
        } else {
            self.marker_labels.push((t, label.to_string()));
        }
    }

    /// Register an effect plugin file (deduplicated) and return its index.
    pub fn add_plugin(&mut self, path: &str) -> u32 {
        if let Some(i) = self.plugins.iter().position(|p| p == path) {
            return i as u32;
        }
        self.plugins.push(path.to_string());
        (self.plugins.len() - 1) as u32
    }

    pub fn plugin_path(&self, idx: u32) -> Option<&str> {
        self.plugins.get(idx as usize).map(String::as_str)
    }

    /// Register a LUT file (deduplicated) and return its index.
    pub fn add_lut(&mut self, path: &str) -> u32 {
        if let Some(i) = self.luts.iter().position(|p| p == path) {
            return i as u32;
        }
        self.luts.push(path.to_string());
        (self.luts.len() - 1) as u32
    }

    pub fn lut_path(&self, idx: u32) -> Option<&str> {
        self.luts.get(idx as usize).map(String::as_str)
    }

    /// The edit's spans: every V1 clip's place in BOTH clocks — timeline
    /// time (where the clip sits, gaps and all) and EDIT time (what the
    /// render plays: gaps skipped, transition overlaps collapsed). This is
    /// the one-truth-of-time mapping; the scrubber, the time readout and
    /// playback continuity all read it, and its total must equal
    /// `render_duration` to the bit — tested.
    pub fn edit_spans(&self) -> Vec<EditSpan> {
        let mut clips: Vec<&Clip> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        let mut spans = Vec::with_capacity(clips.len());
        let mut cursor = 0.0f64;
        let mut prev_dur = 0.0f64;
        for (i, c) in clips.iter().enumerate() {
            let d = if i == 0 {
                0.0
            } else {
                c.transition_in.min(prev_dur).min(c.duration)
            };
            let e0 = (cursor - d).max(0.0);
            spans.push(EditSpan {
                clip: c.id,
                t0: c.start,
                t1: c.end(),
                e0,
                e1: e0 + c.duration,
                fade_in: d,
            });
            cursor = e0 + c.duration;
            prev_dur = c.duration;
        }
        spans
    }

    /// Total edit (render) length. Equals `render_duration(export_segments())`.
    pub fn edit_len(&self) -> f64 {
        self.edit_spans().last().map(|s| s.e1).unwrap_or(0.0)
    }

    /// Timeline → edit time. Inside a clip it is a shift; inside a gap it
    /// collapses to the next clip's entry (the gap does not exist in the
    /// edit); past the end it is the edit's end.
    pub fn timeline_to_edit(&self, t: f64) -> f64 {
        let spans = self.edit_spans();
        if spans.is_empty() {
            return 0.0;
        }
        for s in &spans {
            if t < s.t0 {
                return s.e0; // in the gap before this clip
            }
            if t < s.t1 {
                return s.e0 + (t - s.t0);
            }
        }
        spans.last().map(|s| s.e1).unwrap_or(0.0)
    }

    /// Edit → timeline time. During a transition overlap two timeline
    /// moments share one edit moment; the INCOMING clip wins, matching how
    /// the render treats the overlap as the new clip's head.
    pub fn edit_to_timeline(&self, e: f64) -> f64 {
        let spans = self.edit_spans();
        if spans.is_empty() {
            return 0.0;
        }
        let mut best = spans[0].t0;
        for s in &spans {
            if e >= s.e0 && e <= s.e1 {
                best = s.t0 + (e - s.e0); // later spans overwrite: incoming wins
            }
        }
        if e > spans.last().unwrap().e1 {
            best = spans.last().unwrap().t1;
        }
        best
    }

    /// Make sure a track of this kind exists, creating it if not. Overlay
    /// tracks are made on demand so a project that never uses one never shows
    /// an empty lane.
    pub fn ensure_track(&mut self, kind: TrackKind) {
        if self.tracks.iter().any(|t| t.kind == kind) {
            return;
        }
        let id = self.tracks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let name = match kind {
            TrackKind::Video => "V1",
            TrackKind::Overlay => "V2",
            TrackKind::Audio => "A1",
        };
        let track = Track {
            id,
            name: name.into(),
            kind,
            clips: vec![],
            muted: false,
            gain_db: 0.0,
            solo: false,
            pan: 0.0,
        };
        // Overlays sit above the base video, audio below — the order lanes
        // are drawn in.
        match kind {
            TrackKind::Overlay => self.tracks.insert(0, track),
            _ => self.tracks.push(track),
        }
    }

    /// Overlay clips, in timeline order — everything composited on top of the
    /// cut. Timeline positions, not sequence positions: an overlay appears at
    /// the moment it sits at, and the rest of the edit doesn't move for it.
    pub fn overlay_segments(&self) -> Vec<OverlaySegment> {
        let mut out: Vec<OverlaySegment> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Overlay && !t.muted)
            .flat_map(|t| t.clips.iter())
            .map(|c| OverlaySegment {
                source: c.source.clone(),
                in_point: c.in_point,
                duration: c.duration,
                at: c.start,
                pip: c.pip,
                gain_db: c.gain_db,
                effects: c.effects,
                keys: c.keys.clone(),
                adjustment: c.adjustment,
            })
            .collect();
        out.sort_by(|a, b| a.at.total_cmp(&b.at));
        out
    }

    /// Clips on AUDIO tracks (voice-over, sound effects), flattened for the
    /// export mix: each plays at its TIMELINE position with its own gain and
    /// fades. Overlay clips' audio rides along too — a PiP's sound belongs
    /// in the mix just like its picture belongs on screen.
    pub fn audio_clips(&self) -> Vec<AudioClip> {
        let soloing = self.tracks.iter().any(|t| t.solo);
        let mut out: Vec<AudioClip> = self
            .tracks
            .iter()
            .filter(|t| {
                matches!(t.kind, TrackKind::Audio | TrackKind::Overlay)
                    && !t.muted
                    && (!soloing || t.solo)
            })
            .flat_map(|t| t.clips.iter().map(move |c| (t.gain_db, t.pan, c)))
            .filter(|(_, _, c)| !c.adjustment && !c.source.is_empty())
            .map(|(track_gain, track_pan, c)| AudioClip {
                source: c.source.clone(),
                at: c.start,
                in_point: c.in_point,
                duration: c.duration,
                // Track trim and clip trim compose in dB.
                gain_db: c.gain_db + track_gain,
                fade_in: c.effects.fade_in,
                fade_out: c.effects.fade_out,
                speed: c.speed,
                audio: c.audio.with_track_pan(track_pan),
            })
            .collect();
        out.sort_by(|a, b| a.at.total_cmp(&b.at));
        out
    }

    /// The V1 track's own audio state for the export: (gain dB, silenced) —
    /// silenced when V1 is muted, or when a solo elsewhere excludes it.
    pub fn video_audio_state(&self) -> (f32, bool) {
        let soloing = self.tracks.iter().any(|t| t.solo);
        self.tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .map(|t| (t.gain_db, t.muted || (soloing && !t.solo)))
            .unwrap_or((0.0, false))
    }

    /// The caption showing at timeline time `t`, if any.
    pub fn caption_at(&self, t: f64) -> Option<&crate::captions::Cue> {
        self.captions.iter().find(|c| c.start <= t && t < c.end)
    }

    pub fn clip(&self, id: u64) -> Option<&Clip> {
        self.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == id)
    }

    pub fn clip_mut(&mut self, id: u64) -> Option<&mut Clip> {
        self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == id)
    }

    /// The clip on the given kind of track under timeline time `t`.
    pub fn clip_at(&self, kind: TrackKind, t: f64) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|tr| tr.kind == kind)
            .flat_map(|tr| tr.clips.iter())
            .find(|c| c.start <= t && t < c.end())
    }

    /// The clip that ends at or before `t` on this track — the one a
    /// crossfade would come from.
    pub fn clip_before(&self, kind: TrackKind, t: f64) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|tr| tr.kind == kind)
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.end() <= t + 1e-6)
            .max_by(|a, b| a.end().total_cmp(&b.end()))
    }

    /// The next clip (by start) on the given track kind strictly after `t`.
    pub fn clip_after(&self, kind: TrackKind, t: f64) -> Option<&Clip> {
        self.tracks
            .iter()
            .filter(|tr| tr.kind == kind)
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.start > t + 1e-9)
            .min_by(|a, b| a.start.total_cmp(&b.start))
    }

    /// Split every clip containing timeline time `t` into two at `t`.
    /// Returns how many clips were split.
    pub fn split_at(&mut self, t: f64) -> usize {
        const MIN: f64 = 0.05;
        let mut split = 0;
        let mut new_id = self.next_id;
        for track in &mut self.tracks {
            let mut additions = Vec::new();
            for clip in &mut track.clips {
                if clip.start + MIN < t && t < clip.end() - MIN {
                    let cut = t - clip.start; // offset into the clip, timeline time
                    let mut right = clip.clone();
                    right.id = new_id;
                    new_id += 1;
                    right.start = t;
                    // The source advances faster than the timeline on a
                    // sped-up clip, so the cut point has to be scaled.
                    right.in_point = clip.in_point + cut * clip.speed.max(0.01) as f64;
                    right.duration = clip.duration - cut;
                    clip.duration = cut;
                    additions.push(right);
                    split += 1;
                }
            }
            track.clips.extend(additions);
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        self.next_id = new_id;
        split
    }

    /// Slide `id` left until it touches the clip before it, closing the gap.
    /// Returns the seconds removed. Clips after it are NOT moved — use
    /// `close_all_gaps` for a ripple.
    pub fn close_gap_before(&mut self, id: u64) -> f64 {
        let Some(clip) = self.clip(id) else { return 0.0 };
        let (kind, start) = (
            self.tracks
                .iter()
                .find(|t| t.clips.iter().any(|c| c.id == id))
                .map(|t| t.kind.clone())
                .unwrap_or(TrackKind::Video),
            clip.start,
        );
        let prev_end = self
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.id != id && c.end() <= start + 1e-6)
            .map(|c| c.end())
            .fold(0.0f64, f64::max);
        let gap = start - prev_end;
        if gap <= 1e-6 {
            return 0.0;
        }
        if let Some(c) = self.clip_mut(id) {
            c.start -= gap;
        }
        gap
    }

    /// Butt every clip on every track up against its predecessor: no gaps
    /// anywhere, order preserved. Returns the total seconds removed.
    pub fn close_all_gaps(&mut self) -> f64 {
        let mut removed = 0.0;
        for track in &mut self.tracks {
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
            let mut cursor = 0.0f64;
            for clip in &mut track.clips {
                let gap = clip.start - cursor;
                if gap > 1e-6 {
                    clip.start -= gap;
                    removed += gap;
                }
                cursor = clip.end();
            }
        }
        removed
    }

    /// Shift every clip that starts at or after `from` left by `amount`, on
    /// EVERY track — so video and audio stay in sync through a ripple.
    fn ripple_from(&mut self, from: f64, amount: f64) {
        if amount <= 0.0 {
            return;
        }
        for track in &mut self.tracks {
            for clip in &mut track.clips {
                if clip.start >= from - 1e-6 {
                    clip.start = (clip.start - amount).max(0.0);
                }
            }
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        self.shift_annotations(from, -amount);
    }

    /// Markers, captions and titles live in TIMELINE time — an edit that
    /// moves the timeline under them must carry them along, or a caption
    /// timed to a word drifts onto the wrong shot. Everything at or after
    /// `from` moves by `delta` (never past zero); a span whose START is
    /// before `from` keeps its start (its tail stretches/shrinks with the
    /// material under it).
    pub fn shift_annotations(&mut self, from: f64, delta: f64) {
        if delta.abs() < 1e-9 {
            return;
        }
        for m in &mut self.markers {
            if *m >= from - 1e-6 {
                *m = (*m + delta).max(0.0);
            }
        }
        for (t, _) in &mut self.marker_labels {
            if *t >= from - 1e-6 {
                *t = (*t + delta).max(0.0);
            }
        }
        for c in &mut self.captions {
            if c.start >= from - 1e-6 {
                c.start = (c.start + delta).max(0.0);
                c.end = (c.end + delta).max(c.start + 0.05);
            } else if c.end > from {
                c.end = (c.end + delta).max(c.start + 0.05);
            }
        }
        for t in &mut self.titles {
            if t.start >= from - 1e-6 {
                t.start = (t.start + delta).max(0.0);
                t.end = (t.end + delta).max(t.start + 0.05);
            } else if t.end > from {
                t.end = (t.end + delta).max(t.start + 0.05);
            }
        }
    }

    /// ANCHORING through a plain clip drag: annotations sitting inside the
    /// clip's old span travel with it — the caption stays on the words, the
    /// marker on the beat. Windows are clamped at zero like the clip is.
    pub fn carry_annotations(&mut self, old_start: f64, old_end: f64, delta: f64) {
        if delta.abs() < 1e-9 {
            return;
        }
        let inside = |t: f64| t >= old_start - 1e-6 && t <= old_end + 1e-6;
        for m in &mut self.markers {
            if inside(*m) {
                *m = (*m + delta).max(0.0);
            }
        }
        for (t, _) in &mut self.marker_labels {
            if inside(*t) {
                *t = (*t + delta).max(0.0);
            }
        }
        for c in &mut self.captions {
            if inside(c.start) && inside(c.end) {
                c.start = (c.start + delta).max(0.0);
                c.end = (c.end + delta).max(c.start + 0.05);
            }
        }
        for t in &mut self.titles {
            if inside(t.start) && inside(t.end) {
                t.start = (t.start + delta).max(0.0);
                t.end = (t.end + delta).max(t.start + 0.05);
            }
        }
    }

    /// Drop annotations living entirely inside a removed window — used by
    /// the hole-cutting paths so a caption for trimmed-away speech dies
    /// with it instead of piling onto the join.
    fn drop_annotations_in(&mut self, t0: f64, t1: f64) {
        self.markers.retain(|m| !(*m >= t0 - 1e-6 && *m <= t1 + 1e-6));
        self.marker_labels.retain(|(t, _)| !(*t >= t0 - 1e-6 && *t <= t1 + 1e-6));
        self.captions.retain(|c| !(c.start >= t0 - 1e-6 && c.end <= t1 + 1e-6));
        self.titles.retain(|t| !(t.start >= t0 - 1e-6 && t.end <= t1 + 1e-6));
    }

    /// Remove a clip and close the hole behind it — Shift+Delete in every
    /// NLE. Clips on OTHER tracks lying inside the same span go too (video
    /// and audio arrive as linked pairs, so removing a shot must remove its
    /// sound); rippling one track while leaving another would overlap clips.
    /// Returns the seconds removed from the timeline.
    pub fn ripple_delete(&mut self, id: u64) -> f64 {
        let Some(clip) = self.clip(id) else { return 0.0 };
        let (start, end, duration) = (clip.start, clip.end(), clip.duration);
        for track in &mut self.tracks {
            track
                .clips
                .retain(|c| c.id != id && !(c.start >= start - 1e-6 && c.end() <= end + 1e-6));
        }
        self.ripple_from(end - 1e-9, duration);
        duration
    }

    /// Trim the clip under `t` back to the playhead and close the gap —
    /// Q (trim the head) and W (trim the tail) in Premiere's keymap.
    /// Returns the seconds removed.
    pub fn ripple_trim_to_playhead(&mut self, t: f64, head: bool) -> f64 {
        const MIN: f64 = 0.05;
        // Take the video track's clip as the reference for how much to remove,
        // then ripple every track by that amount so nothing drifts.
        let Some(clip) = self.clip_at(TrackKind::Video, t) else { return 0.0 };
        let (id, start, end) = (clip.id, clip.start, clip.end());
        let removed = if head { t - start } else { end - t };
        if removed <= MIN || (end - start) - removed < MIN {
            return 0.0;
        }
        // Apply the same trim to every track's clip under the playhead, so
        // picture and sound stay locked together.
        let ids: Vec<u64> = self
            .tracks
            .iter()
            .flat_map(|tr| tr.clips.iter())
            .filter(|c| c.start <= t && t < c.end())
            .map(|c| c.id)
            .collect();
        let _ = id;
        for cid in ids {
            if let Some(c) = self.clip_mut(cid) {
                if head {
                    let d = (t - c.start).min(c.duration - MIN).max(0.0);
                    c.in_point += d;
                    c.duration -= d;
                    c.start = t;
                } else {
                    let d = (c.end() - t).min(c.duration - MIN).max(0.0);
                    c.duration -= d;
                }
            }
        }
        if head {
            self.ripple_from(t - 1e-9, removed);
        } else {
            self.ripple_from(end - 1e-9, removed);
        }
        removed
    }

    /// Paste `clip` onto the track it came from at timeline position `at`,
    /// making room by pushing everything from that point along.
    ///
    /// Insert rather than overwrite: pasting should never silently eat
    /// footage you already placed. Rippling keeps the rest of the cut
    /// intact — the same behaviour as pasting a word into a sentence.
    pub fn paste_clip(&mut self, clip: &Clip, at: f64, kind: TrackKind) -> u64 {
        let at = at.max(0.0);
        // An insert edit, the way every NLE does it: split whatever straddles
        // the insertion point first, so no clip is left half-covered, then
        // open a gap on EVERY track. Shifting only the pasted clip's own
        // track would slide the audio out of sync with the picture.
        self.split_at(at);
        for track in &mut self.tracks {
            for c in &mut track.clips {
                if c.start >= at - 1e-6 {
                    c.start += clip.duration;
                }
            }
        }
        self.shift_annotations(at, clip.duration);

        let id = self.next_id;
        self.next_id += 1;
        let mut placed = clip.clone();
        placed.id = id;
        placed.start = at;
        // A crossfade describes a join with a particular neighbour; carrying
        // it to a new position would fade into whatever happens to be there.
        placed.transition_in = 0.0;
        if let Some(track) = self.tracks.iter_mut().find(|t| t.kind == kind) {
            track.clips.push(placed);
            track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        }
        id
    }

    /// Copy a clip and drop the copy straight after the original.
    pub fn duplicate_clip(&mut self, id: u64) -> Option<u64> {
        let (clip, kind) = self.tracks.iter().find_map(|t| {
            t.clips.iter().find(|c| c.id == id).map(|c| (c.clone(), t.kind))
        })?;
        Some(self.paste_clip(&clip, clip.end(), kind))
    }

    /// The clip with this id, and which kind of track it lives on.
    pub fn clip_with_kind(&self, id: u64) -> Option<(Clip, TrackKind)> {
        self.tracks.iter().find_map(|t| {
            t.clips.iter().find(|c| c.id == id).map(|c| (c.clone(), t.kind))
        })
    }

    pub fn delete_clip(&mut self, id: u64) -> bool {
        for track in &mut self.tracks {
            let before = track.clips.len();
            track.clips.retain(|c| c.id != id);
            if track.clips.len() != before {
                return true;
            }
        }
        false
    }

    /// Legal range for a clip's `start` when moving: between its neighbours
    /// on the same track (overlaps are not allowed).
    pub fn move_range(&self, id: u64) -> (f64, f64) {
        for track in &self.tracks {
            if let Some(clip) = track.clips.iter().find(|c| c.id == id) {
                let mut lo = 0.0f64;
                let mut hi = f64::INFINITY;
                for other in &track.clips {
                    if other.id == id {
                        continue;
                    }
                    if other.end() <= clip.start + 1e-9 {
                        lo = lo.max(other.end());
                    } else if other.start + 1e-9 >= clip.end() {
                        hi = hi.min(other.start - clip.duration);
                    }
                }
                return (lo, hi.max(lo));
            }
        }
        (0.0, f64::INFINITY)
    }

    /// Every interesting time to snap against: clip edges (excluding `skip`)
    /// and timeline zero.
    pub fn snap_targets(&self, skip: Option<u64>) -> Vec<f64> {
        let mut v = vec![0.0];
        for track in &self.tracks {
            for c in &track.clips {
                if Some(c.id) == skip {
                    continue;
                }
                v.push(c.start);
                v.push(c.end());
            }
        }
        v
    }

    /// Map a source-media position to timeline time via a clip of `source`,
    /// preferring the clip whose source window contains `pos`.
    pub fn source_to_timeline(&self, source: &str, pos: f64) -> Option<f64> {
        self.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.source == source)
            .find(|c| c.in_point <= pos && pos <= c.in_point + c.source_len())
            .map(|c| c.start + c.output_time_for_source(pos - c.in_point))
    }

    /// Map a window of SOURCE time onto every place it appears on the
    /// timeline, clipped to the clip that carries it.
    ///
    /// Captions are generated against the original recording, but the edit
    /// may have trimmed it, split it, reordered it, or used the same moment
    /// twice — so a source window can land in zero, one, or several places,
    /// and must never spill past the cut it belongs to.
    pub fn map_source_window(&self, source: &str, a: f64, b: f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for track in self.tracks.iter().filter(|t| t.kind == TrackKind::Video) {
            for c in &track.clips {
                if c.source != source {
                    continue;
                }
                let (cs, ce) = (c.in_point, c.in_point + c.source_len());
                let (lo, hi) = (a.max(cs), b.min(ce));
                if hi <= lo {
                    continue;
                }
                out.push((
                    c.start + c.output_time_for_source(lo - cs),
                    c.start + c.output_time_for_source(hi - cs),
                ));
            }
        }
        out.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// The edit flattened for export: V1 clips in timeline order as
    /// (source, in_point, duration). Gaps are collapsed — exactly how editor
    /// playback sequences the cut.
    pub fn export_segments(&self) -> Vec<Segment> {
        self.export_segments_range(None, None)
    }

    /// Like `export_segments`, restricted to the timeline window
    /// [`range_in`, `range_out`] — clips are cut at the boundaries so an
    /// in/out range exports exactly what the markers enclose.
    pub fn export_segments_range(
        &self,
        range_in: Option<f64>,
        range_out: Option<f64>,
    ) -> Vec<Segment> {
        let lo = range_in.unwrap_or(f64::NEG_INFINITY);
        let hi = range_out.unwrap_or(f64::INFINITY);
        let (v_gain, v_silent) = self.video_audio_state();
        let v_pan = self
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .map(|t| t.pan)
            .unwrap_or(0.0);
        let mut clips: Vec<&Clip> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video)
            .flat_map(|t| t.clips.iter())
            .collect();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        clips
            .into_iter()
            .filter_map(|c| {
                let start = c.start.max(lo);
                let end = c.end().min(hi);
                if end - start <= 0.01 {
                    return None; // outside the range (or a sliver)
                }
                let head = start - c.start; // trimmed off the clip's front
                Some(Segment {
                    source: c.source.clone(),
                    in_point: c.in_point + head * c.speed.max(0.01) as f64,
                    duration: end - start,
                    effects: c.effects,
                    // A clip cut into by the range marker loses its
                    // transition — there's no longer a clip to fade from.
                    transition_in: if head > 0.01 { 0.0 } else { c.transition_in },
                    transition_kind: c.transition_kind,
                    gain_db: if v_silent { -120.0 } else { c.gain_db + v_gain },
                    speed: c.speed,
                    keys: c.keys.clone(),
                    stabilize: c.stabilize,
                    audio: c.audio.with_track_pan(v_pan),
                    raw_filter: c.raw_filter.clone(),
                })
            })
            .collect()
    }

    pub fn save(&self, path: &str) -> anyhow::Result<()> {
        write_atomic(path, &serde_json::to_string_pretty(self)?)
    }

    pub fn load(path: &str) -> anyhow::Result<Self> {
        let mut p: Project = serde_json::from_str(&std::fs::read_to_string(path)?)?;
        // next_id is serde(skip); re-seed above every stored id.
        p.next_id = p
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .map(|c| c.id)
            .max()
            .unwrap_or(99)
            + 1;
        Ok(p)
    }
}

/// Per-session editor state: zoom/scroll, selection, drag-in-progress, the
/// timeline playhead (timeline seconds — NOT source seconds), and undo/redo
/// as whole-model snapshots (the model is small; snapshots are simple and
/// unbreakable).
pub struct EditorState {
    pub px_per_s: f32,
    pub scroll_x: f32,
    pub selected: Option<u64>,
    pub drag: Option<Drag>,
    /// Timeline position of the playhead, in seconds.
    pub playhead: f64,
    /// Clip currently feeding the preview during editor playback.
    pub active_clip: Option<u64>,
    /// Export range markers, in timeline seconds (I / O keys).
    pub range_in: Option<f64>,
    pub range_out: Option<f64>,
    pub dirty: bool,
    pub project_path: Option<String>,
    /// The clip whose effect sliders are mid-drag — one undo step per gesture.
    pub fx_gesture: Option<u64>,
    /// A bezier handle mid-drag in the curve editor: (key index, incoming?).
    pub curve_handle_drag: Option<(usize, bool)>,
    /// The full-width curve lane under the timeline.
    pub show_curve_lane: bool,
    /// Index of the title being edited, if any — it shows a box in the
    /// preview and is the one you can drag around.
    pub selected_title: Option<usize>,
    /// The copied clip, and which kind of track it came from.
    pub clipboard: Option<(Clip, TrackKind)>,
    /// Track TARGETING: paste lands on this track (clicked in the lane
    /// header) when its kind matches the copied clip's; None = the source
    /// track's kind, the old behaviour.
    pub target_track: Option<u64>,
    /// The parameter the keyframe controls in the side panel operate on.
    pub key_param: Param,
    /// The keyframe being dragged in the curve editor (index into the
    /// selected clip's track for `key_param`).
    pub curve_drag: Option<usize>,
    /// Additional selected clips (shift-click). `selected` stays the
    /// primary — the one the side panel edits.
    pub multi: std::collections::HashSet<u64>,
    /// When the project last changed — autosave waits for a quiet moment.
    pub changed_at: std::time::Instant,
    /// Have we told the user where the project is being saved? (Once only.)
    pub announced_path: bool,
    undo: Vec<Project>,
    redo: Vec<Project>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Drag {
    Move { id: u64, grab: f64 },
    TrimL { id: u64 },
    TrimR { id: u64 },
    /// Move the cut between this clip and its left neighbour (Ctrl+edge).
    Roll { id: u64, last: f64 },
    /// Move the clip's window through its source (Alt+body).
    Slip { id: u64, last: f64 },
    /// Move the clip; neighbours absorb (Ctrl+Alt+body).
    Slide { id: u64, last: f64 },
    Playhead,
    /// Shift+drag on empty space: rubber-band select. Screen-space origin.
    Lasso { x0: f32, y0: f32 },
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            px_per_s: 60.0,
            scroll_x: 0.0,
            selected: None,
            drag: None,
            playhead: 0.0,
            active_clip: None,
            range_in: None,
            range_out: None,
            dirty: false,
            project_path: None,
            fx_gesture: None,
            selected_title: None,
            clipboard: None,
            target_track: None,
            curve_handle_drag: None,
            show_curve_lane: false,
            key_param: Param::Exposure,
            curve_drag: None,
            multi: std::collections::HashSet::new(),
            changed_at: std::time::Instant::now(),
            announced_path: false,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

impl EditorState {
    /// Snapshot before a mutating operation.
    pub fn push_undo(&mut self, project: &Project) {
        self.undo.push(project.clone());
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.mark_changed();
    }

    /// Something about the project changed — autosave will pick it up.
    pub fn mark_changed(&mut self) {
        self.dirty = true;
        self.changed_at = std::time::Instant::now();
    }

    pub fn undo(&mut self, project: &mut Project) -> bool {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(project, prev));
            self.selected = None;
            self.mark_changed();
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, project: &mut Project) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(project, next));
            self.selected = None;
            self.mark_changed();
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Snap `t` to the nearest target within `tolerance` seconds.
    /// Returns (possibly snapped t, the target hit).
    pub fn snap(t: f64, targets: &[f64], tolerance: f64) -> (f64, Option<f64>) {
        let mut best: Option<f64> = None;
        for &target in targets {
            let d = (t - target).abs();
            if d <= tolerance && best.map_or(true, |b| d < (t - b).abs()) {
                best = Some(target);
            }
        }
        (best.unwrap_or(t), best)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_clip_project() -> Project {
        let mut p = Project::default();
        p.append_video("a", "/tmp/a.mp4", 10.0);
        p.append_audio("a", "/tmp/a.mp4", 10.0);
        p
    }

    /// Dragging a clip carries its annotations: a caption on its words and
    /// a marker on its beat travel with the move; annotations elsewhere
    /// hold still.
    #[test]
    fn dragging_a_clip_carries_its_annotations() {
        let mut p = one_clip_project(); // clip 0..10
        p.markers = vec![3.0, 12.0];
        p.captions.push(crate::captions::Cue { start: 2.0, end: 4.0, text: "on the clip".into() });
        // The clip moves 0..10 → 5..15.
        p.carry_annotations(0.0, 10.0, 5.0);
        assert_eq!(p.markers, vec![8.0, 12.0], "{:?}", p.markers);
        assert!((p.captions[0].start - 7.0).abs() < 1e-9);
        // Moving back left clamps at zero rather than going negative.
        p.carry_annotations(5.0, 15.0, -9.0);
        assert!(p.markers[0] >= 0.0 && (p.markers[0] - 0.0).abs() < 1e-9);
    }

    /// Bezier keys: exact at the ends, monotone for an ease, and the speed
    /// integral stays consistent with its own inverse — picture, sound and
    /// seeking share that one contract.
    #[test]
    fn bezier_keys_ease_and_integrate_consistently() {
        // eval: the standard ease-in-out starts slow, ends slow.
        let keys = vec![
            Keyframe { t: 0.0, value: 0.0, interp: Interp::bezier_default() },
            Keyframe { t: 1.0, value: 1.0, interp: Interp::Linear },
        ];
        assert_eq!(eval_keys(&keys, 0.0), Some(0.0));
        assert_eq!(eval_keys(&keys, 1.0), Some(1.0));
        let q1 = eval_keys(&keys, 0.25).unwrap();
        let mid = eval_keys(&keys, 0.5).unwrap();
        let q3 = eval_keys(&keys, 0.75).unwrap();
        assert!((mid - 0.5).abs() < 0.02, "symmetric ease crosses the middle: {mid}");
        assert!(q1 < 0.2, "slow start: {q1}");
        assert!(q3 > 0.8, "fast approach then settle: {q3}");
        // Monotone: no wobble with tame handles.
        let mut prev = -1.0f32;
        for i in 0..=40 {
            let v = eval_keys(&keys, i as f64 / 40.0).unwrap();
            assert!(v >= prev - 1e-4, "wobble at {i}");
            prev = v;
        }

        // speed ramp 1× → 3× over 2 s through a bezier: the integral must
        // match a fine numeric sum of eval_keys, and invert back.
        let ramp = vec![
            Keyframe { t: 0.0, value: 1.0, interp: Interp::bezier_default() },
            Keyframe { t: 2.0, value: 3.0, interp: Interp::Linear },
        ];
        let total = speed_integral(&ramp, 1.0, 2.0);
        let mut num = 0.0f64;
        let n = 4000;
        for i in 0..n {
            let t = (i as f64 + 0.5) / n as f64 * 2.0;
            num += eval_keys(&ramp, t).unwrap() as f64 * (2.0 / n as f64);
        }
        assert!((total - num).abs() < 0.01, "integral {total} vs numeric {num}");
        // Inverse: source position s maps back to the output time it came from.
        let s_at_1 = speed_integral(&ramp, 1.0, 1.0);
        let back = speed_integral_invert(&ramp, 1.0, s_at_1, 2.0);
        assert!((back - 1.0).abs() < 1e-3, "invert round-trip: {back}");
    }

    /// Markers, captions and titles must FOLLOW the material they annotate
    /// through ripples — a caption timed to a word may not drift onto the
    /// next shot when something upstream is cut or pasted in.
    #[test]
    fn annotations_ride_ripples_and_die_with_their_material() {
        let mut p = one_clip_project(); // 10 s
        p.markers = vec![1.0, 5.0, 8.0];
        p.set_marker_label(5.0, "beat");
        p.captions.push(crate::captions::Cue { start: 4.8, end: 5.4, text: "hello".into() });
        p.titles.push(crate::titles::Title {
            text: "T".into(), start: 7.5, end: 9.0,
            ..Default::default()
        });

        // Cut 2..4 out (ripple): everything after 4 s moves 2 s left;
        // everything before 2 s stays.
        p.cut_holes(vec![(2.0, 4.0)]);
        assert_eq!(p.markers, vec![1.0, 3.0, 6.0], "{:?}", p.markers);
        assert_eq!(p.marker_label(3.0), Some("beat"));
        assert!((p.captions[0].start - 2.8).abs() < 1e-6);
        assert!((p.titles[0].start - 5.5).abs() < 1e-6);

        // A marker and caption INSIDE a removed window die with it.
        p.cut_holes(vec![(2.5, 3.5)]); // takes the 3.0 marker + caption
        assert_eq!(p.markers, vec![1.0, 5.0], "{:?}", p.markers);
        assert!(p.captions.is_empty(), "the caption's speech is gone — so is it");
        assert_eq!(p.marker_label(5.0), None, "its label went with it");

        // Paste opens a gap: annotations after the insert move right.
        let donor = p.tracks[0].clips[0].clone();
        let kind = TrackKind::Video;
        p.paste_clip(&donor, 2.0, kind);
        let d = donor.duration;
        assert_eq!(p.markers, vec![1.0, 5.0 + d], "{:?}", p.markers);
    }

    /// Relink repoints exact paths AND whole moved directories, everywhere
    /// a path lives — clips, pool, music, angles.
    #[test]
    fn relink_moves_files_and_directories() {
        let mut p = one_clip_project();
        p.absorb_sources_into_pool();
        p.music = Some(Music { source: "/tmp/bed.mp3".into(), ..Default::default() });
        p.multicam.push(Angle { source: "/tmp/a.mp4".into(), offset: 0.0 });
        // Exact-file relink.
        let n = p.relink("/tmp/a.mp4", "/media/a.mp4");
        assert!(n >= 3, "clip + pool + angle at least, got {n}");
        assert!(p.tracks.iter().flat_map(|t| t.clips.iter()).all(|c| c.source == "/media/a.mp4"));
        assert_eq!(p.multicam[0].source, "/media/a.mp4");
        // Directory relink: /tmp → /vault moves the bed but must NOT touch
        // /media files, and must never match a mere string prefix like
        // /tmp2.
        let mut q = Project::default();
        q.append_video("x", "/tmp/x.mp4", 2.0);
        q.append_video("y", "/tmp2/y.mp4", 2.0);
        q.relink("/tmp", "/vault");
        let sources: Vec<String> = q.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.source.clone()).collect();
        assert!(sources.contains(&"/vault/x.mp4".to_string()), "{sources:?}");
        assert!(sources.contains(&"/tmp2/y.mp4".to_string()), "prefix must respect path boundaries: {sources:?}");
    }

    /// The switcher cut: pressing an angle splits at the playhead and the
    /// right half plays the other camera at the SAME moment — timeline time
    /// continuous, source time mapped through the angle's offset.
    #[test]
    fn multicam_cuts_to_the_angle_at_the_same_moment() {
        let mut p = one_clip_project(); // V1: /tmp/a.mp4, 0..10
        // Angle B started rolling 2 s before the timeline's zero.
        p.multicam.push(Angle { source: "/tmp/a.mp4".into(), offset: 0.0 });
        p.multicam.push(Angle { source: "/tmp/b.mp4".into(), offset: -2.0 });

        assert!(p.cut_to_angle(4.0, 1), "the cut happens");
        let v1: Vec<_> = p
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Video)
            .unwrap()
            .clips
            .iter()
            .map(|c| (c.source.clone(), c.start, c.in_point, c.duration))
            .collect();
        assert_eq!(v1.len(), 2, "{v1:?}");
        assert_eq!(v1[0], ("/tmp/a.mp4".into(), 0.0, 0.0, 4.0));
        // Angle B at timeline 4.0 = source 4.0 − (−2.0) = 6.0.
        assert_eq!(v1[1].0, "/tmp/b.mp4");
        assert!((v1[1].1 - 4.0).abs() < 1e-9 && (v1[1].2 - 6.0).abs() < 1e-9);
        assert!((v1[1].3 - 6.0).abs() < 1e-9, "keeps the remaining duration");

        // Cutting to the angle already playing at the same moment: no-op.
        assert!(!p.cut_to_angle(5.0, 1), "same angle, same time — nothing to cut");
        // Before an angle's material exists: refused.
        let mut q = one_clip_project();
        q.multicam.push(Angle { source: "/tmp/late.mp4".into(), offset: 6.0 });
        assert!(!q.cut_to_angle(4.0, 0), "the late camera has no frame at t=4");
    }

    #[test]
    fn split_divides_both_tracks_and_preserves_source_mapping() {
        let mut p = one_clip_project();
        assert_eq!(p.split_at(4.0), 2);
        let v: Vec<_> = p.tracks[0].clips.iter().collect();
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].start, v[0].duration, v[0].in_point), (0.0, 4.0, 0.0));
        assert_eq!((v[1].start, v[1].duration, v[1].in_point), (4.0, 6.0, 4.0));
        // Splitting outside any clip does nothing.
        assert_eq!(p.split_at(20.0), 0);
    }

    #[test]
    fn move_range_respects_neighbours() {
        let mut p = one_clip_project();
        p.split_at(4.0);
        let right_id = p.tracks[0].clips[1].id;
        // Move the right piece later, leaving a gap.
        p.clip_mut(right_id).unwrap().start = 7.0;
        let left_id = p.tracks[0].clips[0].id;
        let (lo, hi) = p.move_range(left_id);
        assert_eq!(lo, 0.0);
        assert!((hi - 3.0).abs() < 1e-9, "left clip (4s) may start at most at 3.0, got {hi}");
    }

    #[test]
    fn snapping_picks_nearest_within_tolerance() {
        let targets = [0.0, 4.0, 10.0];
        assert_eq!(EditorState::snap(3.9, &targets, 0.2), (4.0, Some(4.0)));
        assert_eq!(EditorState::snap(5.0, &targets, 0.2), (5.0, None));
    }

    #[test]
    fn undo_redo_roundtrip() {
        let mut p = one_clip_project();
        let mut ed = EditorState::default();
        ed.push_undo(&p);
        p.split_at(5.0);
        assert_eq!(p.tracks[0].clips.len(), 2);
        assert!(ed.undo(&mut p));
        assert_eq!(p.tracks[0].clips.len(), 1);
        assert!(ed.redo(&mut p));
        assert_eq!(p.tracks[0].clips.len(), 2);
    }

    #[test]
    fn atomic_write_replaces_cleanly_and_leaves_no_temp() {
        let path = std::env::temp_dir().join(format!("reel-atomic-{}.reel", std::process::id()));
        let p = path.to_string_lossy().into_owned();
        write_atomic(&p, "first").expect("write");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "first");
        // Overwriting an existing document must succeed and replace it whole.
        write_atomic(&p, "second, longer contents").expect("overwrite");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "second, longer contents");
        assert!(!std::path::Path::new(&format!("{p}.tmp")).exists(), "temp file left behind");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ripple_delete_closes_the_hole_on_every_track() {
        let mut p = one_clip_project(); // 10s on V1 and A1
        p.split_at(4.0);
        p.split_at(7.0); // → [0,4) [4,7) [7,10) on both tracks
        let mid = p.tracks[0].clips[1].id;
        let before = p.duration();
        assert_eq!(p.tracks[0].clips.len(), 3);

        let removed = p.ripple_delete(mid);
        assert!((removed - 3.0).abs() < 1e-9, "removed the middle clip's 3s");
        assert_eq!(p.tracks[0].clips.len(), 2);
        // The third clip slid left into the hole: no gap, and the edit is 3s shorter.
        assert!((p.tracks[0].clips[1].start - 4.0).abs() < 1e-9);
        assert!((p.duration() - (before - 3.0)).abs() < 1e-9);
        // The linked audio clip went with it, and what remains stays in sync
        // with the picture — no overlaps left behind.
        assert_eq!(p.tracks[1].clips.len(), 2, "the sound of that shot went too");
        assert!((p.tracks[1].clips[1].start - 4.0).abs() < 1e-9);
        for track in &p.tracks {
            for pair in track.clips.windows(2) {
                assert!(pair[0].end() <= pair[1].start + 1e-6, "clips must never overlap");
            }
        }
    }

    #[test]
    fn ripple_trim_pulls_the_edit_to_the_playhead() {
        // Trim the head: Q at 2s inside a clip starting at 0.
        let mut p = one_clip_project();
        p.split_at(5.0);
        let removed = p.ripple_trim_to_playhead(2.0, true);
        assert!((removed - 2.0).abs() < 1e-9);
        let first = &p.tracks[0].clips[0];
        assert!((first.start - 0.0).abs() < 1e-9, "clip slid back to the start");
        assert!((first.in_point - 2.0).abs() < 1e-9, "and kept its source offset");
        assert!((first.duration - 3.0).abs() < 1e-9);
        assert!((p.duration() - 8.0).abs() < 1e-9, "edit is 2s shorter");

        // Trim the tail: W at 1s inside a 3s clip removes its last 2s.
        let mut p = one_clip_project();
        p.split_at(3.0);
        let removed = p.ripple_trim_to_playhead(1.0, false);
        assert!((removed - 2.0).abs() < 1e-9);
        assert!((p.tracks[0].clips[0].duration - 1.0).abs() < 1e-9);
        assert!((p.tracks[0].clips[1].start - 1.0).abs() < 1e-9, "the next clip closed up");

        // Refuses to leave a sliver, or to trim outside a clip.
        let mut p = one_clip_project();
        assert_eq!(p.ripple_trim_to_playhead(0.01, true), 0.0);
        assert_eq!(p.ripple_trim_to_playhead(99.0, true), 0.0);
    }

    #[test]
    fn gaps_close_before_a_clip_and_across_the_timeline() {
        let mut p = one_clip_project(); // 10s clip at 0 on V1 and A1
        p.split_at(4.0);
        let right = p.tracks[0].clips[1].id;
        p.clip_mut(right).unwrap().start = 7.0; // leave a 3s hole

        // Closing the gap before that clip moves only it.
        assert!((p.close_gap_before(right) - 3.0).abs() < 1e-9);
        assert!((p.clip(right).unwrap().start - 4.0).abs() < 1e-9);
        assert_eq!(p.close_gap_before(right), 0.0, "already butted up");

        // A hole on each track, closed in one sweep.
        p.clip_mut(right).unwrap().start = 9.0;
        let a_id = p.tracks[1].clips[0].id;
        p.clip_mut(a_id).unwrap().start = 2.0;
        let removed = p.close_all_gaps();
        assert!(removed > 6.0, "should have removed both holes, got {removed}");
        assert_eq!(p.clip(a_id).unwrap().start, 0.0);
        assert!((p.clip(right).unwrap().start - 4.0).abs() < 1e-9);
    }

    #[test]
    fn export_range_cuts_clips_at_the_markers() {
        let mut p = one_clip_project(); // 10s clip at 0
        p.split_at(4.0); // → [0,4) and [4,10)
        // Range 2–6 must yield two segments totalling 4s, with source
        // in-points shifted to match where the markers landed.
        let segs = p.export_segments_range(Some(2.0), Some(6.0));
        assert_eq!(segs.len(), 2);
        assert_eq!((segs[0].in_point, segs[0].duration), (2.0, 2.0));
        assert_eq!((segs[1].in_point, segs[1].duration), (4.0, 2.0));
        let total: f64 = segs.iter().map(|s| s.duration).sum();
        assert!((total - 4.0).abs() < 1e-9);
        // A range beyond every clip exports nothing.
        assert!(p.export_segments_range(Some(50.0), None).is_empty());
        // No markers = the whole edit.
        assert_eq!(p.export_segments().len(), 2);
    }

    #[test]
    fn project_saves_and_loads() {
        let mut p = one_clip_project();
        p.split_at(3.0);
        let path = std::env::temp_dir().join(format!("reel-proj-test-{}.reel", std::process::id()));
        p.save(&path.to_string_lossy()).expect("save");
        let loaded = Project::load(&path.to_string_lossy()).expect("load");
        assert_eq!(loaded.tracks[0].clips.len(), 2);
        // next_id re-seeded above the stored max — appends must not collide.
        let max_id = loaded.tracks.iter().flat_map(|t| t.clips.iter()).map(|c| c.id).max().unwrap();
        let mut loaded2 = loaded;
        loaded2.append_video("b", "/tmp/b.mp4", 1.0);
        let new_max = loaded2.tracks[0].clips.iter().map(|c| c.id).max().unwrap();
        assert!(new_max > max_id);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn source_timeline_mapping_follows_trims() {
        let mut p = one_clip_project();
        p.split_at(4.0);
        // Delete the left piece; right piece starts at 4.0 with in_point 4.0.
        let left_id = p.tracks[0].clips[0].id;
        p.delete_clip(left_id);
        assert_eq!(p.source_to_timeline("/tmp/a.mp4", 5.0), Some(5.0));
        assert_eq!(p.source_to_timeline("/tmp/a.mp4", 2.0), None); // trimmed away
    }

    /// Keyframe evaluation is the maths every animated render rests on:
    /// clamped ends, linear and eased interpolation, holds.
    #[test]
    fn keyframes_interpolate_the_way_the_curve_says() {
        let keys = vec![
            Keyframe { t: 1.0, value: 10.0, interp: Interp::Linear },
            Keyframe { t: 3.0, value: 20.0, interp: Interp::Linear },
        ];
        assert_eq!(eval_keys(&keys, 0.0), Some(10.0), "before the first key: hold");
        assert_eq!(eval_keys(&keys, 5.0), Some(20.0), "after the last key: hold");
        assert_eq!(eval_keys(&keys, 2.0), Some(15.0), "linear midpoint");
        assert_eq!(eval_keys(&keys, 1.5), Some(12.5));

        let hold = vec![
            Keyframe { t: 0.0, value: 1.0, interp: Interp::Hold },
            Keyframe { t: 2.0, value: 9.0, interp: Interp::Linear },
        ];
        assert_eq!(eval_keys(&hold, 1.999), Some(1.0), "hold steps, never ramps");
        assert_eq!(eval_keys(&hold, 2.0), Some(9.0));

        let ease = vec![
            Keyframe { t: 0.0, value: 0.0, interp: Interp::Ease },
            Keyframe { t: 1.0, value: 1.0, interp: Interp::Linear },
        ];
        assert_eq!(eval_keys(&ease, 0.5), Some(0.5), "smoothstep midpoint is half");
        let q = eval_keys(&ease, 0.25).unwrap();
        assert!(q < 0.25, "ease starts slower than linear, got {q}");

        assert_eq!(eval_keys(&[], 1.0), None);
    }

    #[test]
    fn set_and_clear_keys_keep_tracks_sorted_and_tidy() {
        let mut p = one_clip_project();
        let id = p.tracks[0].clips[0].id;
        let c = p.clip_mut(id).unwrap();
        c.set_key(Param::Exposure, 2.0, 1.5, Interp::Linear);
        c.set_key(Param::Exposure, 0.5, 1.0, Interp::Linear);
        c.set_key(Param::Exposure, 2.0, 1.8, Interp::Linear); // replace, not duplicate
        let track = c.key_track(Param::Exposure).unwrap();
        assert_eq!(track.len(), 2);
        assert!(track[0].t < track[1].t, "track stays sorted");
        assert_eq!(track[1].value, 1.8);

        let (fx, _, _) = c.animated(1.25);
        assert!((fx.exposure - 1.4).abs() < 1e-5, "midpoint of 1.0→1.8, got {}", fx.exposure);

        assert!(c.clear_key(Param::Exposure, 0.5));
        assert!(!c.clear_key(Param::Exposure, 0.5), "already gone");
        assert!(c.clear_key(Param::Exposure, 2.0));
        assert!(c.key_track(Param::Exposure).is_none(), "empty tracks are removed");
    }

    /// One truth of time: totals equal render_duration to the bit, gaps
    /// collapse to the next entry, the overlap belongs to the incoming
    /// clip, round trips hold off-overlap — and the PLAYBACK PATH (which
    /// skips a transition's replayed head) is continuous in edit time.
    /// The static map is deliberately two-sheeted around transitions:
    /// a's tail and b's head are sequential on the timeline but
    /// simultaneous in the edit, and no honest function can hide that.
    #[test]
    fn the_timebase_maps_both_ways_and_agrees_with_the_render() {
        let mut p = Project::default();
        //   a: timeline 0..4
        //   b: timeline 4..7, butting a, 1 s crossfade in
        //   gap 7..8
        //   c: timeline 8..10, hard cut
        let _a = p.add_clip("/tmp/a.mp4", TrackKind::Video, 0.0, 0.0, 4.0);
        let b = p.add_clip("/tmp/b.mp4", TrackKind::Video, 4.0, 0.0, 3.0);
        let _c = p.add_clip("/tmp/c.mp4", TrackKind::Video, 8.0, 0.0, 2.0);
        p.clip_mut(b).unwrap().transition_in = 1.0;

        // Totals agree to the bit: 4 + 3 + 2 − 1 = 8.
        let want = render_duration(&p.export_segments());
        assert!((p.edit_len() - want).abs() < 1e-12, "{} vs {want}", p.edit_len());
        assert!((p.edit_len() - 8.0).abs() < 1e-9);

        // Identity inside a; shifted inside b; the gap collapses to c's
        // entry; the end clamps.
        assert!((p.timeline_to_edit(2.0) - 2.0).abs() < 1e-9);
        assert!((p.timeline_to_edit(4.5) - 3.5).abs() < 1e-9, "b's head rides the overlap");
        assert!((p.timeline_to_edit(7.5) - 6.0).abs() < 1e-9, "gaps do not exist in the edit");
        assert!((p.timeline_to_edit(8.5) - 6.5).abs() < 1e-9);
        assert!((p.timeline_to_edit(99.0) - 8.0).abs() < 1e-9);

        // The overlap belongs to the INCOMING clip: edit 3.5 is both a's
        // 3.5 and b's 4.5 — scrubbing there must land in b.
        assert!((p.edit_to_timeline(3.5) - 4.5).abs() < 1e-9);
        // Round trips hold at every point owned by a single clip.
        for e in [0.5, 2.9, 3.5, 4.5, 5.9, 6.5, 7.9] {
            let t = p.edit_to_timeline(e);
            assert!(
                (p.timeline_to_edit(t) - e).abs() < 1e-9,
                "round trip broke at edit {e}: t={t}"
            );
        }

        // The PLAYBACK PATH is continuous: a plays to its end (edit 4.0),
        // and the jump target after a transition — b at timeline start +
        // fade — is the SAME edit moment.
        let end_of_a = p.timeline_to_edit(4.0 - 1e-9);
        let resume_in_b = p.timeline_to_edit(4.0 + 1.0); // b.start + d
        assert!(
            (end_of_a - resume_in_b).abs() < 1e-6,
            "playback would jump in edit time: {end_of_a} vs {resume_in_b}"
        );
    }

    /// The mixer's routing rules: track gain composes with clip gain in dB,
    /// mute silences, and one solo silences everyone else — identically for
    /// the audio-clip list and the V1 cut's own audio.
    #[test]
    fn track_gain_mute_and_solo_route_the_mix() {
        let mut p = one_clip_project();
        p.ensure_track(TrackKind::Audio);
        let vo = p.add_clip("/tmp/vo.wav", TrackKind::Audio, 1.0, 0.0, 2.0);
        if let Some(c) = p.clip_mut(vo) {
            c.gain_db = -2.0;
        }
        if let Some(t) = p.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio) {
            t.gain_db = -4.0;
        }
        // Gains compose in dB. (one_clip_project seeds a linked A1 clip, so
        // find ours by source.)
        let clips = p.audio_clips();
        let ours = clips.iter().find(|c| c.source == "/tmp/vo.wav").expect("vo in the mix");
        assert!((ours.gain_db - -6.0).abs() < 1e-6, "got {}", ours.gain_db);
        // V1 unaffected so far.
        assert_eq!(p.video_audio_state(), (0.0, false));

        // Mute the audio track: its clips leave the mix.
        p.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio).unwrap().muted = true;
        assert!(p.audio_clips().is_empty());
        p.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio).unwrap().muted = false;

        // Solo the audio track: V1's own audio is silenced in the export.
        p.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio).unwrap().solo = true;
        assert!(
            p.audio_clips().iter().any(|c| c.source == "/tmp/vo.wav"),
            "soloed track still sounds"
        );
        let (_, silent) = p.video_audio_state();
        assert!(silent, "a solo elsewhere must silence V1");
        let segs = p.export_segments();
        assert!(segs[0].gain_db < -100.0, "V1 segment gain must be floored when soloed out");

        // Solo V1 instead: the audio track leaves, V1 stays.
        p.tracks.iter_mut().find(|t| t.kind == TrackKind::Audio).unwrap().solo = false;
        p.tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().solo = true;
        assert!(p.audio_clips().is_empty());
        assert!(!p.video_audio_state().1);
    }

    /// Tighten finds the quiet spans and closes them: the definition of the
    /// podcast jump-cut. Envelope in, shorter timeline out — with the pads
    /// protecting word edges and everything after each hole sliding up.
    #[test]
    fn fillers_cut_only_the_umms_and_close_up() {
        let mut p = one_clip_project(); // one clip, source "a.mp4", 0..10s
        let src = p.tracks[0].clips[0].source.clone();
        let cue = |t0: f64, t1: f64, text: &str| crate::captions::Cue {
            start: t0,
            end: t1,
            text: text.into(),
        };
        // Word-level cues: real words, two fillers (one with punctuation).
        let cues = vec![
            cue(1.0, 1.2, "so"),
            cue(2.0, 2.4, "Um,"),
            cue(3.0, 3.3, "today"),
            cue(5.0, 5.5, "uh…"),
            cue(6.0, 6.4, "great"),
        ];
        let words: Vec<String> = ["um", "uh"].iter().map(|w| w.to_string()).collect();
        let holes = p.filler_holes(&src, &cues, &words, 0.05);
        assert_eq!(holes.len(), 2, "exactly the two fillers: {holes:?}");
        assert!((holes[0].0 - 1.95).abs() < 0.01 && (holes[0].1 - 2.45).abs() < 0.01);

        let before = render_duration(&p.export_segments());
        let (cuts, removed) = p.cut_holes(holes);
        let after = render_duration(&p.export_segments());
        assert_eq!(cuts, 2);
        assert!((removed - 1.1).abs() < 0.02, "0.5 + 0.6 with pads = 1.1s, got {removed}");
        assert!((before - after - removed).abs() < 0.01, "the edit shortens by what was cut");
        // Real words survive: "today" (was at 3.0) still exists in the cut —
        // the source second 3.1 must still be reachable on the timeline.
        assert!(
            !p.map_source_window(&src, 3.05, 3.25).is_empty(),
            "the word 'today' must survive the de-um"
        );
        // And overlapping holes never double-cut.
        let overlapping = vec![(1.0, 2.0), (1.5, 2.5)];
        let mut q = one_clip_project();
        let (c2, r2) = q.cut_holes(overlapping);
        assert_eq!(c2, 1, "merged into one hole");
        assert!((r2 - 1.5).abs() < 0.01, "1.0..2.5 once, got {r2}");
    }

    #[test]
    fn tighten_removes_the_quiet_air_and_closes_up() {
        let mut p = one_clip_project(); // 0..10s
        // Envelope at 10 buckets/sec: loud 0..3s, SILENT 3..6s, loud 6..10s.
        let mut peaks = vec![1.0f32; 100];
        for b in peaks.iter_mut().take(60).skip(30) {
            *b = 0.01;
        }
        let mut supplier = |_src: &str| Some((peaks.clone(), 10.0));
        let (cuts, removed) = p.tighten(&mut supplier, 0.05, 0.5, 0.25);
        assert_eq!(cuts, 1);
        // 3s of silence minus 0.25s pad each side = 2.5s removed.
        assert!((removed - 2.5).abs() < 0.15, "removed {removed:.2}s, wanted ~2.5");
        assert!((p.duration() - 7.5).abs() < 0.15, "timeline is {:.2}s", p.duration());
        // No overlaps, no gaps — the cut closed up.
        let clips = &p.tracks[0].clips;
        assert!(clips.len() >= 2);
        for w in clips.windows(2) {
            let gap = w[1].start - w[0].end();
            assert!(gap.abs() < 0.02, "tighten left a {gap:.3}s seam");
        }
        // Nothing to cut → nothing happens.
        let mut loud = |_s: &str| Some((vec![1.0f32; 100], 10.0));
        let (c2, r2) = p.tighten(&mut loud, 0.05, 0.5, 0.25);
        assert_eq!((c2, r2), (0, 0.0));
    }

    /// Roll moves a cut without moving the timeline's total length; slip
    /// changes WHAT plays without moving WHEN; slide moves a clip while its
    /// neighbours absorb the motion. These invariants are the definitions.
    #[test]
    fn roll_slip_and_slide_hold_their_invariants() {
        let mut p = one_clip_project();
        p.split_at(4.0);
        p.split_at(7.0); // 0..4, 4..7, 7..10 — all from the same source
        let ids: Vec<u64> = p.tracks[0].clips.iter().map(|c| c.id).collect();
        let total_before = p.duration();

        // ROLL the middle clip's head +1: prev grows, middle shrinks, and
        // the middle now begins one second LATER in its source.
        let mid_in_before = p.clip(ids[1]).unwrap().in_point;
        let rolled = p.roll(ids[1], 1.0);
        assert!((rolled - 1.0).abs() < 1e-9);
        assert!((p.clip(ids[0]).unwrap().duration - 5.0).abs() < 1e-9);
        assert!((p.clip(ids[1]).unwrap().start - 5.0).abs() < 1e-9);
        assert!((p.clip(ids[1]).unwrap().duration - 2.0).abs() < 1e-9);
        assert!((p.clip(ids[1]).unwrap().in_point - (mid_in_before + 1.0)).abs() < 1e-9);
        assert!((p.duration() - total_before).abs() < 1e-9, "roll must not change the total");
        // Clips still butt.
        assert!((p.clip(ids[0]).unwrap().end() - p.clip(ids[1]).unwrap().start).abs() < 1e-9);

        // ROLL clamps: a huge roll can't erase either side.
        let big = p.roll(ids[1], 100.0);
        assert!(big < 100.0 && p.clip(ids[1]).unwrap().duration >= 0.05);
        let _ = p.roll(ids[1], -big); // put it back roughly

        // SLIP: the clip's window moves through the source; the timeline
        // does not move at all.
        let mut q = one_clip_project();
        q.split_at(4.0);
        let id1 = q.tracks[0].clips[1].id;
        let (s_before, d_before) = {
            let c = q.clip(id1).unwrap();
            (c.start, c.duration)
        };
        // in_point starts at 4; slipping -10 clamps at the source's start.
        let slipped = q.slip(id1, -10.0);
        assert!((slipped + 4.0).abs() < 1e-9, "slip clamps at source 0, got {slipped}");
        let c = q.clip(id1).unwrap();
        assert_eq!((c.start, c.duration), (s_before, d_before), "slip must not move the clip");
        assert!((c.in_point - 0.0).abs() < 1e-9);

        // SLIDE: middle moves, neighbours absorb, three-clip span unchanged.
        let mut r = one_clip_project();
        r.split_at(3.0);
        r.split_at(6.0);
        let rids: Vec<u64> = r.tracks[0].clips.iter().map(|c| c.id).collect();
        let span_before = r.clip(rids[2]).unwrap().end() - r.clip(rids[0]).unwrap().start;
        let next_in_before = r.clip(rids[2]).unwrap().in_point;
        let slid = r.slide(rids[1], 1.5);
        assert!((slid - 1.5).abs() < 1e-9);
        assert!((r.clip(rids[1]).unwrap().start - 4.5).abs() < 1e-9);
        assert!((r.clip(rids[0]).unwrap().duration - 4.5).abs() < 1e-9);
        assert!((r.clip(rids[2]).unwrap().in_point - (next_in_before + 1.5)).abs() < 1e-9);
        let span_after = r.clip(rids[2]).unwrap().end() - r.clip(rids[0]).unwrap().start;
        assert!((span_after - span_before).abs() < 1e-9, "slide must keep the span");
        // Still gapless.
        assert!((r.clip(rids[0]).unwrap().end() - r.clip(rids[1]).unwrap().start).abs() < 1e-9);
        assert!((r.clip(rids[1]).unwrap().end() - r.clip(rids[2]).unwrap().start).abs() < 1e-9);

        // No neighbour → no roll, no slide.
        let mut lone = one_clip_project();
        let lid = lone.tracks[0].clips[0].id;
        assert_eq!(lone.roll(lid, 1.0), 0.0);
        assert_eq!(lone.slide(lid, 1.0), 0.0);
    }

    /// The ramp integral is the contract between picture and sound: both
    /// read source consumption off this one function.
    #[test]
    fn the_speed_integral_is_exact_for_every_curve_shape() {
        let lin = vec![
            Keyframe { t: 0.0, value: 1.0, interp: Interp::Linear },
            Keyframe { t: 4.0, value: 2.0, interp: Interp::Linear },
        ];
        // Mean of 1→2 is 1.5 → 6 s of source over 4 s of output.
        assert!((speed_integral(&lin, 1.0, 4.0) - 6.0).abs() < 1e-9);
        assert!((speed_integral(&lin, 1.0, 2.0) - 2.5).abs() < 1e-9, "quadratic mid: 1·2 + ½·¼·2²");

        // Ease integrates to the same mean over a full interval.
        let ease = vec![
            Keyframe { t: 0.0, value: 1.0, interp: Interp::Ease },
            Keyframe { t: 4.0, value: 2.0, interp: Interp::Linear },
        ];
        assert!((speed_integral(&ease, 1.0, 4.0) - 6.0).abs() < 1e-9);

        // Hold consumes at the held value until the next key.
        let hold = vec![
            Keyframe { t: 0.0, value: 3.0, interp: Interp::Hold },
            Keyframe { t: 2.0, value: 1.0, interp: Interp::Linear },
        ];
        assert!((speed_integral(&hold, 1.0, 2.0) - 6.0).abs() < 1e-9);

        // Before the first key and after the last: held flat.
        let mid = vec![
            Keyframe { t: 1.0, value: 2.0, interp: Interp::Linear },
            Keyframe { t: 2.0, value: 2.0, interp: Interp::Linear },
        ];
        assert!((speed_integral(&mid, 1.0, 3.0) - 6.0).abs() < 1e-9);

        // The inverse gets back to where it started.
        for probe in [0.5, 1.7, 3.2] {
            let src = speed_integral(&lin, 1.0, probe);
            let back = speed_integral_invert(&lin, 1.0, src, 4.0);
            assert!((back - probe).abs() < 1e-6, "invert({src}) = {back}, wanted {probe}");
        }
    }

    /// Pasting inserts: it makes room instead of overwriting whatever was
    /// already there. Losing footage to a stray Ctrl+V is the kind of thing
    /// people never forgive an editor for.
    #[test]
    fn pasting_makes_room_instead_of_overwriting() {
        let mut p = one_clip_project();
        p.split_at(4.0); // 0..4 and 4..10
        let first = p.tracks[0].clips[0].clone();

        let id = p.paste_clip(&first, 4.0, TrackKind::Video);
        let clips = &p.tracks[0].clips;
        assert_eq!(clips.len(), 3, "paste should add a clip, not replace one");

        // Nothing overlaps, and the total length grew by exactly the paste.
        for w in clips.windows(2) {
            assert!(
                w[1].start >= w[0].end() - 1e-6,
                "paste left clips overlapping: {:?} then {:?}",
                (w[0].start, w[0].end()),
                (w[1].start, w[1].end())
            );
        }
        let end = clips.iter().map(|c| c.end()).fold(0.0, f64::max);
        assert!((end - 14.0).abs() < 1e-6, "expected 10s + a 4s paste, got {end}");

        // The pasted clip is a real copy with its own id, at the playhead.
        let pasted = clips.iter().find(|c| c.id == id).unwrap();
        assert_ne!(pasted.id, first.id);
        assert!((pasted.start - 4.0).abs() < 1e-6);
        assert_eq!(pasted.source, first.source);
        assert!((pasted.in_point - first.in_point).abs() < 1e-6);
    }

    /// Pasting into the MIDDLE of a clip has to split it, not shove it aside
    /// and leave a hole where it used to be.
    #[test]
    fn pasting_mid_clip_splits_it_rather_than_leaving_a_hole() {
        let mut p = one_clip_project();
        let src = p.tracks[0].clips[0].clone(); // 0..10
        p.paste_clip(&src, 3.0, TrackKind::Video);

        let mut clips = p.tracks[0].clips.clone();
        clips.sort_by(|a, b| a.start.total_cmp(&b.start));
        // 0..3, the 10s paste at 3..13, then the old tail at 13..20.
        assert_eq!(clips.len(), 3);
        for w in clips.windows(2) {
            let gap = w[1].start - w[0].end();
            assert!(gap.abs() < 1e-6, "paste left a {gap:.3}s hole in the timeline");
        }
        let end = clips.last().unwrap().end();
        assert!((end - 20.0).abs() < 1e-6, "expected 10s + a 10s paste, got {end}");
    }

    #[test]
    fn duplicate_lands_immediately_after_the_original() {
        let mut p = one_clip_project();
        let id = p.tracks[0].clips[0].id;
        let end = p.tracks[0].clips[0].end();
        let copy = p.duplicate_clip(id).expect("duplicate");
        let c = p.tracks[0].clips.iter().find(|c| c.id == copy).unwrap();
        assert!((c.start - end).abs() < 1e-6, "copy should butt against the original");
        assert_eq!(p.tracks[0].clips.len(), 2);
    }

    /// Captions are written against the original recording, so mapping them
    /// into the edit has to survive the three things editors actually do:
    /// cut a line in half, delete a chunk, and reuse a moment twice.
    #[test]
    fn caption_windows_survive_cuts_gaps_and_reuse() {
        let mut p = one_clip_project();
        // Source is 0..10 at timeline 0..10. Cut at 4 and pull the right
        // half later, leaving a gap: 0..4 stays, 4..10 moves to 6..12.
        p.split_at(4.0);
        p.tracks[0].clips[1].start = 6.0;

        // A cue spanning the cut (3.5..4.5 in source) must land in BOTH
        // pieces, clipped at the join — never running over the gap.
        let spans = p.map_source_window("/tmp/a.mp4", 3.5, 4.5);
        assert_eq!(spans.len(), 2, "a cue across a cut belongs to both pieces");
        assert!((spans[0].0 - 3.5).abs() < 1e-6 && (spans[0].1 - 4.0).abs() < 1e-6);
        assert!((spans[1].0 - 6.0).abs() < 1e-6 && (spans[1].1 - 6.5).abs() < 1e-6);

        // Reuse: duplicate the tail, and the same words caption both copies.
        let clip = p.tracks[0].clips[1].clone();
        let mut copy = clip.clone();
        copy.id = p.next_id;
        p.next_id += 1;
        copy.start = 20.0;
        p.tracks[0].clips.push(copy);
        assert_eq!(
            p.map_source_window("/tmp/a.mp4", 5.0, 5.5).len(),
            2,
            "a moment used twice captions twice"
        );

        // A window that was trimmed away captions nowhere.
        let mut q = one_clip_project();
        q.split_at(4.0);
        let left = q.tracks[0].clips[0].id;
        q.delete_clip(left);
        assert!(q.map_source_window("/tmp/a.mp4", 1.0, 2.0).is_empty());
    }
}
