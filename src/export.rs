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
    pub(crate) fn chain(self, w: u32, h: u32, tag: &str) -> String {
        match self {
            Fit::Letterbox => format!(
                // The pad is TRANSPARENT: in the frame server the bars keep
                // alpha 0, so per-clip grading (LUTs, keys) colours the
                // picture and never the bars. In the yuv graph path alpha
                // drops and the pad is plain black — identical look.
                "scale={w}:{h}:force_original_aspect_ratio=decrease:flags=lanczos,\
                 pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black@0,setsar=1"
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
    /// The platform's loudness target, LUFS.
    pub loudness: Option<f32>,
}

impl Preset {
    /// Vertical 9:16 is the shape of TikTok / Reels / Shorts; 1:1 and 4:5 are
    /// the feed shapes; 16:9 is YouTube and X.
    pub const ALL: &'static [Preset] = &[
        Preset { name: "YouTube", note: "1080p · 16:9", w: 1920, h: 1080, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::High, loudness: Some(-14.0) },
        Preset { name: "YouTube 4K", note: "2160p · 16:9", w: 3840, h: 2160, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::High, loudness: Some(-14.0) },
        Preset { name: "TikTok", note: "1080×1920 · 9:16", w: 1080, h: 1920, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced, loudness: Some(-14.0) },
        Preset { name: "Reels / Shorts", note: "1080×1920 · 9:16", w: 1080, h: 1920, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced, loudness: Some(-14.0) },
        Preset { name: "Instagram feed", note: "1080×1350 · 4:5", w: 1080, h: 1350, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced, loudness: Some(-14.0) },
        Preset { name: "Square", note: "1080×1080 · 1:1", w: 1080, h: 1080, fit: Fit::Blur, codec: Codec::H264, quality: Quality::Balanced, loudness: Some(-14.0) },
        Preset { name: "Facebook", note: "1080p · 16:9", w: 1920, h: 1080, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::Balanced, loudness: Some(-14.0) },
        Preset { name: "X / Twitter", note: "720p · 16:9", w: 1280, h: 720, fit: Fit::Letterbox, codec: Codec::H264, quality: Quality::Balanced, loudness: Some(-14.0) },
    ];

    pub fn apply(&self, s: &mut ExportSettings) {
        s.codec = self.codec;
        s.quality = self.quality;
        s.resolution = Resolution::Source; // the preset's frame wins
        s.target = Some((self.w, self.h));
        s.fit = self.fit;
        s.audio = AudioMode::Encode { kbps: 160 };
        // Platform delivery includes the platform's loudness target — the
        // whole point of picking a destination instead of a resolution.
        s.loudness = self.loudness;
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
    /// Deliver the audio at this integrated loudness (LUFS) — the platform
    /// targets (-14 for YouTube and the socials). None = leave levels alone.
    pub loudness: Option<f32>,
    /// HDR-to-HDR passthrough: keep the source's transfer (PQ/HLG) and
    /// encode 10-bit instead of squeezing into 8. Source-file exports only
    /// — the timeline pipeline composites in 8-bit SDR.
    pub hdr_passthrough: bool,
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
            loudness: None,
            hdr_passthrough: false,
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
    /// Build a job whose worker is driven elsewhere (the frame server).
    /// Returns the job plus the shared state/cancel handles the worker owns.
    pub(crate) fn manual(
        output: &str,
    ) -> (Self, Arc<Mutex<ExportState>>, Arc<AtomicBool>) {
        let state = Arc::new(Mutex::new(ExportState::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        (
            Self { output: output.to_string(), state: state.clone(), cancel: cancel.clone() },
            state,
            cancel,
        )
    }

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
        // HDR-to-HDR: 10 bits so PQ/HLG survive the encode without banding,
        // and the source's colour tags restated explicitly — libx265 does
        // NOT carry them through on its own (verified: they probe back as
        // "unknown" without this).
        if s.hdr_passthrough && matches!(s.codec, Codec::H265 | Codec::Av1 | Codec::Vp9) {
            a.extend(["-pix_fmt".into(), "yuv420p10le".into()]);
            if let Some((prim, trc, space)) = probe_color(input) {
                if s.codec == Codec::H265 && !s.hardware {
                    // libx265 ignores the generic -color_* flags (they
                    // probe back "unknown") — its own params carry them.
                    a.extend([
                        "-x265-params".into(),
                        format!("colorprim={prim}:transfer={trc}:colormatrix={space}"),
                    ]);
                } else {
                    a.extend([
                        "-color_primaries".into(), prim,
                        "-color_trc".into(), trc,
                        "-colorspace".into(), space,
                    ]);
                }
            }
        }
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
pub(crate) fn video_encoder_args(codec: Codec, q: Quality, hw: bool) -> Vec<String> {
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

/// The source's colour tags (primaries, transfer, matrix), when tagged —
/// what HDR passthrough must restate on the encoder.
fn probe_color(path: &str) -> Option<(String, String, String)> {
    // `default` output, not csv: csv prints in the stream struct's own
    // field order, not the requested order — a classic silent swap.
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries",
               "stream=color_primaries,color_transfer,color_space", "-of", "default=nw=1", path])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| -> Option<String> {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(str::to_string)
            .filter(|v| !v.is_empty() && v != "unknown")
    };
    Some((field("color_primaries")?, field("color_transfer")?, field("color_space")?))
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
    build_timeline_args_full(segments, output, s, with_audio, target, music, &[], &[])
}

/// Bake a segment's lattice grade and park it as a temp .cube for ffmpeg's
/// lut3d — how the graph fallback renders the SAME grade the GPU pipelines
/// sample. Files are keyed by grade_key, so identical grades share one and
/// re-runs cost nothing.
fn grade_cube(fx: &crate::effects::Effects, luts: &[String]) -> Option<String> {
    if !fx.has_lattice() {
        return None;
    }
    let key = crate::lut::grade_key(fx);
    let path = std::env::temp_dir().join(format!("reel-grade-{key:016x}.cube"));
    if !path.exists() {
        let base = fx
            .lut
            .and_then(|i| luts.get(i as usize))
            .and_then(|p| crate::lut::load(p).ok());
        let lattice = crate::lut::bake_grade(base.as_deref(), fx);
        // Atomic: a concurrent render (or test harness) must never read a
        // half-written cube — write to a private temp, then rename.
        let tmp = path.with_extension(format!("part-{}", std::process::id()));
        if let Err(e) = crate::lut::write_cube(&lattice, &tmp) {
            log::warn!("could not write the grade lattice ({e}); the fallback skips this grade");
            return None;
        }
        if std::fs::rename(&tmp, &path).is_err() && !path.exists() {
            return None;
        }
    }
    Some(path.to_string_lossy().into_owned())
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
    luts: &[String],
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
        // The lattice grade (LUT ∘ WB/levels/HSL ∘ curves) is baked to a
        // temp .cube and applied via lut3d FIRST — the same order the GPU
        // pipelines sample it (grade before the trims).
        let mut fx = seg.effects.filters(duration);
        if let Some(cube) = grade_cube(&seg.effects, luts) {
            fx.insert(0, format!("lut3d='{}'", cube));
        }
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
                    "[{vprev}][v{k}]xfade=transition={}:duration={d:.4}:offset={start:.4}[{vo}];",
                    segments[k].transition_kind.xfade_name()
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
        push_music_mix(&mut graph, &mut audio_out, idx, m, crate::edit::render_duration(segments));
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
    /// Timeline markers — written into the output as CHAPTERS, so a long
    /// export lands on YouTube with its sections already named.
    pub markers: &'a [f64],
    /// Names for markers, attached by time; unnamed ones become Chapter N.
    pub marker_labels: &'a [(f64, String)],
    /// The project's LUT table — `Effects.lut` indexes into it.
    pub luts: &'a [String],
    /// Audio-track and overlay clips mixed into the export — the live mixer
    /// already plays these; a render that dropped them would be a preview
    /// that lies.
    pub audio_clips: &'a [crate::edit::AudioClip],
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
    // The frame server is the renderer (Reel's own compositor draws every
    // frame; ffmpeg encodes). The compiled-filter-graph path remains as the
    // fallback for machines with no GPU adapter, and REEL_RENDER=graph
    // forces it for comparison.
    if std::env::var("REEL_RENDER").as_deref() != Ok("graph") {
        match crate::engine::render::start_timeline(segments, output, settings, project, &overlays)
        {
            Ok(job) => return Ok(job),
            Err(e) => {
                log::warn!("frame server unavailable ({e}); rendering via the ffmpeg graph");
            }
        }
    }
    start_timeline_graph(segments, output, settings, project, overlays)
}

/// The burn-in filters (captions, then titles) for a timeline render, with
/// their sidecar files written next to `output`. Shared by both render paths
/// so a caption looks the same whichever engine drew the picture.
pub(crate) fn burnin_filters(
    output: &str,
    overlays: &Overlays<'_>,
    target: (u32, u32, f64),
) -> Result<Vec<String>> {
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
        std::fs::write(&ass, crate::titles::to_ass(overlays.titles, target.0, target.1))?;
        chain.push(format!("subtitles='{}'", escape_filter_path(&ass)));
    }
    Ok(chain)
}

/// The original renderer: the whole timeline compiled into one ffmpeg
/// filter graph. Kept as the no-GPU fallback and as the reference the frame
/// server was validated against.
pub(crate) fn start_timeline_graph(
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
    // The static graph can't do everything the frame server can. One
    // honest, complete list — silence would be a preview that lies at
    // render time.
    let mut dropped: Vec<&str> = Vec::new();
    if segments.iter().any(|s| !s.keys.is_empty()) {
        dropped.push("keyframe animation");
    }
    if segments.iter().any(|s| s.effects.mask.is_some()) {
        dropped.push("power windows");
    }
    if segments.iter().any(|s| s.effects.key_color.is_some())
        || overlays.overlays.iter().any(|o| o.effects.key_color.is_some())
    {
        dropped.push("chroma keys");
    }
    if segments.iter().any(|s| s.stabilize) {
        dropped.push("stabilization");
    }
    if !overlays.markers.is_empty() {
        dropped.push("chapters");
    }
    if !dropped.is_empty() {
        log::warn!(
            "the graph fallback cannot render: {} — a GPU enables the frame server, \
             which does all of it",
            dropped.join(", ")
        );
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
        overlays.luts,
    );

    let chain = burnin_filters(output, &overlays, target)?;
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


/// Lay a music bed under (or as) the cut's audio: level, delay, trim to the
/// cut, fades, optional sidechain ducking, and the normalize=0 mix. Shared by
/// the graph renderer and the frame server's audio pass so the two can never
/// disagree about what "add music" means.
fn push_music_mix(
    graph: &mut String,
    audio_out: &mut Option<String>,
    idx: usize,
    m: &crate::edit::Music,
    total: f64,
) {
    let anorm = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";
    let delay_ms = (m.start.max(0.0) * 1000.0).round() as u64;
    let delay = if delay_ms > 0 {
        format!(",adelay={delay_ms}|{delay_ms}")
    } else {
        String::new()
    };
    // Trim to the cut's length so a long track can't extend the render, and
    // fade so it never just stops dead.
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
                // The cut's audio drives the compressor AND is kept — so it
                // is split first; a filter output can only be consumed once.
                graph.push_str(&format!(";{cut}asplit=2[sc_keep][sc_key]"));
                graph.push_str(
                    ";[mus][sc_key]sidechaincompress=threshold=0.03:ratio=12:attack=20:release=400:makeup=1[mus_d]",
                );
                ("[sc_keep]", "[mus_d]")
            } else {
                (cut.as_str(), "[mus]")
            };
            // normalize=0: amix otherwise divides every input by the number
            // of inputs, quietly halving the dialogue.
            graph.push_str(&format!(
                ";{}{}amix=inputs=2:duration=first:normalize=0:dropout_transition=0[amix]",
                bed.0, bed.1
            ));
            *audio_out = Some("[amix]".into());
        }
        // Nothing but music: it becomes the soundtrack.
        None => *audio_out = Some("[mus]".into()),
    }
}

/// The audio side of a timeline render, alone, written to a WAV: the same
/// per-segment legs (trim, tempo, gain), the same concat/acrossfade chain,
/// and the same music mix as the graph renderer. The frame server hands the
/// result to its encoder. Returns None when the cut has no audio at all.
/// The ffmpeg filters for one clip's audio processing, leading comma
/// included. Order is fixed and mirrored by the live mixer: repair → EQ →
/// compressor → pan (gain and fades ride elsewhere in the chain).
fn audio_fx_chain(fx: &crate::edit::AudioFx) -> String {
    let mut f = String::new();
    if fx.voice_fix {
        // The repair set: rumble and hum off (two cascaded high-passes =
        // 24 dB/oct — one leaves too much 50 Hz standing), broadband noise
        // down, clicks patched.
        f.push_str(",highpass=f=80,highpass=f=80,afftdn=nr=12:nf=-40,adeclick");
    }
    if fx.eq_low.abs() > 0.01 {
        f.push_str(&format!(",bass=g={:.2}:f=120", fx.eq_low.clamp(-24.0, 24.0)));
    }
    if fx.eq_mid.abs() > 0.01 {
        f.push_str(&format!(
            ",equalizer=f={:.1}:t=q:w=1.0:g={:.2}",
            fx.eq_mid_freq.clamp(100.0, 12000.0),
            fx.eq_mid.clamp(-24.0, 24.0)
        ));
    }
    if fx.eq_high.abs() > 0.01 {
        f.push_str(&format!(",treble=g={:.2}:f=8000", fx.eq_high.clamp(-24.0, 24.0)));
    }
    if fx.comp {
        // acompressor's threshold is linear; ours is stated in dBFS.
        let thresh = 10f32.powf(fx.comp_thresh.clamp(-60.0, 0.0) / 20.0);
        f.push_str(&format!(
            ",acompressor=threshold={:.6}:ratio={:.2}:attack=20:release=250",
            thresh,
            fx.comp_ratio.clamp(1.0, 20.0)
        ));
    }
    if fx.pan.abs() > 1e-4 {
        let (l, r) = fx.pan_gains();
        f.push_str(&format!(",pan=stereo|c0={l:.4}*c0|c1={r:.4}*c1"));
    }
    f
}

pub(crate) fn build_timeline_audio_wav_args(
    segments: &[Segment],
    with_audio: bool,
    music: Option<&crate::edit::Music>,
    audio_clips: &[crate::edit::AudioClip],
    loudness: Option<f32>,
    wav_out: &str,
) -> Option<Vec<String>> {
    let music = music.filter(|m| !m.source.is_empty());
    let audio_clips: Vec<&crate::edit::AudioClip> =
        audio_clips.iter().filter(|c| has_audio_stream(&c.source)).collect();
    if !with_audio && music.is_none() && audio_clips.is_empty() {
        return None;
    }
    let anorm = "aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo";
    let mut a: Vec<String> = Vec::new();
    let mut sources: Vec<&str> = Vec::new();
    if with_audio {
        for seg in segments {
            if !sources.contains(&seg.source.as_str()) {
                sources.push(&seg.source);
            }
        }
        for src in &sources {
            a.extend(["-i".into(), (*src).into()]);
        }
    }
    let music_input = music.map(|m| {
        a.extend(["-i".into(), m.source.clone()]);
        sources.len()
    });
    let clip_inputs: Vec<usize> = audio_clips
        .iter()
        .map(|c| {
            a.extend(["-i".into(), c.source.clone()]);
            a.iter().filter(|x| *x == "-i").count() - 1
        })
        .collect();

    let mut graph = String::new();
    let mut audio_out: Option<String> = None;
    if with_audio {
        for (k, seg) in segments.iter().enumerate() {
            let i = sources.iter().position(|s| *s == seg.source).unwrap();
            let gain = if seg.gain_db.abs() > 0.01 {
                format!(",volume={:.2}dB", seg.gain_db)
            } else {
                String::new()
            };
            if seg.has_ramp() {
                // A speed RAMP: the audio approximates the curve piecewise —
                // one atempo per keyframe interval at that interval's TRUE
                // average rate, read straight off the same integral the
                // video walks. Each chunk therefore consumes exactly the
                // source the picture consumed, and the total length matches
                // the clip's slot to the sample.
                let mut cuts: Vec<f64> = vec![0.0, seg.duration];
                if let Some((_, keys)) = seg
                    .keys
                    .iter()
                    .find(|(p, _)| *p == crate::edit::Param::Speed)
                {
                    for kf in keys {
                        if kf.t > 1e-6 && kf.t < seg.duration - 1e-6 {
                            cuts.push(kf.t);
                        }
                    }
                }
                cuts.sort_by(|a, b| a.total_cmp(b));
                cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
                let mut labels = Vec::new();
                for (j, w) in cuts.windows(2).enumerate() {
                    let (t0, t1) = (w[0], w[1]);
                    let s0 = seg.source_offset_at(t0);
                    let s1 = seg.source_offset_at(t1);
                    let avg = ((s1 - s0) / (t1 - t0).max(1e-9)).clamp(0.05, 20.0);
                    let label = format!("ar{k}_{j}");
                    graph.push_str(&format!(
                        "[{i}:a]atrim=start={:.4}:duration={:.4},asetpts=PTS-STARTPTS,{anorm}{}[{label}];",
                        seg.in_point + s0,
                        s1 - s0,
                        atempo_chain(avg)
                    ));
                    labels.push(label);
                }
                for l in &labels {
                    graph.push_str(&format!("[{l}]"));
                }
                graph.push_str(&format!(
                    "concat=n={}:v=0:a=1{}{gain}[a{k}];",
                    labels.len(),
                    audio_fx_chain(&seg.audio)
                ));
            } else {
                let rate = seg.speed.clamp(0.05, 20.0) as f64;
                let src_len = seg.duration * rate;
                graph.push_str(&format!(
                    "[{i}:a]atrim=start={:.4}:duration={src_len:.4},asetpts=PTS-STARTPTS,{anorm}{}{}{gain}[a{k}];",
                    seg.in_point,
                    atempo_chain(rate),
                    audio_fx_chain(&seg.audio)
                ));
            }
        }
        if segments.iter().any(|seg| seg.transition_in > 0.0) {
            let mut aprev = "a0".to_string();
            for k in 1..segments.len() {
                let d = segments[k]
                    .transition_in
                    .min(segments[k - 1].duration)
                    .min(segments[k].duration);
                let ao = format!("ax{k}");
                if d > 0.0 {
                    graph.push_str(&format!("[{aprev}][a{k}]acrossfade=d={d:.4}[{ao}];"));
                } else {
                    graph.push_str(&format!("[{aprev}][a{k}]concat=n=2:v=0:a=1[{ao}];"));
                }
                aprev = ao;
            }
            graph.push_str(&format!("[{aprev}]anull[acat]"));
        } else {
            for k in 0..segments.len() {
                graph.push_str(&format!("[a{k}]"));
            }
            graph.push_str(&format!("concat=n={}:v=0:a=1[acat]", segments.len()));
        }
        audio_out = Some("[acat]".into());
    }
    // Audio-track clips: trim each window, tempo/gain/fades, delay to its
    // TIMELINE position, then mix everything as one bed with the cut.
    if !audio_clips.is_empty() {
        if graph.is_empty() {
            graph.push_str("anullsrc=r=48000:cl=stereo:d=0.001[zz];[zz]anullsink");
        }
        let mut labels: Vec<String> = Vec::new();
        for (j, (c, idx)) in audio_clips.iter().zip(&clip_inputs).enumerate() {
            let rate = (c.speed as f64).clamp(0.05, 20.0);
            let src_len = c.duration * rate;
            let delay_ms = (c.at.max(0.0) * 1000.0).round() as u64;
            let gain = if c.gain_db.abs() > 0.01 {
                format!(",volume={:.2}dB", c.gain_db)
            } else {
                String::new()
            };
            let mut fades = String::new();
            if c.fade_in > 0.0 {
                fades.push_str(&format!(",afade=t=in:st=0:d={:.3}", c.fade_in));
            }
            if c.fade_out > 0.0 {
                fades.push_str(&format!(
                    ",afade=t=out:st={:.3}:d={:.3}",
                    (c.duration - c.fade_out).max(0.0),
                    c.fade_out
                ));
            }
            let label = format!("ac{j}");
            graph.push_str(&format!(
                ";[{idx}:a]atrim=start={:.4}:duration={src_len:.4},asetpts=PTS-STARTPTS,{anorm}{}{}{gain}{fades},adelay={delay_ms}|{delay_ms}[{label}]",
                c.in_point,
                atempo_chain(rate),
                audio_fx_chain(&c.audio),
            ));
            labels.push(label);
        }
        // Mix the voice/SFX bed with (or as) the cut's audio. normalize=0,
        // as ever — the default halves everything.
        match &audio_out {
            Some(cut) => {
                graph.push_str(&format!(";{cut}"));
                for l in &labels {
                    graph.push_str(&format!("[{l}]"));
                }
                graph.push_str(&format!(
                    "amix=inputs={}:duration=first:normalize=0:dropout_transition=0[awtracks]",
                    labels.len() + 1
                ));
                audio_out = Some("[awtracks]".into());
            }
            None => {
                if labels.len() == 1 {
                    audio_out = Some(format!("[{}]", labels[0]));
                } else {
                    for l in &labels {
                        graph.push_str(&format!("[{l}]"));
                    }
                    graph.push_str(&format!(
                        ";amix=inputs={}:normalize=0[awtracks]",
                        labels.len()
                    ));
                    audio_out = Some("[awtracks]".into());
                }
            }
        }
    }

    if let (Some(idx), Some(m)) = (music_input, music) {
        if graph.is_empty() {
            // push_music_mix writes ";[idx:a]…" — valid mid-graph, not at
            // the very start.
            graph.push_str("anullsrc=r=48000:cl=stereo:d=0.001[zz];[zz]anullsink");
        }
        push_music_mix(&mut graph, &mut audio_out, idx, m, crate::edit::render_duration(segments));
    }
    let mut out_label = audio_out?;
    // Loudness delivery: one-pass loudnorm to the platform target. Sits at
    // the very end so it measures the finished mix, music and all.
    if let Some(target) = loudness {
        graph.push_str(&format!(
            ";{out_label}loudnorm=I={:.1}:TP=-1.5:LRA=11[aloud]",
            target.clamp(-36.0, -8.0)
        ));
        out_label = "[aloud]".into();
    }
    a.splice(0..0, ["-y".into()]);
    a.extend([
        "-filter_complex".into(),
        graph,
        "-map".into(),
        out_label,
        "-c:a".into(),
        "pcm_s16le".into(),
        wav_out.into(),
    ]);
    Some(a)
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
        markers: Vec<f64>,
        marker_labels: Vec<(f64, String)>,
        luts: Vec<String>,
        audio_clips: Vec<crate::edit::AudioClip>,
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
                        markers, marker_labels, luts, audio_clips,
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
                            markers,
                            marker_labels,
                            luts,
                            audio_clips,
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
            transition_kind: Default::default(),
            stabilize: false,
            audio: Default::default(),
            gain_db: 0.0,
            speed: 1.0,
            keys: Vec::new(),
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
            loudness: None,
            hdr_passthrough: false,
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
            loudness: None,
            hdr_passthrough: false,
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
            loudness: None,
            hdr_passthrough: false,
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
            loudness: None,
            hdr_passthrough: false,
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

    /// The graph fallback now carries the WHOLE lattice grade (levels, WB,
    /// HSL, curves) through a baked lut3d. Render a solid colour through it
    /// and check the pixel against grade_reference — the same contract the
    /// GPU pipelines are held to.
    #[test]
    fn the_graph_fallback_applies_the_baked_grade() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-gradefall-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-gradefall-out-{}.mp4", std::process::id()));
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
        // Solid mid-blue, 1s.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i",
                   "color=c=0x4060C0:size=320x240:rate=30:duration=1",
                   "-pix_fmt", "yuv420p", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let fx = crate::effects::Effects {
            levels_black: 0.1,
            levels_gamma: 1.2,
            wb_temp: 0.4,
            curves: Some(crate::effects::Curves {
                master: [0.0, 0.2, 0.5, 0.8, 1.0],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut sg = seg(&src.to_string_lossy(), 0.2, 0.5);
        sg.effects = fx;
        let args = build_timeline_args(&[sg], &out.to_string_lossy(),
            &ExportSettings { quality: Quality::High, hardware: false, ..Default::default() },
            false, (320, 240, 30.0));
        assert!(args.iter().any(|a| a.contains("lut3d")), "the grade must ride a lut3d: {args:?}");
        let run = std::process::Command::new("ffmpeg").args(&args).arg("-y")
            .output().expect("spawn ffmpeg");
        assert!(run.status.success(), "fallback render failed:\n{}",
            String::from_utf8_lossy(&run.stderr));
        // Probe the middle frame's centre pixel.
        let png = dir.join(format!("reel-gradefall-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&png);
        let probe = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-ss", "0.25", "-i", &out.to_string_lossy(),
                   "-frames:v", "1", &png.to_string_lossy()])
            .output().expect("spawn probe");
        if !probe.status.success() {
            let meta = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            let render_err = String::from_utf8_lossy(&run.stderr);
            panic!(
                "probe failed: {}\nout size={meta}\nrender stderr: {render_err}\nargs: {args:?}",
                String::from_utf8_lossy(&probe.stderr)
            );
        }
        let img = image::open(&png).unwrap().to_rgb8();
        let got = img.get_pixel(160, 120).0;
        // The lattice bakes grade_reference THEN the curves — expectation
        // must compose both, exactly like bake_grade does.
        let mut expect = fx.grade_reference([0x40 as f32 / 255.0, 0x60 as f32 / 255.0, 0xC0 as f32 / 255.0]);
        let cv = fx.curves.unwrap();
        for (ch, v) in expect.iter_mut().enumerate() {
            *v = cv.apply(ch, *v);
        }
        for c in 0..3 {
            let e = (expect[c] * 255.0).round() as i32;
            let delta = (got[c] as i32 - e).abs();
            // lut3d interp + yuv420 round-trip: allow a little more than the
            // RGB-only parity tests.
            assert!(delta <= 8, "channel {c}: fallback {} vs reference {e} (Δ{delta})", got[c]);
        }
        for f in [&src, &out, &png] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// HDR in, HDR out: a PQ/BT.2020 source converted with passthrough
    /// keeps its transfer and gains 10 bits; the default 8-bit path is what
    /// it always was. Codecs that can't carry it aren't given it.
    #[test]
    fn hdr_passthrough_keeps_the_transfer_and_ten_bits() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-hdrpass-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-hdrpass-out-{}.mp4", std::process::id()));
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
        // A PQ/BT.2020-tagged 10-bit source.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i",
                   "testsrc2=size=320x240:rate=30:duration=1",
                   "-c:v", "libx265", "-preset", "ultrafast", "-pix_fmt", "yuv420p10le",
                   "-x265-params", "colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc",
                   "-tag:v", "hvc1",
                   &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let s = ExportSettings {
            codec: Codec::H265,
            quality: Quality::Small,
            hardware: false,
            audio: AudioMode::Copy,
            hdr_passthrough: true,
            ..Default::default()
        };
        let args = build_args(&src.to_string_lossy(), &out.to_string_lossy(), &s);
        assert!(args.iter().any(|a| a == "yuv420p10le"), "10-bit asked for: {args:?}");
        let enc = std::process::Command::new("ffmpeg").arg("-y").args(&args)
            .output().expect("spawn encode");
        assert!(enc.status.success(), "passthrough encode failed:\n{}",
            String::from_utf8_lossy(&enc.stderr));
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v", "-show_entries",
                   "stream=pix_fmt,color_transfer,color_primaries", "-of", "csv=p=0",
                   &out.to_string_lossy()])
            .output().expect("ffprobe");
        let text = String::from_utf8_lossy(&probe.stdout);
        assert!(text.contains("yuv420p10le"), "still 10-bit: {text}");
        assert!(text.contains("smpte2084"), "PQ survived: {text}");
        assert!(text.contains("bt2020"), "primaries survived: {text}");
        // H.264 (can't carry it in our chain) is never given the 10-bit flag.
        let s264 = ExportSettings { codec: Codec::H264, hdr_passthrough: true, hardware: false, ..Default::default() };
        let a264 = build_args(&src.to_string_lossy(), "x.mp4", &s264);
        assert!(!a264.iter().any(|a| a == "yuv420p10le"), "{a264:?}");
        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
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
    /// Per-channel RMS in dB, from astats.
    fn channel_levels(file: &Path) -> (f32, f32) {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "info", "-i", &file.to_string_lossy(),
                   "-af", "astats=metadata=0", "-f", "null", "-"])
            .output()
            .expect("run astats");
        let text = String::from_utf8_lossy(&out.stderr);
        let mut rms: Vec<f32> = Vec::new();
        for line in text.lines() {
            if let Some(v) = line.split("RMS level dB:").nth(1) {
                if let Ok(x) = v.trim().parse::<f32>() {
                    rms.push(x);
                }
            }
        }
        // Channel 1, Channel 2, then Overall — take the first two.
        assert!(rms.len() >= 2, "astats gave {} RMS lines:\n{text}", rms.len());
        (rms[0], rms[1])
    }

    /// Pan renders as a balance: a full-right pan silences the left channel
    /// of the exported mix and leaves the right at its level.
    #[test]
    fn pan_lands_in_the_export() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-pan-src-{}.mp4", std::process::id()));
        let wav = dir.join(format!("reel-pan-{}.wav", std::process::id()));
        for f in [&src, &wav] {
            let _ = std::fs::remove_file(f);
        }
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=2",
                   "-f", "lavfi", "-i", "sine=frequency=500:duration=2",
                   "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                   "-c:a", "aac", "-shortest", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let mut sg = seg(&src.to_string_lossy(), 0.0, 1.5);
        sg.audio.pan = 1.0;
        let args = build_timeline_audio_wav_args(&[sg], true, None, &[], None, &wav.to_string_lossy())
            .expect("wav args");
        assert!(std::process::Command::new("ffmpeg").args(&args)
            .status().map(|s| s.success()).unwrap_or(false), "wav render failed");
        let (l, r) = channel_levels(&wav);
        assert!(l < -60.0, "left must be silent after a full-right pan, got {l} dB");
        assert!(r > -30.0, "right keeps the signal, got {r} dB");
        assert!(r - l > 30.0, "the two channels must be far apart, got L {l} / R {r}");
        for f in [&src, &wav] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// "Fix voice": a steady 50 Hz hum under bursty speech-like tone. The
    /// repair chain guts the hum band and keeps the voice band.
    #[test]
    fn voice_fix_removes_hum_and_keeps_the_voice() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-fix-src-{}.mp4", std::process::id()));
        let wav = dir.join(format!("reel-fix-{}.wav", std::process::id()));
        for f in [&src, &wav] {
            let _ = std::fs::remove_file(f);
        }
        // Voice = 1 kHz gated on/off at 2 Hz (non-stationary, so the
        // denoiser must not treat it as noise); hum = steady 50 Hz.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=30:duration=4",
                   "-f", "lavfi", "-i",
                   "sine=frequency=1000:duration=4,volume=volume='0.4*gt(sin(2*PI*t*2),0)':eval=frame",
                   "-f", "lavfi", "-i", "sine=frequency=50:duration=4,volume=0.3",
                   "-filter_complex", "[1][2]amix=inputs=2:normalize=0[a]",
                   "-map", "0:v", "-map", "[a]",
                   "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                   "-c:a", "aac", "-shortest", &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        // Render twice: raw and fixed.
        let raw = seg(&src.to_string_lossy(), 0.0, 3.5);
        let mut fixed = raw.clone();
        fixed.audio.voice_fix = true;
        let wav_raw = dir.join(format!("reel-fixraw-{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&wav_raw);
        for (sg, out) in [(&raw, &wav_raw), (&fixed, &wav)] {
            let args = build_timeline_audio_wav_args(
                std::slice::from_ref(sg), true, None, &[], None, &out.to_string_lossy())
                .expect("wav args");
            assert!(std::process::Command::new("ffmpeg").args(&args)
                .status().map(|s| s.success()).unwrap_or(false), "wav render failed");
        }
        let hum_before = band_level(&wav_raw, 0.2, 3.2, 50);
        let hum_after = band_level(&wav, 0.2, 3.2, 50);
        let voice_before = band_level(&wav_raw, 0.2, 3.2, 1000);
        let voice_after = band_level(&wav, 0.2, 3.2, 1000);
        let hum_drop = hum_before - hum_after;
        let voice_drop = voice_before - voice_after;
        assert!(
            hum_drop > 10.0,
            "the fix must gut the hum (before {hum_before}, after {hum_after})"
        );
        assert!(
            voice_drop < 6.0,
            "the voice must survive the fix (before {voice_before}, after {voice_after})"
        );
        for f in [&src, &wav, &wav_raw] {
            let _ = std::fs::remove_file(f);
        }
    }

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
            transition_kind: Default::default(),
            stabilize: false,
            audio: Default::default(),
            gain_db: 0.0,
            speed: 1.0,
            keys: Vec::new(),
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
    /// The Phase-2 promise, measured in pixels: a keyframed exposure ramp
    /// actually ramps in the rendered file, landing its midpoint where the
    /// curve says. Only the frame server can do this — a static filter graph
    /// has one exposure for the whole clip.
    #[test]
    fn a_keyframed_ramp_lands_its_midpoints_in_the_render() {
        use crate::edit::{Interp, Keyframe, Param};
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-kf-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-kf-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // A flat mid-gray source: every brightness change is the keyframes'.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi",
                   "-i", "color=c=0x808080:size=320x240:rate=25:duration=2",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p", &src.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let mut s0 = seg(&src.to_string_lossy(), 0.0, 2.0);
        s0.keys = vec![(
            Param::Exposure,
            vec![
                Keyframe { t: 0.0, value: 1.0, interp: Interp::Linear },
                Keyframe { t: 2.0, value: 1.5, interp: Interp::Linear },
            ],
        )];
        let segs = vec![s0];
        let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
        let job = match crate::engine::render::start_timeline(
            &segs, &out.to_string_lossy(), &s, (320, 240, 25.0), &Overlays::default(),
        ) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("no GPU — skipping keyframe render test ({e})");
                return;
            }
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "keyframed render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "keyframed render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        let level_at = |t: &str| -> f64 {
            let png = dir.join(format!("reel-kf-f-{}.png", std::process::id()));
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-ss", t, "-i", &out.to_string_lossy(),
                       "-frames:v", "1", &png.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            let img = image::open(&png).expect("read frame").to_luma8();
            let _ = std::fs::remove_file(&png);
            let sum: u64 = img.pixels().map(|p| p.0[0] as u64).sum();
            sum as f64 / (img.width() * img.height()) as f64
        };
        // Exposure multiplies the sRGB-encoded value (the reference formula):
        // 0x80 = 128 → ×1.0 = 128, ×1.25 = 160, ×1.5 = 192.
        let start = level_at("0.05");
        let mid = level_at("1.0");
        let end = level_at("1.9");
        assert!((start - 128.0).abs() < 8.0, "ramp start should be ~128, got {start:.1}");
        assert!((mid - 160.0).abs() < 8.0, "ramp midpoint should be ~160, got {mid:.1}");
        assert!((end - 192.0).abs() < 10.0, "ramp end should be ~192, got {end:.1}");
        assert!(start < mid && mid < end, "brightness must rise monotonically");

        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Stabilisation, measured: a synthetically shaky clip rendered with and
    /// without the flag; the stabilised output's mean inter-frame difference
    /// (shake energy) must drop by at least a third. Not "the filter ran" —
    /// the shake actually went away.
    #[test]
    fn stabilisation_measurably_reduces_shake() {
        if !std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-filters"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("vidstabtransform"))
            .unwrap_or(false)
        {
            eprintln!("ffmpeg has no vidstab — skipping stabilisation test");
            return;
        }
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-stab-src-{}.mp4", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "testsrc2=size=960x720:rate=25:duration=4",
                   "-vf", "crop=640:480:x='160+50*sin(n/3.1)':y='120+40*cos(n/2.3)'",
                   "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
                   &src.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let shake_of = |stabilize: bool| -> f64 {
            let out = dir.join(format!(
                "reel-stab-{}-{}.mp4",
                stabilize as u8,
                std::process::id()
            ));
            let _ = std::fs::remove_file(&out);
            let mut s0 = seg(&src.to_string_lossy(), 0.0, 4.0);
            s0.stabilize = stabilize;
            let segs = vec![s0];
            let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
            let job = match crate::engine::render::start_timeline(
                &segs, &out.to_string_lossy(), &s, (640, 480, 25.0), &Overlays::default(),
            ) {
                Ok(j) => j,
                Err(_) => return -1.0, // no GPU
            };
            let deadline = Instant::now() + Duration::from_secs(180);
            loop {
                let st = job.state();
                if st.finished {
                    assert!(st.error.is_none(), "stab render failed: {:?}", st.error);
                    break;
                }
                assert!(Instant::now() < deadline, "stab render timed out");
                std::thread::sleep(Duration::from_millis(50));
            }
            let probe = std::process::Command::new("ffmpeg")
                .args(["-i", &out.to_string_lossy(),
                       "-vf", "tblend=all_mode=difference,signalstats,metadata=print:key=lavfi.signalstats.YAVG",
                       "-f", "null", "-"])
                .output()
                .expect("measure shake");
            let text = String::from_utf8_lossy(&probe.stderr);
            let vals: Vec<f64> = text
                .lines()
                .filter_map(|l| l.split("YAVG=").nth(1))
                .filter_map(|v| v.trim().parse().ok())
                .collect();
            let _ = std::fs::remove_file(&out);
            assert!(!vals.is_empty(), "no YAVG values from signalstats");
            vals.iter().sum::<f64>() / vals.len() as f64
        };

        let raw = shake_of(false);
        if raw < 0.0 {
            eprintln!("no GPU — skipping stabilisation test");
            return;
        }
        let stab = shake_of(true);
        assert!(
            stab < raw * 0.67,
            "stabilisation barely helped: shake {raw:.2} → {stab:.2}"
        );
        let _ = std::fs::remove_file(&src);
    }

    /// The live mixer plays audio-track clips; the render must too — this
    /// pins the fix for a preview-lies bug where A1 audio existed only in
    /// the editor. A 700 Hz beep placed at 2 s on the audio track must
    /// sound at 2 s in the export, and nowhere before.
    #[test]
    fn audio_track_clips_sound_in_the_export() {
        let dir = std::env::temp_dir();
        let vid = dir.join(format!("reel-atr-vid-{}.mp4", std::process::id()));
        let beep = dir.join(format!("reel-atr-beep-{}.wav", std::process::id()));
        let out = dir.join(format!("reel-atr-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // The cut itself: video with a quiet 200 Hz hum.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "color=c=gray:size=160x120:rate=25:duration=6",
                   "-f", "lavfi", "-i", "sine=frequency=200:duration=6",
                   "-af", "volume=-20dB",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
                   &vid.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi",
                   "-i", "sine=frequency=700:duration=1.5", &beep.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let segs = vec![seg(&vid.to_string_lossy(), 0.0, 6.0)];
        let clips = vec![crate::edit::AudioClip {
            source: beep.to_string_lossy().into(),
            at: 2.0,
            in_point: 0.0,
            duration: 1.5,
            gain_db: 0.0,
            fade_in: 0.0,
            fade_out: 0.0,
            speed: 1.0,
            audio: Default::default(),
        }];
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let job = start_timeline_with_captions(
            &segs,
            &out.to_string_lossy(),
            &s,
            (160, 120, 25.0),
            Overlays { audio_clips: &clips, ..Default::default() },
        )
        .expect("start render");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        // 700 Hz band level before (0.5–1.5 s) vs during (2.2–3.2 s).
        let before = band_level(&out, 0.5, 1.5, 700);
        let during = band_level(&out, 2.2, 3.2, 700);
        assert!(
            during - before > 20.0,
            "the audio-track beep is missing from the export: {before:.1} dB → {during:.1} dB"
        );

        for f in [&vid, &beep, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Loudness delivery: ask for −16 LUFS and the finished file must
    /// MEASURE −16 (±1.5). ebur128 on the real output is the only honest
    /// referee here.
    #[test]
    fn delivered_loudness_hits_the_target() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-loud-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-loud-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // A quiet tone (−30ish) so normalisation has real work to do.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i", "color=c=gray:size=160x120:rate=25:duration=4",
                   "-f", "lavfi", "-i", "sine=frequency=300:duration=4",
                   "-af", "volume=-18dB",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
                   &src.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let segs = vec![seg(&src.to_string_lossy(), 0.0, 4.0)];
        let s = ExportSettings {
            quality: Quality::Small,
            hardware: false,
            loudness: Some(-16.0),
            hdr_passthrough: false,
            ..Default::default()
        };
        let job = start_timeline_with_captions(
            &segs, &out.to_string_lossy(), &s, (160, 120, 25.0), Overlays::default(),
        )
        .expect("start loudness render");
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "loudness render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "loudness render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        let probe = std::process::Command::new("ffmpeg")
            .args(["-i", &out.to_string_lossy(), "-af", "ebur128", "-f", "null", "-"])
            .output()
            .expect("run ebur128");
        let text = String::from_utf8_lossy(&probe.stderr);
        let measured: f32 = text
            .lines()
            .rev()
            .find_map(|l| {
                let l = l.trim();
                l.strip_prefix("I:")
                    .and_then(|v| v.trim().strip_suffix("LUFS"))
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or_else(|| panic!("no integrated loudness in ebur128 output:\n{text}"));
        assert!(
            (measured - -16.0).abs() < 1.5,
            "asked for −16 LUFS, delivered {measured:.1}"
        );

        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// A still grabbed mid-transition must SHOW the transition, and cues
    /// active at that moment must burn in — the still is a one-frame render,
    /// not a shortcut past it.
    #[test]
    fn a_still_composes_the_wipe_and_burns_the_caption() {
        use crate::edit::TransitionKind;
        let dir = std::env::temp_dir();
        let mk = |name: &str, colour: &str| -> std::path::PathBuf {
            let f = dir.join(format!("reel-stw-{name}-{}.mp4", std::process::id()));
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi",
                       "-i", &format!("color=c={colour}:size=320x240:rate=25:duration=3"),
                       "-c:v", "libx264", "-pix_fmt", "yuv420p", &f.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            f
        };
        let red = mk("a", "red");
        let blue = mk("b", "blue");
        let out = dir.join(format!("reel-stw-{}.png", std::process::id()));
        let _ = std::fs::remove_file(&out);

        let mut s1 = seg(&blue.to_string_lossy(), 0.0, 3.0);
        s1.transition_in = 2.0;
        s1.transition_kind = TransitionKind::WipeRight;
        let segs = vec![seg(&red.to_string_lossy(), 0.0, 3.0), s1];
        let cues = vec![crate::captions::Cue { start: 1.5, end: 2.5, text: "MID WIPE".into() }];
        let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
        // Timeline t=2.0 is the wipe's midpoint (overlap runs 1..3).
        match crate::engine::render::still_png(
            &segs,
            &Overlays { captions: &cues, caption_size: 20, ..Default::default() },
            (320, 240, 25.0),
            &s,
            2.0,
            &out.to_string_lossy(),
        ) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("no GPU — skipping still test ({e})");
                return;
            }
        }
        let img = image::open(&out).expect("read still").to_rgb8();
        let px = |x: u32, y: u32| img.get_pixel(x, y).0;
        let [r, _, b] = px(60, 60);
        assert!(b > 130 && r < 90, "left of the travelling edge must be blue, got {:?}", px(60, 60));
        let [r, _, b] = px(260, 60);
        assert!(r > 130 && b < 90, "right must still be red, got {:?}", px(260, 60));
        // The caption burned: white pixels along the bottom band.
        let white = img
            .enumerate_pixels()
            .filter(|(_, y, p)| *y > 180 && p.0.iter().all(|c| *c > 200))
            .count();
        assert!(white > 100, "the caption is missing from the still ({white} white px)");
        let _ = std::fs::remove_file(&out);
        for f in [&red, &blue] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// A wipe, mid-travel, in rendered pixels: red cuts to blue with a
    /// WipeRight — halfway through, the frame's left half must be blue
    /// (incoming) and its right half still red. Geometry, not just opacity.
    #[test]
    fn a_wipe_reveals_geometrically_in_the_render() {
        use crate::edit::TransitionKind;
        let dir = std::env::temp_dir();
        let mk = |name: &str, colour: &str| -> std::path::PathBuf {
            let f = dir.join(format!("reel-wipe-{name}-{}.mp4", std::process::id()));
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi",
                       "-i", &format!("color=c={colour}:size=320x240:rate=25:duration=3"),
                       "-c:v", "libx264", "-pix_fmt", "yuv420p", &f.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            f
        };
        let red = mk("a", "red");
        let blue = mk("b", "blue");
        let out = dir.join(format!("reel-wipe-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);

        let mut s1 = seg(&blue.to_string_lossy(), 0.0, 3.0);
        s1.transition_in = 2.0;
        s1.transition_kind = TransitionKind::WipeRight;
        let segs = vec![seg(&red.to_string_lossy(), 0.0, 3.0), s1];
        let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
        let job = match crate::engine::render::start_timeline(
            &segs, &out.to_string_lossy(), &s, (320, 240, 25.0), &Overlays::default(),
        ) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("no GPU — skipping wipe test ({e})");
                return;
            }
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "wipe render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "wipe render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        // The overlap runs 1..3 in output time; t=2 is prog 0.5.
        let png = dir.join(format!("reel-wipe-f-{}.png", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-ss", "2", "-i", &out.to_string_lossy(),
                   "-frames:v", "1", &png.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));
        let img = image::open(&png).expect("read frame").to_rgb8();
        let px = |x: u32, y: u32| img.get_pixel(x, y).0;
        let [r, _, b] = px(60, 120);
        assert!(b > 130 && r < 90, "left quarter should be the incoming blue, got rgb{:?}", px(60, 120));
        let [r, _, b] = px(260, 120);
        assert!(r > 130 && b < 90, "right quarter should still be red, got rgb{:?}", px(260, 120));

        for f in [&red, &blue, &out, &png] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Green screen, end to end: a full-frame overlay of a red box on green
    /// is keyed over a blue base. Where the green was, blue must show; the
    /// red box must survive. Measured in the rendered pixels — the only
    /// definition of "the key works".
    #[test]
    fn a_green_screen_overlay_composites_over_the_cut() {
        use crate::edit::{OverlaySegment, Pip};
        let dir = std::env::temp_dir();
        let base = dir.join(format!("reel-key-base-{}.mp4", std::process::id()));
        let fg = dir.join(format!("reel-key-fg-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-key-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi",
                   "-i", "color=c=blue:size=640x480:rate=10:duration=2",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p", &base.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));
        // Green screen with a red box dead centre.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi",
                   "-i", "color=c=0x00b140:size=640x480:rate=10:duration=2,\
                          drawbox=x=220:y=160:w=200:h=160:color=red:t=fill",
                   "-c:v", "libx264", "-pix_fmt", "yuv420p", &fg.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let segs = vec![seg(&base.to_string_lossy(), 0.0, 2.0)];
        let mut fx = crate::effects::Effects::default();
        fx.key_color = Some([0.0, 0.694, 0.251]); // 0x00b140
        let ov = vec![OverlaySegment {
            source: fg.to_string_lossy().into(),
            in_point: 0.0,
            duration: 2.0,
            at: 0.0,
            // Full frame: the classic green-screen composite.
            pip: Pip { x: 0.5, y: 0.5, scale: 1.0 },
            gain_db: 0.0,
            effects: fx,
            keys: Vec::new(),
        }];
        let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
        let job = match crate::engine::render::start_timeline(
            &segs, &out.to_string_lossy(), &s, (640, 480, 10.0),
            &Overlays { overlays: &ov, ..Default::default() },
        ) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("no GPU — skipping chroma key test ({e})");
                return;
            }
        };
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "key render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "key render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        let png = dir.join(format!("reel-key-f-{}.png", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-ss", "1", "-i", &out.to_string_lossy(),
                   "-frames:v", "1", &png.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));
        let img = image::open(&png).expect("read frame").to_rgb8();
        let px = |x: u32, y: u32| img.get_pixel(x, y).0;

        // The corners were green screen — the blue base must show.
        for (x, y) in [(30, 30), (610, 30), (30, 450), (610, 450)] {
            let [r, g, b] = px(x, y);
            assert!(
                b > 120 && g < 110 && r < 90,
                "at ({x},{y}) expected the blue base, got rgb({r},{g},{b}) — key failed"
            );
        }
        // The red box survives the key.
        let [r, g, b] = px(320, 240);
        assert!(
            r > 130 && g < 100 && b < 100,
            "centre should keep the red subject, got rgb({r},{g},{b})"
        );

        for f in [&base, &fg, &out, &png] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// HDR in, correct SDR out. Two tagged PQ fixtures — 100-nit white and
    /// 5-nit near-black — rendered through the frame server. Without
    /// tone-mapping, PQ's curve reads the dim one at ~92/255 (washed) and
    /// the white one at ~192 (dull); linearised and mapped, the white lands
    /// bright and the dim one dark. This is the phone-footage bug every
    /// editor forum complains about, pinned in pixels.
    #[test]
    fn hdr_sources_are_tone_mapped_not_washed_out() {
        let dir = std::env::temp_dir();
        let make_pq = |name: &str, colour: &str| -> std::path::PathBuf {
            let f = dir.join(format!("reel-hdr-{name}-{}.mp4", std::process::id()));
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-f", "lavfi",
                       "-i", &format!("color=c={colour}:size=320x240:rate=25:duration=2"),
                       "-vf",
                       "format=yuv420p,setparams=range=tv:color_primaries=bt709:color_trc=bt709:colorspace=bt709,\
                        zscale=transfer=linear:npl=100,zscale=primaries=bt2020,\
                        zscale=transfer=smpte2084:matrix=bt2020nc,format=yuv420p10le",
                       "-c:v", "libx264", "-color_primaries", "bt2020",
                       "-color_trc", "smpte2084", "-colorspace", "bt2020nc",
                       &f.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            f
        };
        let white = make_pq("white", "white");
        // ~5 nits: 5% linear of the 100-nit reference.
        let dim = make_pq("dim", "0x3B3B3B");

        assert_eq!(
            crate::video::decoder::probe_transfer(&white.to_string_lossy()).as_deref(),
            Some("smpte2084"),
            "the PQ tag must be detected"
        );

        let level_of = |src: &std::path::Path| -> f64 {
            let out = dir.join(format!(
                "reel-hdr-out-{}-{}.mp4",
                src.file_stem().unwrap().to_string_lossy(),
                std::process::id()
            ));
            let _ = std::fs::remove_file(&out);
            let segs = vec![seg(&src.to_string_lossy(), 0.0, 1.0)];
            let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
            let job = match crate::engine::render::start_timeline(
                &segs, &out.to_string_lossy(), &s, (320, 240, 25.0), &Overlays::default(),
            ) {
                Ok(j) => j,
                Err(e) => {
                    eprintln!("no GPU — skipping HDR test ({e})");
                    return -1.0;
                }
            };
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let st = job.state();
                if st.finished {
                    assert!(st.error.is_none(), "HDR render failed: {:?}", st.error);
                    break;
                }
                assert!(Instant::now() < deadline, "HDR render timed out");
                std::thread::sleep(Duration::from_millis(50));
            }
            let png = dir.join(format!("reel-hdr-f-{}.png", std::process::id()));
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-ss", "0.5", "-i", &out.to_string_lossy(),
                       "-frames:v", "1", &png.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            let img = image::open(&png).expect("read frame").to_luma8();
            let _ = std::fs::remove_file(&png);
            let _ = std::fs::remove_file(&out);
            let sum: u64 = img.pixels().map(|p| p.0[0] as u64).sum();
            sum as f64 / (img.width() * img.height()) as f64
        };

        if !crate::engine::sources::have_libplacebo() {
            eprintln!("ffmpeg has no libplacebo — skipping HDR tone-map test");
            return;
        }
        let w = level_of(&white);
        if w < 0.0 {
            return; // no GPU
        }
        let d = level_of(&dim);
        // libplacebo maps against the BT.2408 203-nit reference: 100-nit
        // white lands around 185, 5-nit gray around 48. Without tone-mapping
        // both drift toward the murky middle (white ~158, dim ~92).
        assert!(
            (165.0..=215.0).contains(&w),
            "100-nit PQ white should land bright (~185), got {w:.0}"
        );
        assert!(d < 80.0, "5-nit PQ gray should land dark (~48), got {d:.0} — PQ read as sRGB");
        assert!(w / d.max(1.0) > 2.5, "the mapped contrast collapsed ({w:.0} vs {d:.0})");

        for f in [&white, &dim] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// The decisive ramp test: the source's LUMINANCE encodes its own time
    /// (brightness = source seconds), so each output frame tells us exactly
    /// which source moment it shows. A 1→3 ramp over 4 s must consume 8 s of
    /// source, accelerating along the integral — and the audio must still be
    /// exactly 4 s long.
    #[test]
    fn a_speed_ramp_accelerates_through_the_source_exactly() {
        use crate::edit::{Interp, Keyframe, Param};
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-ramp-src-{}.mp4", std::process::id()));
        let out = dir.join(format!("reel-ramp-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // Luma rises 25.5/s: source second N is gray level 25.5·N.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error",
                   "-f", "lavfi", "-i",
                   // The ramp lives in RGB so the YUV encode/decode round-trips;
                   // writing luma directly lands in limited range and skews it.
                   "color=c=black:size=320x240:rate=25:duration=10,format=rgb24,\
                    geq=r='clip(25.5*T,0,255)':g='clip(25.5*T,0,255)':b='clip(25.5*T,0,255)'",
                   "-f", "lavfi", "-i", "sine=frequency=440:duration=10",
                   "-c:v", "libx264", "-qp", "0", "-pix_fmt", "yuv420p",
                   "-c:a", "aac", "-shortest", &src.to_string_lossy()])
            .status().map(|st| st.success()).unwrap_or(false));

        let mut s0 = seg(&src.to_string_lossy(), 0.0, 4.0);
        s0.keys = vec![(
            Param::Speed,
            vec![
                Keyframe { t: 0.0, value: 1.0, interp: Interp::Linear },
                Keyframe { t: 4.0, value: 3.0, interp: Interp::Linear },
            ],
        )];
        assert!((s0.source_len() - 8.0).abs() < 1e-9, "1→3 over 4s must eat 8s of source");
        let segs = vec![s0];
        let s = ExportSettings { quality: Quality::High, hardware: false, ..Default::default() };
        let job = match crate::engine::render::start_timeline(
            &segs, &out.to_string_lossy(), &s, (320, 240, 25.0), &Overlays::default(),
        ) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("no GPU — skipping ramp render test ({e})");
                return;
            }
        };
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            let st = job.state();
            if st.finished {
                assert!(st.error.is_none(), "ramp render failed: {:?}", st.error);
                break;
            }
            assert!(Instant::now() < deadline, "ramp render timed out");
            std::thread::sleep(Duration::from_millis(50));
        }

        // Which source second is on screen at output time t?
        let src_time_at = |t: f64| -> f64 {
            let png = dir.join(format!("reel-ramp-f-{}.png", std::process::id()));
            assert!(std::process::Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-ss", &format!("{t}"), "-i", &out.to_string_lossy(),
                       "-frames:v", "1", &png.to_string_lossy()])
                .status().map(|st| st.success()).unwrap_or(false));
            let img = image::open(&png).expect("read frame").to_luma8();
            let _ = std::fs::remove_file(&png);
            let sum: u64 = img.pixels().map(|p| p.0[0] as u64).sum();
            (sum as f64 / (img.width() * img.height()) as f64) / 25.5
        };
        // integral(t) = t + t²/4 for a 1→3 linear ramp over 4 s.
        for (t, want) in [(0.4, 0.44), (2.0, 3.0), (3.8, 7.41)] {
            let got = src_time_at(t);
            assert!(
                (got - want).abs() < 0.35,
                "at output {t}s the picture shows source {got:.2}s, the curve says {want:.2}s"
            );
        }

        // The output (and its AUDIO) still fill exactly the 4 s slot.
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe");
        assert!((info.duration - 4.0).abs() < 0.3, "output ran {:.2}s", info.duration);
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "a:0",
                   "-show_entries", "stream=duration", "-of", "csv=p=0",
                   &out.to_string_lossy()])
            .output()
            .expect("ffprobe audio");
        let adur: f64 = String::from_utf8_lossy(&probe.stdout).trim().parse().unwrap_or(0.0);
        assert!(
            (adur - 4.0).abs() < 0.4,
            "ramped audio ran {adur:.2}s against a 4s slot — the piecewise tempo is off"
        );

        for f in [&src, &out] {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Both render paths — the frame server and the graph fallback — must
    /// produce the same cut. This drives each EXPLICITLY (the dispatcher
    /// picks the frame server wherever a GPU exists, so the graph would
    /// otherwise silently lose its coverage) and compares the outputs.
    #[test]
    fn both_render_paths_agree_on_a_real_cut() {
        let dir = std::env::temp_dir();
        let s = ExportSettings { quality: Quality::Small, hardware: false, ..Default::default() };
        let segs = vec![seg(&fixture(), 0.0, 0.4), seg(&fixture(), 1.4, 0.4)];
        let mut done = Vec::new();
        for (name, which) in [("fs", true), ("graph", false)] {
            let out = dir.join(format!("reel-paths-{name}-{}.mp4", std::process::id()));
            let _ = std::fs::remove_file(&out);
            let job = if which {
                match crate::engine::render::start_timeline(
                    &segs, &out.to_string_lossy(), &s, (320, 240, 30.0), &Overlays::default(),
                ) {
                    Ok(j) => j,
                    Err(e) => {
                        eprintln!("no GPU — skipping frame-server leg ({e})");
                        continue;
                    }
                }
            } else {
                start_timeline_graph(
                    &segs, &out.to_string_lossy(), &s, (320, 240, 30.0), Overlays::default(),
                )
                .expect("start graph render")
            };
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let st = job.state();
                if st.finished {
                    assert!(st.error.is_none(), "{name} render failed: {:?}", st.error);
                    break;
                }
                assert!(Instant::now() < deadline, "{name} render timed out");
                std::thread::sleep(Duration::from_millis(50));
            }
            let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe");
            assert!(
                (info.duration - 0.8).abs() < 0.3,
                "{name}: expected the 0.8s cut, got {:.2}s",
                info.duration
            );
            done.push((name, info.duration, out));
        }
        // When both ran, they must agree with each other, not just the spec.
        if done.len() == 2 {
            assert!(
                (done[0].1 - done[1].1).abs() < 0.15,
                "paths disagree on duration: {done:?}"
            );
        }
        for (_, _, f) in done {
            let _ = std::fs::remove_file(f);
        }
    }

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
            effects: Default::default(),
            keys: Vec::new(),
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
            transition_kind: Default::default(),
            stabilize: false,
            audio: Default::default(),
            gain_db: 0.0,
            speed: 2.0,
            keys: Vec::new(),
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
            Overlays { captions: &cues, caption_size: 20, titles: &titles, music: None, overlays: &[], markers: &[], marker_labels: &[], luts: &[], audio_clips: &[] },
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
            loudness: None,
            hdr_passthrough: false,
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
