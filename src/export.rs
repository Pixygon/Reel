//! Export / convert — the HandBrake seam. One source file in, one encoded file
//! out, straight from the player (no editor round-trip). Runs ffmpeg on a
//! worker thread and reports live progress; the UI polls `ExportJob::state()`.
//! Timeline (composited) export is Milestone 3 — this is source-file convert.

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
pub fn build_timeline_args(
    segments: &[(String, f64, f64)],
    output: &str,
    s: &ExportSettings,
    with_audio: bool,
    target: (u32, u32, f64),
) -> Vec<String> {
    let (tw, th, tfps) = target;
    let mut a: Vec<String> = Vec::new();
    // One -i per unique source, in first-appearance order.
    let mut sources: Vec<&str> = Vec::new();
    for (src, _, _) in segments {
        if !sources.contains(&src.as_str()) {
            sources.push(src);
        }
    }
    for src in &sources {
        a.extend(["-i".into(), (*src).into()]);
    }

    // concat demands identical geometry/rate/audio format across segments, so
    // every segment is normalised to the target frame with the chosen fit
    // (never distorting), square pixels, one frame rate, one audio format.
    // This is what makes mixing differently-sized sources work — and what
    // reshapes a whole edit into a preset's frame.
    let anorm = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";

    let mut graph = String::new();
    for (k, (src, in_point, duration)) in segments.iter().enumerate() {
        let i = sources.iter().position(|s| s == src).unwrap();
        let vnorm = format!("{},fps={tfps:.4}", s.fit.chain(tw, th, &k.to_string()));
        graph.push_str(&format!(
            "[{i}:v]trim=start={in_point:.4}:duration={duration:.4},setpts=PTS-STARTPTS,{vnorm}[v{k}];"
        ));
        if with_audio {
            graph.push_str(&format!(
                "[{i}:a]atrim=start={in_point:.4}:duration={duration:.4},asetpts=PTS-STARTPTS,{anorm}[a{k}];"
            ));
        }
    }
    for k in 0..segments.len() {
        graph.push_str(&format!("[v{k}]"));
        if with_audio {
            graph.push_str(&format!("[a{k}]"));
        }
    }
    graph.push_str(&format!(
        "concat=n={}:v=1:a={}[vcat]",
        segments.len(),
        if with_audio { "1[acat]" } else { "0" }
    ));
    // No post-concat scale: `target` already carries the chosen resolution,
    // and every segment was normalised to it above.
    a.extend(["-filter_complex".into(), graph, "-map".into(), "[vcat]".into()]);
    if with_audio {
        a.extend(["-map".into(), "[acat]".into()]);
    }

    // Non-video codecs handed in by mistake render as H.264 — the safe
    // default for a timeline.
    let vcodec = if matches!(s.codec, Codec::H265 | Codec::Av1 | Codec::Vp9) { s.codec } else { Codec::H264 };
    a.extend(video_encoder_args(vcodec, s.quality, s.hardware));
    if with_audio {
        let (codec, kbps) = if s.codec == Codec::Vp9 { ("libopus", 128) } else { ("aac", 160) };
        a.extend(["-c:a".into(), codec.into(), "-b:a".into(), format!("{kbps}k")]);
    }
    if s.codec != Codec::Vp9 {
        a.extend(["-movflags".into(), "+faststart".into()]);
    }
    a.push(output.into());
    a
}

/// Start a timeline (edit) export. Progress is driven by the cut's total
/// duration = the sum of segment durations.
pub fn start_timeline(
    segments: &[(String, f64, f64)],
    output: &str,
    settings: &ExportSettings,
    project: (u32, u32, f64),
) -> Result<ExportJob> {
    if segments.is_empty() {
        return Err(anyhow!("the timeline is empty"));
    }
    if Path::new(output).exists() {
        return Err(anyhow!("output already exists: {output}"));
    }
    let with_audio = segments.iter().all(|(src, _, _)| has_audio_stream(src));
    let total: f64 = segments.iter().map(|(_, _, d)| d).sum();
    let args = build_timeline_args(segments, output, settings, with_audio, render_target(project, settings));
    spawn_job(args, output, total)
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
            ("/m/a.mp4".to_string(), 0.0, 2.0),
            ("/m/a.mp4".to_string(), 5.0, 1.5),
            ("/m/b.mp4".to_string(), 1.0, 3.0),
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
        assert!(graph.contains("concat=n=3:v=1:a=1[acat]"), "{graph}");
        assert!(joined.contains("-map [vcat]") && joined.contains("-map [acat]"));
    }

    #[test]
    fn timeline_graph_without_audio_or_with_scale() {
        let segs = vec![("/m/a.mp4".to_string(), 0.0, 2.0)];
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
        let segs = vec![
            (fixture(), 0.0, 0.4),
            (fixture(), 1.4, 0.4),
        ];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline(&segs, &out.to_string_lossy(), &s, (320, 240, 30.0))
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
            (a.to_string_lossy().into_owned(), 0.2, 0.8),
            (b.to_string_lossy().into_owned(), 0.5, 0.7),
            (a.to_string_lossy().into_owned(), 1.2, 0.5),
        ];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline(&segs, &out.to_string_lossy(), &s, (640, 480, 25.0))
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
