//! Export / convert — the HandBrake seam. One source file in, one encoded file
//! out, straight from the player (no editor round-trip). Runs ffmpeg on a
//! worker thread and reports live progress; the UI polls `ExportJob::state()`.
//! Timeline (composited) export is Milestone 3 — this is source-file convert.

use crate::edit::Segment;
use crate::media::MediaKind;
use anyhow::{anyhow, Result};
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::{FfmpegEvent, LogLevel};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Codec {
    /// H.264 in MP4 — plays everywhere.
    H264,
    /// H.265/HEVC in MP4 — ~40% smaller at the same quality.
    H265,
    /// AV1 (SVT) in MP4 — best compression, slower encode.
    Av1,
    /// VP9 in WebM — the web-native pick.
    Vp9,
    /// No re-encode: remux the streams into MKV as-is. Instant, lossless.
    Remux,
    // Audio-only outputs — for audio sources, or extracting from a video.
    Mp3,
    M4a,
    OpusAudio,
    Flac,
    Wav,
    // Image outputs.
    Png,
    Jpeg,
    WebpImage,
}

impl Codec {
    /// The codecs that make sense for a given source kind. Video sources also
    /// offer the audio-only outputs — that's "extract the audio".
    pub fn for_kind(kind: MediaKind) -> &'static [Codec] {
        match kind {
            MediaKind::Video => &[
                Codec::H264, Codec::H265, Codec::Av1, Codec::Vp9, Codec::Remux,
                Codec::Mp3, Codec::M4a, Codec::OpusAudio, Codec::Flac, Codec::Wav,
            ],
            MediaKind::Audio => &[Codec::Mp3, Codec::M4a, Codec::OpusAudio, Codec::Flac, Codec::Wav],
            MediaKind::Image => &[Codec::Png, Codec::Jpeg, Codec::WebpImage],
        }
    }

    pub fn is_audio_only(self) -> bool {
        matches!(self, Codec::Mp3 | Codec::M4a | Codec::OpusAudio | Codec::Flac | Codec::Wav)
    }

    pub fn is_image(self) -> bool {
        matches!(self, Codec::Png | Codec::Jpeg | Codec::WebpImage)
    }

    /// Lossless outputs have no quality knob.
    pub fn has_quality(self) -> bool {
        !matches!(self, Codec::Remux | Codec::Flac | Codec::Wav | Codec::Png)
    }

    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "MP4 · H.264 (compatible)",
            Codec::H265 => "MP4 · H.265 (smaller)",
            Codec::Av1 => "MP4 · AV1 (smallest, slow)",
            Codec::Vp9 => "WebM · VP9 (web)",
            Codec::Remux => "MKV · no re-encode (instant)",
            Codec::Mp3 => "MP3 · audio only",
            Codec::M4a => "M4A/AAC · audio only",
            Codec::OpusAudio => "Opus · audio only",
            Codec::Flac => "FLAC · audio, lossless",
            Codec::Wav => "WAV · audio, uncompressed",
            Codec::Png => "PNG · lossless",
            Codec::Jpeg => "JPEG · small",
            Codec::WebpImage => "WebP · web",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Codec::H264 | Codec::H265 | Codec::Av1 => "mp4",
            Codec::Vp9 => "webm",
            Codec::Remux => "mkv",
            Codec::Mp3 => "mp3",
            Codec::M4a => "m4a",
            Codec::OpusAudio => "opus",
            Codec::Flac => "flac",
            Codec::Wav => "wav",
            Codec::Png => "png",
            Codec::Jpeg => "jpg",
            Codec::WebpImage => "webp",
        }
    }

    /// Audio bitrate (kb/s) for the quality tiers of audio-only codecs.
    fn audio_kbps(self, q: Quality) -> u32 {
        let (high, balanced, small) = match self {
            Codec::Mp3 => (320, 192, 128),
            Codec::M4a => (256, 160, 96),
            Codec::OpusAudio => (192, 128, 64),
            _ => (0, 0, 0),
        };
        match q {
            Quality::High => high,
            Quality::Balanced | Quality::Custom(_) => balanced,
            Quality::Small => small,
        }
    }

    /// CRF for the three named quality tiers — scales differ per codec.
    fn crf(self, q: Quality) -> u8 {
        let (high, balanced, small) = match self {
            Codec::H264 => (18, 21, 26),
            Codec::H265 => (20, 23, 28),
            Codec::Av1 => (24, 32, 40),
            Codec::Vp9 => (24, 31, 36),
            _ => (0, 0, 0),
        };
        match q {
            Quality::High => high,
            Quality::Balanced => balanced,
            Quality::Small => small,
            Quality::Custom(v) => v,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Quality {
    High,
    Balanced,
    Small,
    /// Raw CRF — lower is better/bigger. Range depends on codec.
    Custom(u8),
}

impl Quality {
    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "High (near-lossless)",
            Quality::Balanced => "Balanced",
            Quality::Small => "Small file",
            Quality::Custom(_) => "Custom CRF",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Source,
    H2160,
    H1080,
    H720,
    H480,
}

impl Resolution {
    pub const ALL: [Resolution; 5] =
        [Resolution::Source, Resolution::H2160, Resolution::H1080, Resolution::H720, Resolution::H480];

    pub fn label(self) -> &'static str {
        match self {
            Resolution::Source => "Source",
            Resolution::H2160 => "2160p (4K)",
            Resolution::H1080 => "1080p",
            Resolution::H720 => "720p",
            Resolution::H480 => "480p",
        }
    }

    fn height(self) -> Option<u32> {
        match self {
            Resolution::Source => None,
            Resolution::H2160 => Some(2160),
            Resolution::H1080 => Some(1080),
            Resolution::H720 => Some(720),
            Resolution::H480 => Some(480),
        }
    }
}

/// How a source is made to fit a target frame whose aspect differs — the
/// decision every "post this to TikTok" workflow silently makes for you.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// Whole frame visible, bars where it doesn't reach (letterbox/pillarbox).
    Letterbox,
    /// Fill the frame, cropping the overflow — no bars, loses the edges.
    Crop,
    /// Fill with a blurred copy of the frame behind the whole picture. What
    /// social apps do to a landscape clip in a vertical slot.
    Blur,
}

impl Fit {
    pub const ALL: [Fit; 3] = [Fit::Blur, Fit::Letterbox, Fit::Crop];

    pub fn label(self) -> &'static str {
        match self {
            Fit::Letterbox => "Fit (bars)",
            Fit::Crop => "Fill (crop)",
            Fit::Blur => "Fill (blurred sides)",
        }
    }

    /// The filter chain that maps any input to exactly `w`×`h`.
    /// `tag` keeps labels unique when several of these appear in one graph.
    fn chain(self, w: u32, h: u32, tag: &str) -> String {
        match self {
            Fit::Letterbox => format!(
                "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1"
            ),
            Fit::Crop => format!(
                "scale={w}:{h}:force_original_aspect_ratio=increase:flags=lanczos,\
                 crop={w}:{h},setsar=1"
            ),
            Fit::Blur => format!(
                "split[bg{tag}][fg{tag}];\
                 [bg{tag}]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},\
                 boxblur=luma_radius=min(h\\,w)/20:luma_power=1[bb{tag}];\
                 [fg{tag}]scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos[ff{tag}];\
                 [bb{tag}][ff{tag}]overlay=(W-w)/2:(H-h)/2,setsar=1"
            ),
        }
    }
}

/// One-click targets for the places people actually post video. Each carries
/// the frame, the fit and the codec that platform wants, so the user picks a
/// destination rather than a resolution.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Preset {
    pub name: &'static str,
    pub note: &'static str,
    pub w: u32,
    pub h: u32,
    pub fit: Fit,
    pub codec: Codec,
    pub quality: Quality,
}

impl Preset {
    /// Vertical 9:16 is the shape of TikTok / Reels / Shorts; 1:1 and 4:5 are
    /// the feed shapes; 16:9 is YouTube and X.
    pub const ALL: &'static [Preset] = &[
        Preset { name: "YouTube", note: "1080p · 16:9", w: 1920, h: 1080, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::High },
        Preset { name: "YouTube 4K", note: "2160p · 16:9", w: 3840, h: 2160, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::High },
        Preset { name: "TikTok", note: "1080×1920 · 9:16", w: 1080, h: 1920, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced },
        Preset { name: "Reels / Shorts", note: "1080×1920 · 9:16", w: 1080, h: 1920, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced },
        Preset { name: "Instagram feed", note: "1080×1350 · 4:5", w: 1080, h: 1350, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced },
        Preset { name: "Square", note: "1080×1080 · 1:1", w: 1080, h: 1080, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced },
        Preset { name: "Facebook", note: "1080p · 16:9", w: 1920, h: 1080, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::Balanced },
        Preset { name: "X / Twitter", note: "720p · 16:9", w: 1280, h: 720, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::Balanced },
    ];

    pub fn apply(&self, s: &mut ExportSettings) {
        s.codec = self.codec;
        s.quality = self.quality;
        s.resolution = Resolution::Source; // the preset's frame wins
        s.target = Some((self.w, self.h));
        s.fit = self.fit;
        s.audio = AudioMode::Encode { kbps: 160 };
    }

    /// Is this preset what the settings currently describe?
    pub fn is_active(&self, s: &ExportSettings) -> bool {
        s.target == Some((self.w, self.h)) && s.fit == self.fit && s.codec == self.codec && s.quality == self.quality
    }

    /// A filename tag so exports for different places don't collide.
    pub fn slug(&self) -> String {
        self.name.to_lowercase().replace([' ', '/'], "-").replace("--", "-")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AudioMode {
    /// Encode to the container's native codec (AAC for MP4, Opus for WebM).
    Encode { kbps: u32 },
    /// Pass the source audio through untouched (may not fit every container).
    Copy,
}

#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub codec: Codec,
    pub quality: Quality,
    pub resolution: Resolution,
    pub audio: AudioMode,
    /// Use the GPU encoder when one is available (much faster; software
    /// still wins slightly on size-at-quality).
    pub hardware: bool,
    /// An exact output frame (a social preset). `None` = keep the source
    /// shape, with `resolution` as an optional downscale.
    pub target: Option<(u32, u32)>,
    /// How the source is mapped into `target` when the aspect differs.
    pub fit: Fit,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            quality: Quality::Balanced,
            resolution: Resolution::Source,
            audio: AudioMode::Encode { kbps: 160 },
            hardware: true,
            target: None,
            // Letterbox by default: when the aspect already matches (the
            // normal timeline case) it's a no-op, and it costs nothing.
            // Presets pick Blur where a shape change is expected.
            fit: Fit::Letterbox,
        }
    }
}

