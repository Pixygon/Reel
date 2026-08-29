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
        }
    }

    pub const ALL: [Param; 15] = [
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Interp {
    Linear,
    /// Step: hold this value until the next keyframe.
    Hold,
    /// Smooth in and out (smoothstep) — the "just make it nice" curve.
    Ease,
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
}

impl Default for Music {
    fn default() -> Self {
        Self { source: String::new(), start: 0.0, gain_db: -12.0, duck: true, fade: 1.0 }
    }
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
    /// Timeline positions flagged by the user. Part of the document: a
    /// marker you dropped yesterday should still be there today.
    #[serde(default)]
    pub markers: Vec<f64>,
    /// The .cube files this project grades with; clips reference them by
    /// index (`Effects.lut`).
    #[serde(default)]
    pub luts: Vec<String>,
    #[serde(skip)]
    next_id: u64,
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
                Track { id: 1, name: "V1".into(), kind: TrackKind::Video, clips: vec![], muted: false },
                Track { id: 2, name: "A1".into(), kind: TrackKind::Audio, clips: vec![], muted: false },
            ],
            captions: Vec::new(),
            caption_size: default_caption_size(),
            titles: Vec::new(),
            music: None,
            markers: Vec::new(),
            luts: Vec::new(),
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
        if holes.is_empty() {
            return (0, 0.0);
        }
        holes.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Cut from the END backwards so earlier hole positions stay valid.
        let mut removed = 0.0;
        let mut cuts = 0;
        for (t0, t1) in holes.iter().rev() {
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
            let mut span = 0.0f64;
            for id in doomed {
                span = span.max(self.clip(id).map(|c| c.duration).unwrap_or(0.0));
                self.delete_clip(id);
            }
            // Close the hole on every track.
            let gap = t1 - t0;
            for track in &mut self.tracks {
                for c in &mut track.clips {
                    if c.start >= t1 - 0.01 {
                        c.start -= gap;
                    }
                }
                track.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
            }
            removed += gap;
            cuts += 1;
            let _ = span;
        }
        (cuts, removed)
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
        let track = Track { id, name: name.into(), kind, clips: vec![], muted: false };
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
        let mut out: Vec<AudioClip> = self
            .tracks
            .iter()
            .filter(|t| {
                matches!(t.kind, TrackKind::Audio | TrackKind::Overlay) && !t.muted
            })
            .flat_map(|t| t.clips.iter())
            .map(|c| AudioClip {
                source: c.source.clone(),
                at: c.start,
                in_point: c.in_point,
                duration: c.duration,
                gain_db: c.gain_db,
                fade_in: c.effects.fade_in,
                fade_out: c.effects.fade_out,
                speed: c.speed,
            })
            .collect();
        out.sort_by(|a, b| a.at.total_cmp(&b.at));
        out
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
                    gain_db: c.gain_db,
                    speed: c.speed,
                    keys: c.keys.clone(),
                    stabilize: c.stabilize,
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
    /// Index of the title being edited, if any — it shows a box in the
    /// preview and is the one you can drag around.
    pub selected_title: Option<usize>,
    /// The copied clip, and which kind of track it came from.
    pub clipboard: Option<(Clip, TrackKind)>,
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

    /// Tighten finds the quiet spans and closes them: the definition of the
    /// podcast jump-cut. Envelope in, shorter timeline out — with the pads
    /// protecting word edges and everything after each hole sliding up.
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
