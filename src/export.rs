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
use std::sync::{Arc, Mutex};

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
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            codec: Codec::H264,
            quality: Quality::Balanced,
            resolution: Resolution::Source,
            audio: AudioMode::Encode { kbps: 160 },
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
        if let Some(h) = s.resolution.height() {
            // -2: keep aspect, round width to even (encoders require it).
            a.extend(["-vf".into(), format!("scale=-2:{h}:flags=lanczos")]);
        }
    }

    match s.codec {
        Codec::H264 => a.extend(["-c:v".into(), "libx264".into(), "-preset".into(), "medium".into()]),
        Codec::H265 => a.extend(["-c:v".into(), "libx265".into(), "-preset".into(), "medium".into(), "-tag:v".into(), "hvc1".into()]),
        Codec::Av1 => a.extend(["-c:v".into(), "libsvtav1".into(), "-preset".into(), "6".into()]),
        Codec::Vp9 => a.extend(["-c:v".into(), "libvpx-vp9".into(), "-b:v".into(), "0".into(), "-row-mt".into(), "1".into()]),
        Codec::Remux => a.extend(["-c".into(), "copy".into()]),
        _ => unreachable!(),
    }
    if s.codec != Codec::Remux {
        a.extend(["-crf".into(), s.codec.crf(s.quality).to_string()]);
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

/// Start an export on a worker thread. `duration` is the source duration in
/// seconds (drives the progress fraction).
pub fn start(input: &str, output: &str, settings: &ExportSettings, duration: f64) -> Result<ExportJob> {
    if Path::new(output).exists() {
        return Err(anyhow!("output already exists: {output}"));
    }
    let args = build_args(input, output, settings);
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