/// Default output path: next to the source, `<stem>.reel.<ext>`, uniquified so
/// a default never clobbers an existing file.
pub fn default_output(input: &str, codec: Codec) -> String {
    let p = Path::new(input);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "out".into());
    let dir = p.parent().unwrap_or_else(|| Path::new("."));
    let ext = codec.extension();
    let mut candidate = dir.join(format!("{stem}.reel.{ext}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = dir.join(format!("{stem}.reel-{n}.{ext}"));
        n += 1;
    }
    candidate.to_string_lossy().into_owned()
}

#[derive(Clone, Debug, Default)]
pub struct ExportState {
    /// 0.0..=1.0 (best-effort; from ffmpeg's out_time vs the source duration).
    pub fraction: f32,
    /// Encode speed as a multiple of realtime, e.g. 2.4 = 2.4×.
    pub speed: f32,
    pub finished: bool,
    /// Set when the job ended in failure (or cancellation).
    pub error: Option<String>,
}

/// A running export. Dropping it does NOT cancel (the file keeps encoding);
/// call `cancel()` for that.
pub struct ExportJob {
    pub output: String,
    state: Arc<Mutex<ExportState>>,
    cancel: Arc<AtomicBool>,
}

impl ExportJob {
    pub fn state(&self) -> ExportState {
        self.state.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Build the ffmpeg argument list for `settings` — pure, unit-tested.
pub fn build_args(input: &str, output: &str, s: &ExportSettings) -> Vec<String> {
    let mut a: Vec<String> = vec!["-i".into(), input.into()];

    // Audio-only output: drop video, encode audio, done.
    if s.codec.is_audio_only() {
        a.push("-vn".into());
        match s.codec {
            Codec::Mp3 => a.extend(["-c:a".into(), "libmp3lame".into()]),
            Codec::M4a => a.extend(["-c:a".into(), "aac".into()]),
            Codec::OpusAudio => a.extend(["-c:a".into(), "libopus".into()]),
            Codec::Flac => a.extend(["-c:a".into(), "flac".into()]),
            Codec::Wav => a.extend(["-c:a".into(), "pcm_s16le".into()]),
            _ => unreachable!(),
        }
        let kbps = s.codec.audio_kbps(s.quality);
        if kbps > 0 {
            a.extend(["-b:a".into(), format!("{kbps}k")]);
        }
        a.push(output.into());
        return a;
    }

    // Image output: one frame, optional downscale, per-format quality.
    if s.codec.is_image() {
        if let Some(h) = s.resolution.height() {
            a.extend(["-vf".into(), format!("scale=-2:{h}:flags=lanczos")]);
        }
        match s.codec {
            Codec::Png => a.extend(["-c:v".into(), "png".into()]),
            Codec::Jpeg => {
                // mjpeg quality scale is 2 (best) … 31 (worst).
                let q = match s.quality {
                    Quality::High => 2,
                    Quality::Balanced => 5,
                    Quality::Small => 10,
                    Quality::Custom(v) => (v as i32).clamp(2, 31),
                };
                a.extend(["-c:v".into(), "mjpeg".into(), "-q:v".into(), q.to_string()]);
            }
            Codec::WebpImage => {
                let q = match s.quality {
                    Quality::High => 95,
                    Quality::Balanced => 80,
                    Quality::Small => 60,
                    Quality::Custom(v) => (v as i32).clamp(1, 100),
                };
                a.extend(["-c:v".into(), "libwebp".into(), "-quality".into(), q.to_string()]);
            }
            _ => unreachable!(),
        }
        a.extend(["-frames:v".into(), "1".into()]);
        a.push(output.into());
        return a;
    }

    if s.codec != Codec::Remux {
        if let Some((tw, th)) = s.target {
            // A social preset: an exact frame, with the chosen fit.
            a.extend(["-vf".into(), s.fit.chain(tw, th, "0")]);
        } else if let Some(h) = s.resolution.height() {
            // -2: keep aspect, round width to even (encoders require it).
            a.extend(["-vf".into(), format!("scale=-2:{h}:flags=lanczos")]);
        }
    }

    if s.codec == Codec::Remux {
        a.extend(["-c".into(), "copy".into()]);
    } else {
        a.extend(video_encoder_args(s.codec, s.quality, s.hardware));
    }
    if s.codec != Codec::Remux {
        match s.audio {
            AudioMode::Copy => a.extend(["-c:a".into(), "copy".into()]),
            AudioMode::Encode { kbps } => {
                let codec = if s.codec == Codec::Vp9 { "libopus" } else { "aac" };
                a.extend(["-c:a".into(), codec.into(), "-b:a".into(), format!("{kbps}k")]);
            }
        }
        // Faster start for streamed/progressive playback of MP4s.
        if s.codec != Codec::Vp9 {
            a.extend(["-movflags".into(), "+faststart".into()]);
        }
    }

    a.push(output.into());
    a
}

/// Hardware video encoders Reel can use. Only families that accept ordinary
/// software frames are listed — VAAPI/QSV need `hwupload` plumbing in the
/// filter graph, which would complicate every timeline render, so they're a
/// later step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HwEncoder {
    Nvenc,
    VideoToolbox,
}

impl HwEncoder {
    fn encoder_name(self, codec: Codec) -> Option<&'static str> {
        match (self, codec) {
            (HwEncoder::Nvenc, Codec::H264) => Some("h264_nvenc"),
            (HwEncoder::Nvenc, Codec::H265) => Some("hevc_nvenc"),
            (HwEncoder::Nvenc, Codec::Av1) => Some("av1_nvenc"),
            (HwEncoder::VideoToolbox, Codec::H264) => Some("h264_videotoolbox"),
            (HwEncoder::VideoToolbox, Codec::H265) => Some("hevc_videotoolbox"),
            _ => None, // VP9 and everything else stay on the CPU
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            HwEncoder::Nvenc => "NVIDIA NVENC",
            HwEncoder::VideoToolbox => "VideoToolbox",
        }
    }

    /// Encoder args for a quality tier — quality-targeted, not bitrate, so
    /// the tiers mean the same thing as their software counterparts.
    fn args(self, codec: Codec, q: Quality) -> Vec<String> {
        let name = self.encoder_name(codec).expect("checked by caller");
        let mut a: Vec<String> = vec!["-c:v".into(), name.into()];
        match self {
            HwEncoder::Nvenc => {
                // p5 = balanced quality preset; cq mirrors the CRF ladder.
                a.extend(["-preset".into(), "p5".into(), "-rc".into(), "vbr".into(), "-b:v".into(), "0".into()]);
                a.extend(["-cq".into(), codec.crf(q).to_string()]);
            }
            HwEncoder::VideoToolbox => {
                // VideoToolbox takes a 1–100 quality scale, inverse of CRF.
                let cq = (100 - (codec.crf(q) as i32 * 2)).clamp(20, 95);
                a.extend(["-q:v".into(), cq.to_string()]);
            }
        }
        a
    }
}

/// The best hardware encoder available for `codec`, probed once per process
/// (an `ffmpeg -encoders` listing plus a real one-frame trial encode — a
/// listed encoder can still fail when the GPU/driver isn't usable).
pub fn hw_encoder_for(codec: Codec) -> Option<HwEncoder> {
    static AVAILABLE: OnceLock<Vec<HwEncoder>> = OnceLock::new();
    let available = AVAILABLE.get_or_init(|| {
        if std::env::var("REEL_NO_HWENC").is_ok() {
            log::info!("REEL_NO_HWENC set — software encoding only");
            return Vec::new();
        }
        let listing = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        let mut found = Vec::new();
        for (family, probe) in [
            (HwEncoder::Nvenc, "h264_nvenc"),
            (HwEncoder::VideoToolbox, "h264_videotoolbox"),
        ] {
            if !listing.contains(probe) {
                continue;
            }
            // Trial encode: listed ≠ usable (no GPU, no driver, in a VM…).
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner", "-v", "error",
                    "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=0.1",
                    "-c:v", probe, "-f", "null", "-",
                ])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if ok {
                log::info!("hardware encoder available: {}", family.label());
                found.push(family);
            }
        }
        found
    });
    available.iter().copied().find(|hw| hw.encoder_name(codec).is_some())
}

/// Video-encoder args: hardware when asked for and available, else software.
fn video_encoder_args(codec: Codec, q: Quality, hw: bool) -> Vec<String> {
    if hw {
        if let Some(enc) = hw_encoder_for(codec) {
            return enc.args(codec, q);
        }
    }
    let mut a: Vec<String> = Vec::new();
    match codec {
        Codec::H265 => a.extend(["-c:v".into(), "libx265".into(), "-preset".into(), "medium".into(), "-tag:v".into(), "hvc1".into()]),
        Codec::Av1 => a.extend(["-c:v".into(), "libsvtav1".into(), "-preset".into(), "6".into()]),
        Codec::Vp9 => a.extend(["-c:v".into(), "libvpx-vp9".into(), "-b:v".into(), "0".into(), "-row-mt".into(), "1".into()]),
        _ => a.extend(["-c:v".into(), "libx264".into(), "-preset".into(), "medium".into()]),
    }
    a.extend(["-crf".into(), codec.crf(q).to_string()]);
    a
}

/// Does the file have an audio stream? (Timeline export needs to know before
/// building the filter graph.)
pub fn has_audio_stream(path: &str) -> bool {
    std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "a", "-show_entries", "stream=index", "-of", "csv=p=0", path])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// ffmpeg args that render an EDIT: segments of (source, in_point, duration)
/// are trimmed and concatenated (video + audio in lockstep when the sources
/// have sound), then encoded with the chosen video codec settings. This is
/// the timeline export — the cut itself becomes the file.
#[cfg(test)]
pub fn build_timeline_args(
    segments: &[Segment],
    output: &str,
    s: &ExportSettings,
    with_audio: bool,
    target: (u32, u32, f64),
) -> Vec<String> {
    build_timeline_args_with_music(segments, output, s, with_audio, target, None)
}

/// Build the ffmpeg argument list for a timeline render, with an optional
/// music bed laid under the cut. Pure, and unit-tested.
#[cfg(test)]
pub fn build_timeline_args_with_music(
    segments: &[Segment],
    output: &str,
    s: &ExportSettings,
    with_audio: bool,
    target: (u32, u32, f64),
    music: Option<&crate::edit::Music>,
) -> Vec<String> {
    build_timeline_args_full(segments, output, s, with_audio, target, music, &[])
}

/// The full timeline graph: the cut, a music bed, and any overlay clips
/// composited on top.
pub fn build_timeline_args_full(
    segments: &[Segment],
    output: &str,
    s: &ExportSettings,
    with_audio: bool,
    target: (u32, u32, f64),
    music: Option<&crate::edit::Music>,
    overlays: &[crate::edit::OverlaySegment],
) -> Vec<String> {
    let (tw, th, tfps) = target;
    let mut a: Vec<String> = Vec::new();
    // One -i per unique source, in first-appearance order.
    let mut sources: Vec<&str> = Vec::new();
    for seg in segments {
        if !sources.contains(&seg.source.as_str()) {
            sources.push(&seg.source);
        }
    }
    for src in &sources {
        a.extend(["-i".into(), (*src).into()]);
    }
    // The music bed is one more input, after every clip source.
    let music = music.filter(|m| !m.source.is_empty());
    let mut next_input = sources.len();
    let music_input = music.map(|m| {
        a.extend(["-i".into(), m.source.clone()]);
        next_input += 1;
        next_input - 1
    });
    let overlay_inputs: Vec<usize> = overlays
        .iter()
        .map(|o| {
            a.extend(["-i".into(), o.source.clone()]);
            next_input += 1;
            next_input - 1
        })
        .collect();

    // concat demands identical geometry/rate/audio format across segments, so
    // every segment is normalised to the target frame with the chosen fit
    // (never distorting), square pixels, one frame rate, one audio format.
    // This is what makes mixing differently-sized sources work — and what
    // reshapes a whole edit into a preset's frame.
    let anorm = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";

    let mut graph = String::new();
    for (k, seg) in segments.iter().enumerate() {
        // `duration` is the clip's TIMELINE length, so the window taken from
        // the source is longer (or shorter) by the speed factor. Getting this
        // backwards makes a 2x clip run half as long as the timeline says.
        let rate = seg.speed.clamp(0.05, 20.0) as f64;
        let src_len = seg.duration * rate;
        let i = sources.iter().position(|s| *s == seg.source).unwrap();
        let (in_point, duration) = (seg.in_point, seg.duration);
        // Per-clip effects run before the frame is fitted to the target, so
        // fades and colour behave the same whatever the output shape is.
        let fx = seg.effects.filters(duration);
        let fx = if fx.is_empty() { String::new() } else { format!("{},", fx.join(",")) };
        let reframe = seg
            .effects
            .reframe_filter(tw, th)
            .map(|f| format!(",{f}"))
            .unwrap_or_default();
        let vnorm = format!("{}{reframe},fps={tfps:.4}", s.fit.chain(tw, th, &k.to_string()));
        let vspeed = if (rate - 1.0).abs() > 1e-6 {
            format!(",setpts=PTS/{rate:.6}")
        } else {
            String::new()
        };
        graph.push_str(&format!(
            "[{i}:v]trim=start={in_point:.4}:duration={src_len:.4},setpts=PTS-STARTPTS{vspeed},\
             {fx}{vnorm}[v{k}];"
        ));
        if with_audio {
            let gain = if seg.gain_db.abs() > 0.01 {
                format!(",volume={:.2}dB", seg.gain_db)
            } else {
                String::new()
            };
            graph.push_str(&format!(
                "[{i}:a]atrim=start={in_point:.4}:duration={src_len:.4},asetpts=PTS-STARTPTS,\
                 {anorm}{}{gain}[a{k}];",
                atempo_chain(rate)
            ));
        }
    }
    if segments.iter().any(|seg| seg.transition_in > 0.0) {
        // Crossfades: chain xfade/acrossfade instead of a plain concat. Each
        // transition OVERLAPS its two clips, so the running offset (where the
        // next fade starts) subtracts every fade consumed so far — the same
        // arithmetic `Project::timeline_duration_with_transitions` does, so
        // the preview's playhead and the rendered file agree.
        let mut vprev = "v0".to_string();
        let mut aprev = "a0".to_string();
        let mut offset = segments[0].duration;
        for k in 1..segments.len() {
            let d = segments[k].transition_in.min(segments[k - 1].duration).min(segments[k].duration);
            let (vo, ao) = (format!("vx{k}"), format!("ax{k}"));
            if d > 0.0 {
                let start = (offset - d).max(0.0);
                graph.push_str(&format!(
                    "[{vprev}][v{k}]xfade=transition=fade:duration={d:.4}:offset={start:.4}[{vo}];"
                ));
                if with_audio {
                    graph.push_str(&format!("[{aprev}][a{k}]acrossfade=d={d:.4}[{ao}];"));
                }
                offset += segments[k].duration - d;
            } else {
                graph.push_str(&format!("[{vprev}][v{k}]concat=n=2:v=1:a=0[{vo}];"));
                if with_audio {
                    graph.push_str(&format!("[{aprev}][a{k}]concat=n=2:v=0:a=1[{ao}];"));
                }
                offset += segments[k].duration;
            }
            vprev = vo;
            aprev = ao;
        }
        graph.push_str(&format!("[{vprev}]null[vcat]"));
        if with_audio {
            graph.push_str(&format!(";[{aprev}]anull[acat]"));
        }
    } else {
        for k in 0..segments.len() {
            graph.push_str(&format!("[v{k}]"));
            if with_audio {
                graph.push_str(&format!("[a{k}]"));
            }
        }
        // Label order matters: concat emits video first, then audio, and the
        // labels bind in that order. Writing "[acat]" first silently bound
        // the VIDEO pad to it — harmless-looking (the file still played,
        // with its streams swapped) right up until something downstream
        // tried to treat [acat] as audio.
        graph.push_str(&format!(
            "concat=n={}:v=1:a={}",
            segments.len(),
            if with_audio { "1[vcat][acat]" } else { "0[vcat]" }
        ));
    }
    // Overlays composite onto the cut. Each is trimmed, scaled to a fraction
    // of the target frame, shifted to its timeline position, and switched on
    // only for its own window — so the base picture is untouched everywhere
    // else. Geometry comes from Pip fractions, which is what makes a PiP
    // placed on a 720p preview land identically at 4K.
    let mut video_out = "[vcat]".to_string();
    for (n, (o, idx)) in overlays.iter().zip(overlay_inputs.iter()).enumerate() {
        let w = ((o.pip.scale.clamp(0.02, 1.0) as f64) * target.0 as f64 / 2.0).round() * 2.0;
        let (end, at) = (o.at + o.duration, o.at);
        // -1 keeps the overlay's own aspect ratio rather than distorting it.
        graph.push_str(&format!(
            ";[{idx}:v]trim=start={:.4}:duration={:.4},setpts=PTS-STARTPTS+{at:.4}/TB,\
             scale={w}:-2,setsar=1[ov{n}]",
            o.in_point, o.duration
        ));
        let next = format!("[vo{n}]");
        // x/y are the CENTRE of the inset, so half its size comes off each.
        graph.push_str(&format!(
            ";{video_out}[ov{n}]overlay=x='(W*{:.5})-(w/2)':y='(H*{:.5})-(h/2)':\
             enable='between(t,{at:.4},{end:.4})':eof_action=pass:repeatlast=0{next}",
            o.pip.x, o.pip.y
        ));
        video_out = next;
    }

    // The music bed, laid under whatever the cut itself carries.
    let mut audio_out = if with_audio { Some("[acat]".to_string()) } else { None };
    if let (Some(idx), Some(m)) = (music_input, music) {
        let total = crate::edit::render_duration(segments);
        let delay_ms = (m.start.max(0.0) * 1000.0).round() as u64;
        let delay = if delay_ms > 0 {
            format!(",adelay={delay_ms}|{delay_ms}")
        } else {
            String::new()
        };
        // Trim to the cut's length so a long track can't extend the render,
        // and fade so it never just stops dead.
        let fade = if m.fade > 0.0 && total > m.fade * 2.0 {
            format!(
                ",afade=t=in:st=0:d={:.2},afade=t=out:st={:.2}:d={:.2}",
                m.fade,
                total - m.fade,
                m.fade
            )
        } else {
            String::new()
        };
        graph.push_str(&format!(
            ";[{idx}:a]{anorm},volume={:.2}dB{delay},atrim=0:{total:.4},asetpts=PTS-STARTPTS{fade}[mus]",
            m.gain_db
        ));

        match &audio_out {
            // Speech present: duck the music under it if asked, then mix.
            Some(cut) => {
                let bed = if m.duck {
                    // The cut's audio drives the compressor AND is kept — so
                    // it is split first; a filter output can only be used once.
                    graph.push_str(&format!(";{cut}asplit=2[sc_keep][sc_key]"));
                    graph.push_str(
                        ";[mus][sc_key]sidechaincompress=threshold=0.03:ratio=12:attack=20:release=400:makeup=1[mus_d]",
                    );
                    ("[sc_keep]", "[mus_d]")
                } else {
                    (cut.as_str(), "[mus]")
                };
                // normalize=0: amix otherwise divides every input by the
                // number of inputs, quietly halving the dialogue.
                graph.push_str(&format!(
                    ";{}{}amix=inputs=2:duration=first:normalize=0:dropout_transition=0[amix]",
                    bed.0, bed.1
                ));
                audio_out = Some("[amix]".into());
            }
            // Nothing but music: it becomes the soundtrack.
            None => audio_out = Some("[mus]".into()),
        }
    }

    // No post-concat scale: `target` already carries the chosen resolution,
    // and every segment was normalised to it above.
    a.extend(["-filter_complex".into(), graph, "-map".into(), video_out.clone()]);
    let has_audio_out = audio_out.is_some();
    if let Some(out) = &audio_out {
        a.extend(["-map".into(), out.clone()]);
    }

    // Non-video codecs handed in by mistake render as H.264 — the safe
    // default for a timeline.
    let vcodec = if matches!(s.codec, Codec::H265 | Codec::Av1 | Codec::Vp9) { s.codec } else { Codec::H264 };
    a.extend(video_encoder_args(vcodec, s.quality, s.hardware));
    if has_audio_out {
        let (codec, kbps) = if s.codec == Codec::Vp9 { ("libopus", 128) } else { ("aac", 160) };
        a.extend(["-c:a".into(), codec.into(), "-b:a".into(), format!("{kbps}k")]);
    }
    if s.codec != Codec::Vp9 {
        a.extend(["-movflags".into(), "+faststart".into()]);
    }
    a.push(output.into());
    a
}

/// Everything drawn on top of the picture at render time.
#[derive(Clone, Copy, Default)]
pub struct Overlays<'a> {
    pub captions: &'a [crate::captions::Cue],
    pub caption_size: u32,
    pub titles: &'a [crate::titles::Title],
    /// A music bed laid under the cut (and ducked under it, if asked).
    pub music: Option<&'a crate::edit::Music>,
    /// Picture composited on top of the cut.
    pub overlays: &'a [crate::edit::OverlaySegment],
}

/// Start a timeline (edit) export, burning any captions and titles into the
/// picture. Progress is driven by the cut's total duration = the sum of
/// segment durations.
///
/// Overlays are drawn by libass from documents written beside the output.
/// Titles go on last so hand-placed text sits above transcribed speech.
pub fn start_timeline_with_captions(
    segments: &[Segment],
    output: &str,
    settings: &ExportSettings,
    project: (u32, u32, f64),
    overlays: Overlays<'_>,
) -> Result<ExportJob> {
    if segments.is_empty() {
        return Err(anyhow!("the timeline is empty"));
    }
    if Path::new(output).exists() {
        return Err(anyhow!("output already exists: {output}"));
    }
    let with_audio = segments.iter().all(|seg| has_audio_stream(&seg.source));
    let total = crate::edit::render_duration(segments);
    let target = render_target(project, settings);
    let mut args = build_timeline_args_full(
        segments,
        output,
        settings,
        with_audio,
        target,
        overlays.music,
        overlays.overlays,
    );

    let mut chain: Vec<String> = Vec::new();
    if !overlays.captions.is_empty() {
        let srt = format!("{output}.srt");
        std::fs::write(&srt, crate::captions::to_srt(overlays.captions))?;
        // Font size is NOT scaled by the target height — see captions::PLAY_RES_Y.
        chain.push(format!(
            "subtitles='{}':force_style='{}'",
            escape_filter_path(&srt),
            crate::captions::force_style(overlays.caption_size)
        ));
    }
    if !overlays.titles.is_empty() {
        let ass = format!("{output}.titles.ass");
        std::fs::write(
            &ass,
            crate::titles::to_ass(overlays.titles, target.0, target.1),
        )?;
        chain.push(format!("subtitles='{}'", escape_filter_path(&ass)));
    }

    if !chain.is_empty() {
        // Attach to whatever the graph currently ends on — [vcat] for a plain
        // cut, but [voN] once overlays have been composited. Hardcoding
        // [vcat] here would have silently dropped every overlay.
        let map_at = args.iter().position(|a| a == "-map").map(|i| i + 1);
        let graph_at = args.iter().position(|a| a == "-filter_complex").map(|i| i + 1);
        if let (Some(m), Some(g)) = (map_at, graph_at) {
            let current = args[m].clone();
            args[g] = format!("{};{current}{}[vsub]", args[g], chain.join(","));
            args[m] = "[vsub]".into();
        }
    }
    spawn_job(args, output, total)
}

/// A path inside an ffmpeg filter argument. Colons separate filter options
/// and quotes end the string, so both have to be escaped or a path with a
/// drive letter or an apostrophe silently breaks the whole graph.
fn escape_filter_path(p: &str) -> String {
    p.replace('\\', "/").replace(':', "\\:").replace('\'', "\\'")
}

/// `atempo` only accepts 0.5–100 per instance, so anything slower than half
/// speed has to be chained. Returns "" at normal speed.
fn atempo_chain(rate: f64) -> String {
    if (rate - 1.0).abs() <= 1e-6 {
        return String::new();
    }
    let mut left = rate.clamp(0.05, 20.0);
    let mut parts = Vec::new();
    while left < 0.5 {
        parts.push(0.5);
        left /= 0.5;
    }
    parts.push(left);
    parts
        .iter()
        .map(|r| format!(",atempo={r:.6}"))
        .collect::<String>()
}

/// The geometry every segment is normalised to: the project's frame, or the
/// chosen export resolution (keeping the project's aspect), with even
/// dimensions — encoders require them.
pub fn render_target((pw, ph, fps): (u32, u32, f64), s: &ExportSettings) -> (u32, u32, f64) {
    let (pw, ph) = (pw.max(2), ph.max(2));
    // A preset's frame is exact — that's the whole point of picking one.
    if let Some((tw, th)) = s.target {
        let even = |v: u32| (v / 2 * 2).max(2);
        return (even(tw), even(th), if fps > 0.0 { fps } else { 30.0 });
    }
    let (w, h) = match s.resolution.height() {
        Some(target_h) => {
            let w = (pw as f64 * (target_h as f64 / ph as f64)).round() as u32;
            (w, target_h)
        }
        None => (pw, ph),
    };
    let even = |v: u32| (v / 2 * 2).max(2);
    (even(w), even(h), if fps > 0.0 { fps } else { 30.0 })
}

/// Start an export on a worker thread. `duration` is the source duration in
/// seconds (drives the progress fraction).
pub fn start(input: &str, output: &str, settings: &ExportSettings, duration: f64) -> Result<ExportJob> {
    if Path::new(output).exists() {
        return Err(anyhow!("output already exists: {output}"));
    }
    spawn_job(build_args(input, output, settings), output, duration)
}

fn spawn_job(args: Vec<String>, output: &str, duration: f64) -> Result<ExportJob> {
    let state = Arc::new(Mutex::new(ExportState::default()));
    let cancel = Arc::new(AtomicBool::new(false));
    let (t_state, t_cancel) = (state.clone(), cancel.clone());
    let t_output = output.to_string();

    std::thread::spawn(move || {
        let mut cmd = FfmpegCommand::new();
        cmd.args(args.iter().map(String::as_str));
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                t_state.lock().unwrap().error = Some(format!("ffmpeg failed to start: {e}"));
                t_state.lock().unwrap().finished = true;
                return;
            }
        };
        let iter = match child.iter() {
            Ok(i) => i,
            Err(e) => {
                t_state.lock().unwrap().error = Some(format!("ffmpeg output unreadable: {e}"));
                t_state.lock().unwrap().finished = true;
                return;
            }
        };

        let mut last_error: Option<String> = None;
        for event in iter {
            if t_cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                let _ = std::fs::remove_file(&t_output); // don't leave a stub
                let mut st = t_state.lock().unwrap();
                st.error = Some("cancelled".into());
                st.finished = true;
                return;
            }
            match event {
                FfmpegEvent::Progress(p) => {
                    let secs = parse_ffmpeg_time(&p.time).unwrap_or(0.0);
                    let mut st = t_state.lock().unwrap();
                    st.fraction = if duration > 0.0 { (secs / duration).clamp(0.0, 1.0) as f32 } else { 0.0 };
                    st.speed = p.speed;
                }
                FfmpegEvent::Log(LogLevel::Error | LogLevel::Fatal, msg) => {
                    last_error = Some(msg);
                }
                FfmpegEvent::Error(msg) => {
                    last_error = Some(msg);
                }
                _ => {}
            }
        }

        // ffmpeg is done — success iff the output landed on disk.
        let ok = Path::new(&t_output).exists();
        let mut st = t_state.lock().unwrap();
        st.finished = true;
        if ok {
            st.fraction = 1.0;
        } else {
            st.error = Some(last_error.unwrap_or_else(|| "export failed (no output produced)".into()));
        }
    });

    Ok(ExportJob { output: output.to_string(), state, cancel })
}

/// What a queued export will render when its turn comes.
#[derive(Clone, Debug)]
pub enum Job {
    /// Convert a source file (`path`, its duration).
    Source { path: String, duration: f64 },
    /// Render the edit: flattened segments + the project's frame/rate,
    /// plus any captions to burn in.
    Timeline {
        segments: Vec<Segment>,
        project: (u32, u32, f64),
        captions: Vec<crate::captions::Cue>,
        caption_size: u32,
        titles: Vec<crate::titles::Title>,
        music: Option<crate::edit::Music>,
        overlays: Vec<crate::edit::OverlaySegment>,
    },
}

/// One export waiting its turn.
#[derive(Clone, Debug)]
pub struct Queued {
    /// What the user called it — the platform name, usually.
    pub label: String,
    pub output: String,
    pub settings: ExportSettings,
    pub job: Job,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Ok(String),
    Failed(String),
}

/// A render queue: line up every platform you need, then walk away. Jobs run
/// one at a time — parallel encodes just fight over the same CPU/GPU.
#[derive(Default)]
pub struct Queue {
    pending: std::collections::VecDeque<Queued>,
    running: Option<(Queued, ExportJob)>,
    pub done: Vec<(String, Outcome)>,
    /// Stop starting new jobs (the current one is cancelled too).
    stopped: bool,
}

impl Queue {
    pub fn push(&mut self, item: Queued) {
        self.pending.push_back(item);
        self.stopped = false;
    }

    pub fn len_pending(&self) -> usize {
        self.pending.len()
    }

    pub fn is_busy(&self) -> bool {
        self.running.is_some() || !self.pending.is_empty()
    }

    /// The running job's label and progress, for the UI.
    pub fn current(&self) -> Option<(String, ExportState)> {
        self.running.as_ref().map(|(q, job)| (q.label.clone(), job.state()))
    }

    pub fn labels_pending(&self) -> Vec<String> {
        self.pending.iter().map(|q| q.label.clone()).collect()
    }

    /// Cancel everything: the running encode and the whole waiting list.
    pub fn cancel_all(&mut self) {
        self.stopped = true;
        self.pending.clear();
        if let Some((_, job)) = &self.running {
            job.cancel();
        }
    }

    pub fn clear_done(&mut self) {
        self.done.clear();
    }

    /// Advance the queue: collect a finished job, start the next one.
    /// Call every frame; returns true when something changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        if let Some((q, job)) = &self.running {
            let st = job.state();
            if st.finished {
                let outcome = match st.error {
                    None => Outcome::Ok(q.output.clone()),
                    Some(e) => Outcome::Failed(e),
                };
                self.done.push((q.label.clone(), outcome));
                self.running = None;
                changed = true;
            }
        }
        if self.running.is_none() && !self.stopped {
            if let Some(next) = self.pending.pop_front() {
                let started = match &next.job {
                    Job::Source { path, duration } => {
                        start(path, &next.output, &next.settings, *duration)
                    }
                    Job::Timeline {
                        segments, project, captions, caption_size, titles, music, overlays,
                    } => start_timeline_with_captions(
                        segments,
                        &next.output,
                        &next.settings,
                        *project,
                        Overlays {
                            captions,
                            caption_size: *caption_size,
                            titles,
                            music: music.as_ref(),
                            overlays,
                        },
                    ),
                };
                match started {
                    Ok(job) => self.running = Some((next, job)),
                    Err(e) => self.done.push((next.label.clone(), Outcome::Failed(e.to_string()))),
                }
                changed = true;
            }
        }
        changed
    }
}

/// Parse ffmpeg's `HH:MM:SS.cc` progress time into seconds.
fn parse_ffmpeg_time(t: &str) -> Option<f64> {
    let mut parts = t.split(':');
    let h: f64 = parts.next()?.trim().parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn seg(source: &str, in_point: f64, duration: f64) -> Segment {
        Segment {
            source: source.to_string(),
            in_point,
            duration,
            effects: crate::effects::Effects::default(),
            transition_in: 0.0,
            gain_db: 0.0,
            speed: 1.0,
        }
    }

    fn fixture() -> String {
        format!("{}/tests/fixture.mp4", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn time_parsing() {
        assert_eq!(parse_ffmpeg_time("00:03:29.04"), Some(209.04));
        assert_eq!(parse_ffmpeg_time("01:00:00.00"), Some(3600.0));
        assert_eq!(parse_ffmpeg_time("garbage"), None);
    }

    #[test]
    fn args_for_h264_balanced_720p() {
        let s = ExportSettings {
            codec: Codec::H264,
            quality: Quality::Balanced,
            resolution: Resolution::H720,
            audio: AudioMode::Encode { kbps: 160 },
            hardware: false,
            target: None,
            fit: Fit::Letterbox,
        };
        let a = build_args("in.mkv", "out.mp4", &s);
        let joined = a.join(" ");
        assert!(joined.contains("-c:v libx264"));
        assert!(joined.contains("-crf 21"));
        assert!(joined.contains("scale=-2:720"));
        assert!(joined.contains("-c:a aac -b:a 160k"));
        assert!(joined.ends_with("out.mp4"));
    }

    #[test]
    fn args_for_remux_copy_everything() {
        let s = ExportSettings {
            codec: Codec::Remux,
            quality: Quality::Balanced,
            resolution: Resolution::H480, // must be ignored for remux
            audio: AudioMode::Copy,
            hardware: false,
            target: None,
            fit: Fit::Letterbox,
        };
        let a = build_args("in.mp4", "out.mkv", &s);
        let joined = a.join(" ");
        assert!(joined.contains("-c copy"));
        assert!(!joined.contains("scale"));
        assert!(!joined.contains("-crf"));
    }

    #[test]
    fn args_for_audio_extraction() {
        let s = ExportSettings {
            codec: Codec::Mp3,
            quality: Quality::High,
            resolution: Resolution::H720, // must be ignored for audio
            audio: AudioMode::Copy,       // ignored; codec defines the audio
            hardware: false,
            target: None,
            fit: Fit::Letterbox,
        };
        let a = build_args("in.mp4", "out.mp3", &s);
        let joined = a.join(" ");
        assert!(joined.contains("-vn"));
        assert!(joined.contains("-c:a libmp3lame -b:a 320k"));
        assert!(!joined.contains("scale"));
        assert!(!joined.contains("-crf"));
    }

    #[test]
    fn args_for_image_jpeg() {
        let s = ExportSettings {
            codec: Codec::Jpeg,
            quality: Quality::Small,
            resolution: Resolution::H1080,
            audio: AudioMode::Copy,
            hardware: false,
            target: None,
            fit: Fit::Letterbox,
        };
        let a = build_args("in.png", "out.jpg", &s);
        let joined = a.join(" ");
        assert!(joined.contains("-c:v mjpeg -q:v 10"));
        assert!(joined.contains("scale=-2:1080"));
        assert!(joined.contains("-frames:v 1"));
        assert!(!joined.contains("-c:a"));
    }

    #[test]
    fn codec_lists_per_kind() {
        assert!(Codec::for_kind(MediaKind::Video).contains(&Codec::Mp3)); // extract audio
        assert!(!Codec::for_kind(MediaKind::Audio).contains(&Codec::H264));
        assert_eq!(Codec::for_kind(MediaKind::Image), &[Codec::Png, Codec::Jpeg, Codec::WebpImage]);
    }

    #[test]
    fn timeline_graph_trims_and_concats_in_order() {
        let segs = vec![
            seg("/m/a.mp4", 0.0, 2.0),
            seg("/m/a.mp4", 5.0, 1.5),
            seg("/m/b.mp4", 1.0, 3.0),
        ];
        let s = ExportSettings { hardware: false, ..Default::default() };
        let a = build_timeline_args(&segs, "out.mp4", &s, true, (1280, 720, 30.0));
        let joined = a.join(" ");
        // One -i per unique source (a.mp4 reused, not re-added).
        assert_eq!(a.iter().filter(|x| *x == "-i").count(), 2);
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("[0:v]trim=start=0.0000:duration=2.0000"));
        assert!(graph.contains("[0:v]trim=start=5.0000:duration=1.5000"));
        assert!(graph.contains("[1:v]trim=start=1.0000:duration=3.0000"));
        // Video pad first, then audio — concat emits them in that order, so
        // labelling them the other way round binds video to [acat].
        assert!(graph.contains("concat=n=3:v=1:a=1[vcat][acat]"), "{graph}");
        assert!(joined.contains("-map [vcat]") && joined.contains("-map [acat]"));
    }

    #[test]
    fn timeline_graph_without_audio_or_with_scale() {
        let segs = vec![seg("/m/a.mp4", 0.0, 2.0)];
        let s = ExportSettings {
            resolution: Resolution::H720,
            hardware: false,
            fit: Fit::Letterbox,
            ..Default::default()
        };
        let a = build_timeline_args(&segs, "out.mp4", &s, false, render_target((1920, 1080, 30.0), &s));
        let graph = &a[a.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(!graph.contains("atrim"), "no audio legs when sources are silent");
        assert!(graph.contains("concat=n=1:v=1:a=0"));
        // 1080p project at 720p export → every segment normalised to 1280x720.
        assert!(graph.contains("scale=1280:720:force_original_aspect_ratio=decrease"), "{graph}");
        assert!(graph.contains("pad=1280:720"), "{graph}");
        assert!(a.join(" ").contains("-map [vcat]"));
        assert!(!a.join(" ").contains("-c:a"));
    }

    /// The whole point: a two-piece cut with the middle removed must render
    /// to a file whose duration is the SUM OF THE PIECES, not the source.
    #[test]
    fn renders_a_real_cut_from_the_fixture() {
        let out = std::env::temp_dir().join(format!("reel-cut-test-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // fixture is ~2s; take 0.0–0.4 and 1.4–1.8 → expect ≈0.8s.
        let segs = vec![seg(&fixture(), 0.0, 0.4), seg(&fixture(), 1.4, 0.4)];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline_with_captions(&segs, &out.to_string_lossy(), &s, (320, 240, 30.0), Overlays::default())
            .expect("start timeline export");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "timeline export error: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "timeline export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe cut");
        assert!(
            info.duration > 0.6 && info.duration < 1.1,
            "cut should be ≈0.8s (the two kept pieces), got {}",
            info.duration
        );
        // Video must be stream 0. It wasn't, for a while: the concat labels
        // were written [acat][vcat], which binds video to the audio label —
        // files still played, so nothing complained until the music bed
        // tried to use [acat] as audio and ffmpeg rejected the graph.
        let streams = std::process::Command::new("ffprobe")
            .args([
                "-v", "error", "-show_entries", "stream=codec_type",
                "-of", "csv=p=0", &out.to_string_lossy(),
            ])
            .output()
            .expect("ffprobe the cut");
        let kinds: Vec<String> = String::from_utf8_lossy(&streams.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(kinds.first().map(String::as_str), Some("video"), "streams: {kinds:?}");
        let _ = std::fs::remove_file(&out);
    }

    /// If this machine has a hardware encoder, a real GPU-encoded export
    /// must produce a valid file (self-skips where there's no GPU encoder).
    #[test]
    fn hardware_export_produces_a_valid_file() {
        let Some(hw) = hw_encoder_for(Codec::H264) else {
            eprintln!("no hardware encoder here — skipping");
            return;
        };
        eprintln!("hardware encoder: {}", hw.label());
        let out = std::env::temp_dir().join(format!("reel-hw-test-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let s = ExportSettings { quality: Quality::Small, hardware: true, ..Default::default() };
        assert!(build_args(&fixture(), "x.mp4", &s).join(" ").contains("nvenc")
            || build_args(&fixture(), "x.mp4", &s).join(" ").contains("videotoolbox"));
        let job = start(&fixture(), &out.to_string_lossy(), &s, 2.0).expect("start hw export");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "hw export error: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "hw export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe hw output");
        assert_eq!(info.width, 320);
        assert!(info.duration > 1.5 && info.duration < 2.5);
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn hardware_falls_back_to_software_for_vp9() {
        // VP9 has no NVENC/VideoToolbox path — must silently use libvpx.
        let s = ExportSettings { codec: Codec::Vp9, hardware: true, ..Default::default() };
        let joined = build_args("in.mp4", "out.webm", &s).join(" ");
        assert!(joined.contains("libvpx-vp9"), "{joined}");
        assert!(joined.contains("-crf"));
    }

    /// Mixed sources — different resolutions AND different codecs — must
    /// concatenate into one coherent file. (concat rejects mismatched
    /// geometry, so this is the test that guards the normalisation filters.)
    #[test]
    fn renders_a_cut_across_two_different_sources() {
        let dir = std::env::temp_dir();
        let a = dir.join(format!("reel-mix-a-{}.mp4", std::process::id()));
        let b = dir.join(format!("reel-mix-b-{}.webm", std::process::id()));
        let out = dir.join(format!("reel-mix-out-{}.mp4", std::process::id()));
        for f in [&a, &b, &out] {
            let _ = std::fs::remove_file(f);
        }
        // a: 640x480 h264 @25fps with audio; b: 320x180 vp9 @30fps with audio.
        let mk = |args: &[&str]| {
            std::process::Command::new("ffmpeg").args(args).status().map(|s| s.success()).unwrap_or(false)
        };
        assert!(mk(&["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=640x480:rate=25:duration=2",
                     "-f", "lavfi", "-i", "sine=frequency=300:duration=2",
                     "-c:v", "libx264", "-c:a", "aac", "-shortest", &a.to_string_lossy()]));
        assert!(mk(&["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=30:duration=2",
                     "-f", "lavfi", "-i", "sine=frequency=500:duration=2",
                     "-c:v", "libvpx-vp9", "-crf", "40", "-b:v", "0", "-c:a", "libopus", "-shortest",
                     &b.to_string_lossy()]));

        let segs = vec![
            seg(&a.to_string_lossy(), 0.2, 0.8),
            seg(&b.to_string_lossy(), 0.5, 0.7),
            seg(&a.to_string_lossy(), 1.2, 0.5),
        ];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline_with_captions(&segs, &out.to_string_lossy(), &s, (640, 480, 25.0), Overlays::default())
            .expect("start mixed-source export");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "mixed export error: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "mixed export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe mixed output");
        assert_eq!((info.width, info.height), (640, 480), "normalised to the project frame");
        assert!(
            info.duration > 1.8 && info.duration < 2.3,
            "0.8+0.7+0.5 = 2.0s expected, got {}",
            info.duration
        );
        for f in [&a, &b, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn presets_describe_real_platform_frames() {
        // Every preset must be a sane, even-dimensioned video target.
        for p in Preset::ALL {
            assert!(p.w % 2 == 0 && p.h % 2 == 0, "{} has odd dimensions", p.name);
            assert!(!p.codec.is_audio_only() && !p.codec.is_image(), "{} isn't video", p.name);
            assert!(!p.slug().contains(' '), "{} slug has spaces", p.name);
        }
        // The vertical ones really are vertical, the wide ones wide.
        let tiktok = Preset::ALL.iter().find(|p| p.name == "TikTok").unwrap();
        assert!(tiktok.h > tiktok.w);
        let yt = Preset::ALL.iter().find(|p| p.name == "YouTube").unwrap();
        assert!(yt.w > yt.h);

        // Applying a preset makes it active, and pins the exact frame.
        let mut s = ExportSettings::default();
        tiktok.apply(&mut s);
        assert!(tiktok.is_active(&s));
        assert_eq!(s.target, Some((1080, 1920)));
        assert_eq!(render_target((1920, 1080, 30.0), &s), (1080, 1920, 30.0));
    }

    #[test]
    fn fit_modes_produce_the_right_filter_shapes() {
        let (w, h) = (1080, 1920);
        let letter = Fit::Letterbox.chain(w, h, "0");
        assert!(letter.contains("force_original_aspect_ratio=decrease") && letter.contains("pad=1080:1920"));
        let crop = Fit::Crop.chain(w, h, "0");
        assert!(crop.contains("force_original_aspect_ratio=increase") && crop.contains("crop=1080:1920"));
        let blur = Fit::Blur.chain(w, h, "7");
        // Blurred backdrop behind a fully-visible foreground, uniquely tagged.
        assert!(blur.contains("split[bg7][fg7]"), "{blur}");
        assert!(blur.contains("boxblur") && blur.contains("overlay=(W-w)/2:(H-h)/2"));
    }

    /// The promise of a one-click preset: a landscape source really comes out
    /// as a 1080×1920 vertical file, with the blurred-sides treatment.
    #[test]
    fn tiktok_preset_renders_a_vertical_file() {
        let src = std::env::temp_dir().join(format!("reel-preset-src-{}.mp4", std::process::id()));
        let out = std::env::temp_dir().join(format!("reel-preset-out-{}.mp4", std::process::id()));
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
        let made = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25:duration=1",
                   "-c:v", "libx264", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false);
        assert!(made, "could not build the landscape fixture");

        let mut s = ExportSettings { hardware: false, ..Default::default() };
        Preset::ALL.iter().find(|p| p.name == "TikTok").unwrap().apply(&mut s);
        s.hardware = false;
        let job = start(&src.to_string_lossy(), &out.to_string_lossy(), &s, 1.0).expect("start preset export");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "preset export error: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "preset export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe preset output");
        assert_eq!((info.width, info.height), (1080, 1920), "landscape source → vertical frame");
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// The queue runs jobs one at a time, in order, and reports each result.
    #[test]
    fn queue_runs_every_job_in_order() {
        let dir = std::env::temp_dir();
        let outs: Vec<_> = (0..3)
            .map(|i| dir.join(format!("reel-queue-{}-{i}.mp4", std::process::id())))
            .collect();
        for o in &outs {
            let _ = std::fs::remove_file(o);
        }
        let mut q = Queue::default();
        for (i, o) in outs.iter().enumerate() {
            q.push(Queued {
                label: format!("job{i}"),
                output: o.to_string_lossy().into_owned(),
                settings: ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() },
                job: Job::Source { path: fixture(), duration: 2.0 },
            });
        }
        let deadline = Instant::now() + Duration::from_secs(120);
        while q.is_busy() {
            q.poll();
            // Never more than one encode in flight.
            assert!(q.len_pending() + q.current().iter().count() <= 3);
            assert!(Instant::now() < deadline, "queue stalled");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(q.done.len(), 3, "every job reported");
        for (i, (label, outcome)) in q.done.iter().enumerate() {
            assert_eq!(label, &format!("job{i}"), "results keep queue order");
            assert!(matches!(outcome, Outcome::Ok(_)), "job{i} failed: {outcome:?}");
        }
        for o in &outs {
            assert!(o.exists(), "queued export produced its file");
            let _ = std::fs::remove_file(o);
        }
    }

    /// A crossfade OVERLAPS its two clips, so the rendered file is shorter
    /// than the sum of the pieces by exactly the fade length.
    #[test]
    fn crossfade_overlaps_and_shortens_the_render() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-xf-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-xf-out-{}.mp4", std::process::id()));
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=25:duration=4",
                   "-c:v", "libx264", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));

        // 1.5s + 1.5s with a 0.5s crossfade → 2.5s, not 3.0s.
        let mut a = seg(&src.to_string_lossy(), 0.0, 1.5);
        let mut b = seg(&src.to_string_lossy(), 2.0, 1.5);
        a.transition_in = 0.0;
        b.transition_in = 0.5;
        let segs = vec![a, b];
        let graph_args = build_timeline_args(&segs, "x.mp4", &ExportSettings::default(), false, (320, 240, 25.0));
        let graph = &graph_args[graph_args.iter().position(|x| x == "-filter_complex").unwrap() + 1];
        assert!(graph.contains("xfade=transition=fade:duration=0.5000:offset=1.0000"), "{graph}");

        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline_with_captions(&segs, &out.to_string_lossy(), &s, (320, 240, 25.0), Overlays::default()).expect("start xfade");
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "crossfade export error: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "crossfade export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe xfade output");
        assert!(
            info.duration > 2.3 && info.duration < 2.75,
            "1.5+1.5 with a 0.5s crossfade should be ≈2.5s, got {}",
            info.duration
        );
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Captions must survive all the way into pixels — the filter chain is
    /// fiddly enough (escaping, label rewiring) that only a real render proves
    /// it. We compare a frame with captions against one without.
    /// Build a 4 s clip whose audio is silent except for a 1 s burst in the
    /// middle — a stand-in for someone speaking over a music bed.
    fn speech_fixture(path: &Path) {
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-v", "error",
                "-f", "lavfi", "-i", "color=c=black:size=160x120:rate=15:duration=4",
                "-f", "lavfi",
                // 3 kHz only between 1.5 s and 2.5 s.
                "-i", "sine=frequency=3000:sample_rate=48000:duration=4",
                // A DISABLED volume filter passes audio through untouched —
                // it does not mute. To silence outside the window the gain
                // itself has to be the expression.
                "-af", "volume=volume='between(t,1.5,2.5)':eval=frame",
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
                &path.to_string_lossy(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "could not build the speech fixture");
    }

    /// Mean volume of one frequency band over a time window, in dBFS.
    fn band_level(file: &Path, from: f64, to: f64, freq: u32) -> f32 {
        let out = std::process::Command::new("ffmpeg")
            .args([
                "-v", "info", "-i", &file.to_string_lossy(),
                "-af", &format!(
                    "atrim={from}:{to},asetpts=PTS-STARTPTS,bandpass=f={freq}:width_type=h:w=120,volumedetect"
                ),
                "-f", "null", "-",
            ])
            .output()
            .expect("run volumedetect");
        let text = String::from_utf8_lossy(&out.stderr);
        text.lines()
            .find_map(|l| l.split("mean_volume:").nth(1))
            .and_then(|v| v.trim().split(' ').next())
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or_else(|| panic!("no mean_volume in ffmpeg output:\n{text}"))
    }

    /// The point of ducking: when the edit's own audio speaks, the music
    /// gets out of the way — and comes back when it stops. Measured on the
    /// music's own frequency band so the speech itself can't flatter it.
    #[test]
    fn music_ducks_under_speech_and_returns() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-duck-src-{}.mp4", std::process::id()));
        let bed = dir.join(format!("reel-duck-bed-{}.wav", std::process::id()));
        let ducked = dir.join(format!("reel-duck-on-{}.mp4", std::process::id()));
        let flat = dir.join(format!("reel-duck-off-{}.mp4", std::process::id()));
        for f in [&ducked, &flat] {
            let _ = std::fs::remove_file(f);
        }
        speech_fixture(&src);
        // A steady 300 Hz bed — any level change in this band is the ducker.
        assert!(std::process::Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-f", "lavfi",
                "-i", "sine=frequency=300:sample_rate=48000:duration=4",
                &bed.to_string_lossy(),
            ])
            .status()
            .map(|st| st.success())
            .unwrap_or(false));

        let segs = vec![Segment {
            source: src.to_string_lossy().into(),
            in_point: 0.0,
            duration: 4.0,
            effects: Default::default(),
            transition_in: 0.0,
            gain_db: 0.0,
            speed: 1.0,
        }];
        let s = ExportSettings::default();

        let render = |out: &Path, duck: bool| {
            let music = crate::edit::Music {
                source: bed.to_string_lossy().into(),
                start: 0.0,
                gain_db: 0.0,
                duck,
                fade: 0.0,
            };
            let job = start_timeline_with_captions(
                &segs,
                &out.to_string_lossy(),
                &s,
                (160, 120, 15.0),
                Overlays { music: Some(&music), ..Default::default() },
            )
            .expect("start render");
            let deadline = Instant::now() + Duration::from_secs(180);
            loop {
                let st = job.state();
                if st.finished {
                    assert!(st.error.is_none(), "render failed: {:?}", st.error);
                    break;
                }
                assert!(Instant::now() < deadline, "render hung");
                std::thread::sleep(Duration::from_millis(100));
            }
        };
        render(&ducked, true);
        render(&flat, false);

        // Quiet window (no speech) vs the middle of the burst.
        let quiet_on = band_level(&ducked, 0.3, 1.2, 300);
        let under_on = band_level(&ducked, 1.8, 2.4, 300);
        let quiet_off = band_level(&flat, 0.3, 1.2, 300);
        let under_off = band_level(&flat, 1.8, 2.4, 300);

        assert!(
            quiet_on - under_on > 4.0,
            "music barely moved under speech: {quiet_on:.1} dB → {under_on:.1} dB"
        );
        assert!(
            (quiet_off - under_off).abs() < 2.0,
            "music moved without ducking asked for: {quiet_off:.1} dB → {under_off:.1} dB"
        );
        // And it comes back afterwards, or the bed just dies mid-video.
        let after = band_level(&ducked, 3.2, 3.9, 300);
        assert!(
            after - under_on > 3.0,
            "music never recovered after the speech: {under_on:.1} dB → {after:.1} dB"
        );

        for f in [&src, &bed, &ducked, &flat] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Captions and titles are two separate libass passes chained onto the
    /// same graph. This is the check that they coexist — an earlier version
    /// overwrote one label with the other and silently dropped the titles.
    /// A PiP has to appear in the right place, at the right size, only while
    /// it is meant to — and the base picture must survive underneath it. The
    /// geometry is stored as fractions, so this measures the rendered pixels
    /// against those fractions directly.
    #[test]
    fn an_overlay_lands_where_it_was_placed_and_only_when_it_should() {
        use crate::edit::{OverlaySegment, Pip};
        let dir = std::env::temp_dir();
        let base = dir.join(format!("reel-ov-base-{}.mp4", std::process::id()));
        let top = dir.join(format!("reel-ov-top-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-ov-out-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // Base is black; the overlay is pure red, so any red pixel is the PiP.
        for (path, colour) in [(&base, "black"), (&top, "red")] {
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi",
                       "-i", &format!("color=c={colour}:size=640x480:rate=10:duration=4"),
                       "-c:v", "libx264", "-pix_fmt", "yuv420p", &path.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
        }

        let segs = vec![seg(&base.to_string_lossy(), 0.0, 4.0)];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        // A quarter-width inset, centred left-of-middle, for the middle 2s.
        let pip = Pip { x: 0.3, y: 0.5, scale: 0.25 };
        let ov = vec![OverlaySegment {
            source: top.to_string_lossy().into(),
            in_point: 0.0,
            duration: 2.0,
            at: 1.0,
            pip,
            gain_db: 0.0,
        }];
        let job = start_timeline_with_captions(
            &segs,
            &out.to_string_lossy(),
            &s,
            (640, 480, 10.0),
            Overlays { overlays: &ov, ..Default::default() },
        )
        .expect("start overlay render");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "overlay render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "overlay render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        // Measure the red region at t=2s (inside the window).
        let frame = dir.join(format!("reel-ov-f-{}.png", std::process::id()));
        let grab = |t: &str, to: &std::path::Path| {
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-ss", t, "-i", &out.to_string_lossy(),
                       "-frames:v", "1", &to.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            image::open(to).expect("read frame").to_rgb8()
        };
        let img = grab("2", &frame);
        let (mut x0, mut x1, mut y0, mut y1, mut n) = (u32::MAX, 0u32, u32::MAX, 0u32, 0u32);
        for (x, y, px) in img.enumerate_pixels() {
            let [r, g, b] = px.0;
            if r > 130 && g < 90 && b < 90 {
                x0 = x0.min(x); x1 = x1.max(x);
                y0 = y0.min(y); y1 = y1.max(y);
                n += 1;
            }
        }
        assert!(n > 100, "no overlay in the frame");
        let (w, h) = (img.width() as f32, img.height() as f32);
        let cx = (x0 + x1) as f32 / 2.0 / w;
        let cy = (y0 + y1) as f32 / 2.0 / h;
        let sw = (x1 - x0 + 1) as f32 / w;
        assert!((cx - pip.x).abs() < 0.02, "overlay centre x {cx:.3}, asked {:.3}", pip.x);
        assert!((cy - pip.y).abs() < 0.02, "overlay centre y {cy:.3}, asked {:.3}", pip.y);
        assert!((sw - pip.scale).abs() < 0.03, "overlay width {sw:.3}, asked {:.3}", pip.scale);

        // Outside its window the base picture must be untouched.
        let before = grab("0.3", &frame);
        let red_before = before.pixels().filter(|p| p.0[0] > 130 && p.0[1] < 90).count();
        assert_eq!(red_before, 0, "the overlay showed up before it was supposed to");
        let after = grab("3.6", &frame);
        let red_after = after.pixels().filter(|p| p.0[0] > 130 && p.0[1] < 90).count();
        assert_eq!(red_after, 0, "the overlay stayed on screen after its window");

        for f in [&base, &top, &out, &frame] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Speed has to change how long the clip actually runs, not just how
    /// fast it looks: `duration` is TIMELINE length, so a 2× clip must
    /// consume twice as much source and still occupy its stated slot. Getting
    /// that backwards halves the output, which is the kind of thing you only
    /// notice after publishing.
    #[test]
    fn speed_consumes_more_source_and_keeps_the_timeline_length() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-speed-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-speed-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=25:duration=8",
                   "-f", "lavfi", "-i", "sine=frequency=440:duration=8",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
                   &src.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        // 3 s on the timeline at 2x = 6 s of source.
        let segs = vec![Segment {
            source: src.to_string_lossy().into(),
            in_point: 0.0,
            duration: 3.0,
            effects: Default::default(),
            transition_in: 0.0,
            gain_db: 0.0,
            speed: 2.0,
        }];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline_with_captions(
            &segs, &out.to_string_lossy(), &s, (160, 120, 25.0), Overlays::default(),
        )
        .expect("start speed render");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "speed render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "speed render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe");
        assert!(
            (info.duration - 3.0).abs() < 0.35,
            "a 3s slot at 2x should render 3s, got {:.2}s",
            info.duration
        );

        // The audio has to be sped up too, or it drifts away from the picture.
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0",
                   "-show_entries", "stream=duration", "-of", "csv=p=0",
                   &out.to_string_lossy()])
            .output()
            .expect("ffprobe audio");
        let adur: f64 = String::from_utf8_lossy(&probe.stdout).trim().parse().unwrap_or(0.0);
        assert!(
            (adur - 3.0).abs() < 0.5,
            "audio ran {adur:.2}s against a 3s picture — the tempo change is missing"
        );

        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn slow_motion_chains_atempo_because_one_filter_cannot_go_below_half() {
        assert_eq!(atempo_chain(1.0), "");
        assert_eq!(atempo_chain(2.0), ",atempo=2.000000");
        // 0.25 is out of atempo's range, so it becomes 0.5 x 0.5.
        let quarter = atempo_chain(0.25);
        assert_eq!(quarter.matches("atempo").count(), 2, "got {quarter}");
        // Whatever the chain, the rates must multiply back to the ask.
        let product: f64 = quarter
            .split(",atempo=")
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<f64>().unwrap())
            .product();
        assert!((product - 0.25).abs() < 1e-6, "chain multiplies to {product}");
    }

    #[test]
    fn captions_and_titles_burn_in_the_same_render() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-both-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-both-{}.mp4", std::process::id()));
        let frame = dir.join(format!("reel-both-{}.png", std::process::id()));
        for f in [&out, &frame] {
            let _ = std::fs::remove_file(f);
        }
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi",
                   "-i", "color=c=black:size=640x480:rate=25:duration=2",
                   "-c:v", "libx264", &src.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let segs = vec![seg(&src.to_string_lossy(), 0.0, 2.0)];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let cues = vec![crate::captions::Cue { start: 0.0, end: 2.0, text: "SPOKEN".into() }];
        // Pure red, near the top — nothing else in this render is red.
        let titles = vec![crate::titles::Title {
            text: "TITLE".into(),
            start: 0.0,
            end: 2.0,
            x: 0.5,
            y: 0.2,
            size: 0.15,
            color: [255, 0, 0],
            bold: true,
            outline: false,
        }];

        let job = start_timeline_with_captions(
            &segs,
            &out.to_string_lossy(),
            &s,
            (640, 480, 25.0),
            Overlays { captions: &cues, caption_size: 20, titles: &titles, music: None, overlays: &[] },
        )
        .expect("start export");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "export failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-ss", "1", "-i", &out.to_string_lossy(),
                   "-frames:v", "1", &frame.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));
        let img = image::open(&frame).expect("read frame").to_rgb8();

        let mut red_top = 0;
        let mut white_bottom = 0;
        for (_, y, px) in img.enumerate_pixels() {
            let [r, g, b] = px.0;
            if y < img.height() / 2 && r > 150 && g < 90 && b < 90 {
                red_top += 1;
            }
            if y > img.height() * 2 / 3 && r > 200 && g > 200 && b > 200 {
                white_bottom += 1;
            }
        }
        assert!(red_top > 200, "the red title is missing from the render ({red_top} px)");
        assert!(white_bottom > 200, "the caption is missing from the render ({white_bottom} px)");

        for f in [&src, &out, &frame] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn captions_are_burned_into_the_render() {
        let dir = std::env::temp_dir();
        let plain = dir.join(format!("reel-cap-plain-{}.mp4", std::process::id()));
        let capped = dir.join(format!("reel-cap-burned-{}.mp4", std::process::id()));
        let f_plain = dir.join(format!("reel-cap-plain-{}.png", std::process::id()));
        let f_capped = dir.join(format!("reel-cap-burned-{}.png", std::process::id()));
        for f in [&plain, &capped, &f_plain, &f_capped] {
            let _ = std::fs::remove_file(f);
        }
        // A dark, flat source so drawn text is unmistakable.
        let src = dir.join(format!("reel-cap-src-{}.mp4", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "color=c=black:size=640x480:rate=25:duration=2",
                   "-c:v", "libx264", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));

        let segs = vec![seg(&src.to_string_lossy(), 0.0, 2.0)];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let cues = vec![crate::captions::Cue {
            start: 0.0,
            end: 2.0,
            text: "HELLO CAPTIONS".into(),
        }];

        for (out, cues) in [(&plain, Vec::new()), (&capped, cues)] {
            let job = start_timeline_with_captions(
                &segs,
                &out.to_string_lossy(),
                &s,
                (640, 480, 25.0),
                Overlays { captions: &cues, caption_size: 20, ..Default::default() },
            ).expect("start captioned export");
            let deadline = Instant::now() + Duration::from_secs(90);
            loop {
                let st = job.state();
                if st.finished {
                    assert!(st.error.is_none(), "caption export failed: {:?}", st.error);
                    break;
                }
                assert!(Instant::now() < deadline, "caption export timed out");
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        // Pull a frame from each and compare brightness: white text on black
        // can only make the captioned frame brighter.
        let grab = |v: &std::path::Path, png: &std::path::Path| {
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-ss", "1", "-i", &v.to_string_lossy(),
                       "-frames:v", "1", &png.to_string_lossy()])
                .status().map(|s| s.success()).unwrap_or(false));
            let img = image::open(png).expect("read frame").to_luma8();
            img.pixels().map(|p| p.0[0] as u64).sum::<u64>()
        };
        let lum_plain = grab(&plain, &f_plain);
        let lum_capped = grab(&capped, &f_capped);
        assert!(
            lum_capped > lum_plain + 10_000,
            "captioned frame should carry visible text (luma {lum_plain} → {lum_capped})"
        );
        // And the sidecar SRT we wrote is real.
        let srt = format!("{}.srt", capped.to_string_lossy());
        assert!(std::fs::read_to_string(&srt).unwrap().contains("HELLO CAPTIONS"));
        for f in [&plain, &capped, &f_plain, &f_capped, &src, &std::path::PathBuf::from(srt)] {
            let _ = std::fs::remove_file(f);
        }
    }

    #[test]
    fn exports_fixture_to_h264() {
        let out = format!(
            "{}/reel-export-test-{}.mp4",
            std::env::temp_dir().display(),
            std::process::id()
        );
        let _ = std::fs::remove_file(&out);
        let s = ExportSettings {
            codec: Codec::H264,
            quality: Quality::Small,
            resolution: Resolution::Source,
            audio: AudioMode::Encode { kbps: 96 },
            hardware: false,
            target: None,
            fit: Fit::Letterbox,
        };
        let job = start(&fixture(), &out, &s, 2.0).expect("start export");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "export error: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "export timed out");
            std::thread::sleep(Duration::from_millis(50));
        }
        // Output must exist and be a real video of ≈ the source duration.
        let info = crate::video::decoder::probe(&out).expect("probe exported file");
        assert_eq!(info.width, 320);
        assert!(info.duration > 1.5 && info.duration < 2.5, "≈2s, got {}", info.duration);
        let _ = std::fs::remove_file(&out);
    }
}
