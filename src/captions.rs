//! Captions, generated on this machine and nowhere else.
//!
//! This is the feature short-form creators say they cannot work without, and
//! the one that keeps people on tools that upload their audio: CapCut's
//! auto-captions run in the cloud, and the open-source alternatives exist but
//! "take some setup and scripts" — which is exactly why people don't use them.
//!
//! So the bar here is not "we support captions", it is **one button, no
//! setup**. If the machine has whisper.cpp, Reel uses it; if it doesn't, Reel
//! fetches the official prebuilt engine (~9 MB) and the model itself on first
//! use. Either way the whole job is local: no account, no upload, no
//! per-minute billing, works on a plane.
//!
//! The transcription engine runs as a subprocess, the same proven pattern we
//! use for ffmpeg: no linking, no build-time C++ toolchain, and a failure
//! can never take the editor down with it.

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// One caption: a line of text with the timeline window it belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Whisper models we offer, smallest first. English-only variants are much
/// faster and are the right default for social video.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    TinyEn,
    BaseEn,
    SmallEn,
}

impl Model {
    pub const ALL: [Model; 3] = [Model::TinyEn, Model::BaseEn, Model::SmallEn];

    pub fn label(self) -> &'static str {
        match self {
            Model::TinyEn => "Fast (tiny, ~75 MB)",
            Model::BaseEn => "Balanced (base, ~148 MB)",
            Model::SmallEn => "Accurate (small, ~488 MB)",
        }
    }

    fn file(self) -> &'static str {
        match self {
            Model::TinyEn => "ggml-tiny.en.bin",
            Model::BaseEn => "ggml-base.en.bin",
            Model::SmallEn => "ggml-small.en.bin",
        }
    }

    fn url(self) -> String {
        format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}?download=true",
            self.file()
        )
    }

    pub fn path(self) -> PathBuf {
        model_dir().join(self.file())
    }

    pub fn is_downloaded(self) -> bool {
        self.path().exists()
    }
}

fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(&std::env::var("HOME").unwrap_or_default()).join(".cache")
        });
    base.join("reel")
}

fn model_dir() -> PathBuf {
    cache_dir().join("models")
}

/// Where a Reel-fetched engine lives. Upstream's builds set RUNPATH=$ORIGIN,
/// so the whole directory is self-contained once extracted — no installer,
/// no system libraries, nothing left behind outside this folder.
fn engine_dir() -> PathBuf {
    cache_dir().join("engine")
}

/// The whisper.cpp executable, if this machine already has one on PATH.
/// Named `whisper-cli` in current builds; older packages shipped it as
/// `main` or `whisper`.
pub fn system_engine() -> Option<PathBuf> {
    let which = if cfg!(windows) { "where" } else { "which" };
    for name in ["whisper-cli", "whisper.cpp", "whisper", "whisper-cpp"] {
        if let Ok(out) = Command::new(which).arg(name).output() {
            if out.status.success() {
                let p = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !p.is_empty() {
                    return Some(PathBuf::from(p));
                }
            }
        }
    }
    None
}

fn fetched_engine() -> Option<PathBuf> {
    let exe = engine_dir().join(if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" });
    exe.exists().then_some(exe)
}

/// An engine we can run right now, without fetching anything.
pub fn engine() -> Option<PathBuf> {
    system_engine().or_else(fetched_engine)
}

/// The upstream release we fetch when a machine has no engine. Pinned rather
/// than "latest" so a Reel build always gets a binary we have actually run.
const ENGINE_RELEASE: &str = "b4938";

/// The prebuilt archive for this platform, if upstream publishes one.
fn engine_asset() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("whisper-bin-ubuntu-x64.tar.gz"),
        ("linux", "aarch64") => Some("whisper-bin-ubuntu-arm64.tar.gz"),
        ("windows", "x86_64") => Some("whisper-bin-x64.zip"),
        _ => None,
    }
}

/// Get an engine, fetching the official prebuilt one if this machine has
/// none. This is what turns captions from "install these tools first" into
/// one button.
pub fn ensure_engine(set: &dyn Fn(&str, f32), cancel: &AtomicBool) -> Result<PathBuf> {
    if let Some(e) = engine() {
        return Ok(e);
    }
    let asset = engine_asset().ok_or_else(|| {
        anyhow!("no caption engine for this platform — install whisper.cpp to enable captions")
    })?;
    let url = format!(
        "https://github.com/ggml-org/whisper.cpp/releases/download/{ENGINE_RELEASE}/{asset}"
    );
    set("Fetching the caption engine (first time only)…", 0.0);

    let dir = engine_dir();
    std::fs::create_dir_all(&dir)?;
    let archive = dir.join(asset);
    download_to(&url, &archive, "Fetching the caption engine", set, cancel)?;

    // bsdtar handles both .tar.gz and .zip, and ships with Windows 10+ as
    // tar.exe — one extraction path for every platform we build for.
    let ok = Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("--strip-components=1")
        .arg("-C")
        .arg(&dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&archive);
    if !ok {
        bail!("could not unpack the caption engine");
    }
    // Some archives nest one level deeper than --strip-components handles.
    if fetched_engine().is_none() {
        if let Some(found) = find_engine_under(&dir, 3) {
            if let Some(parent) = found.parent() {
                if parent != dir {
                    move_dir_contents(parent, &dir)?;
                }
            }
        }
    }
    #[cfg(unix)]
    if let Some(exe) = fetched_engine() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    fetched_engine().ok_or_else(|| anyhow!("the caption engine unpacked without a runnable binary"))
}

fn find_engine_under(dir: &Path, depth: u32) -> Option<PathBuf> {
    let target = if cfg!(windows) { "whisper-cli.exe" } else { "whisper-cli" };
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            dirs.push(p);
        } else if p.file_name().is_some_and(|n| n == target) {
            return Some(p);
        }
    }
    if depth == 0 {
        return None;
    }
    dirs.into_iter().find_map(|d| find_engine_under(&d, depth - 1))
}

fn move_dir_contents(from: &Path, to: &Path) -> Result<()> {
    for e in std::fs::read_dir(from)?.flatten() {
        let dest = to.join(e.file_name());
        if dest.exists() {
            continue;
        }
        std::fs::rename(e.path(), dest)?;
    }
    Ok(())
}

/// Progress of a captioning run, polled by the UI.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// What's happening right now, in the user's language.
    pub stage: String,
    /// 0..=1 where we can measure it (model download); 0 otherwise.
    pub fraction: f32,
    pub finished: bool,
    pub error: Option<String>,
    pub cues: Vec<Cue>,
}

pub struct Job {
    state: Arc<Mutex<Progress>>,
    cancel: Arc<AtomicBool>,
}

impl Job {
    pub fn state(&self) -> Progress {
        self.state.lock().unwrap().clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Transcribe `media` in the background. Downloads the model on first use.
pub fn start(media: &str, model: Model) -> Job {
    let state = Arc::new(Mutex::new(Progress {
        stage: "Starting…".into(),
        ..Default::default()
    }));
    let cancel = Arc::new(AtomicBool::new(false));
    let (t_state, t_cancel, media) = (state.clone(), cancel.clone(), media.to_string());

    std::thread::spawn(move || {
        let set = |stage: &str, frac: f32| {
            let mut s = t_state.lock().unwrap();
            s.stage = stage.to_string();
            s.fraction = frac;
        };
        let finish = |err: Option<String>, cues: Vec<Cue>| {
            let mut s = t_state.lock().unwrap();
            s.finished = true;
            s.error = err;
            s.cues = cues;
        };

        match run(&media, model, &set, &t_cancel) {
            Ok(cues) => finish(None, cues),
            Err(e) => finish(Some(e.to_string()), Vec::new()),
        }
    });

    Job { state, cancel }
}

fn run(
    media: &str,
    model: Model,
    set: &dyn Fn(&str, f32),
    cancel: &AtomicBool,
) -> Result<Vec<Cue>> {
    let engine = ensure_engine(set, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }

    if !model.is_downloaded() {
        set("Fetching the speech model (first time only)…", 0.0);
        download_model(model, set, cancel)?;
    }
    if cancel.load(Ordering::Relaxed) {
        bail!("cancelled");
    }

    // whisper.cpp wants 16 kHz mono PCM; ffmpeg is already a hard dependency.
    set("Extracting audio…", 0.0);
    let tmp = std::env::temp_dir().join(format!("reel-captions-{}", std::process::id()));
    let wav = tmp.with_extension("wav");
    let ok = Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i", media, "-vn", "-ac", "1", "-ar", "16000",
               "-c:a", "pcm_s16le", &wav.to_string_lossy()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        bail!("could not read the audio from this file");
    }

    set("Listening…", 0.0);
    let prefix = tmp.to_string_lossy().to_string();
    let out = Command::new(&engine)
        .args([
            "-m", &model.path().to_string_lossy(),
            "-f", &wav.to_string_lossy(),
            "-osrt",              // write <prefix>.srt
            "-of", &prefix,
            // NB: do NOT pass -nt here. It reads like "don't print
            // timestamps to the console", but it also strips them from the
            // SRT writer: every segment collapses into one 30-second cue,
            // so the captions stop following the speech entirely.
        ])
        .output()
        .map_err(|e| anyhow!("caption engine failed to start: {e}"))?;
    let _ = std::fs::remove_file(&wav);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("caption engine error: {}", err.lines().last().unwrap_or("unknown"));
    }

    let srt_path = PathBuf::from(format!("{prefix}.srt"));
    let srt = std::fs::read_to_string(&srt_path)
        .map_err(|e| anyhow!("the engine produced no captions ({e})"))?;
    let _ = std::fs::remove_file(&srt_path);
    let cues = parse_srt(&srt);
    if cues.is_empty() {
        bail!("no speech found in this file");
    }
    Ok(cues)
}

fn download_model(model: Model, set: &dyn Fn(&str, f32), cancel: &AtomicBool) -> Result<()> {
    std::fs::create_dir_all(model_dir())?;
    download_to(&model.url(), &model.path(), "Fetching the speech model", set, cancel)
}

/// Stream a URL to a file, reporting progress and honouring cancel. Writes to
/// a `.part` and renames only on success, so an interrupted download is never
/// mistaken for a usable file.
fn download_to(
    url: &str,
    target: &Path,
    label: &str,
    set: &dyn Fn(&str, f32),
    cancel: &AtomicBool,
) -> Result<()> {
    let tmp = target.with_extension("part");
    let mut resp = ureq::get(url)
        .call()
        .map_err(|e| anyhow!("could not reach the download host: {e}"))?;
    let total: u64 = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&tmp)?;
    let mut buf = vec![0u8; 1 << 16];
    let mut done: u64 = 0;
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&tmp);
            bail!("cancelled");
        }
        let n = std::io::Read::read(&mut reader, &mut buf)?;
        if n == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buf[..n])?;
        done += n as u64;
        if total > 0 {
            set(
                &format!("{label} — {} of {} MB", done >> 20, total >> 20),
                done as f32 / total as f32,
            );
        }
    }
    drop(file);
    std::fs::rename(&tmp, target)?;
    Ok(())
}

/// Parse SRT into cues. Tolerant of the usual variations: CRLF, comma or dot
/// decimals, multi-line text, and missing indices.
pub fn parse_srt(srt: &str) -> Vec<Cue> {
    let mut cues = Vec::new();
    for block in srt.replace("\r\n", "\n").split("\n\n") {
        let mut lines = block.trim().lines();
        let Some(first) = lines.next() else { continue };
        // The index line is optional; the timing line contains "-->".
        let timing = if first.contains("-->") {
            first.to_string()
        } else {
            match lines.next() {
                Some(l) if l.contains("-->") => l.to_string(),
                _ => continue,
            }
        };
        let Some((a, b)) = timing.split_once("-->") else { continue };
        let (Some(start), Some(end)) = (parse_ts(a.trim()), parse_ts(b.trim())) else { continue };
        let text = lines.collect::<Vec<_>>().join(" ").trim().to_string();
        if !text.is_empty() {
            cues.push(Cue { start, end, text });
        }
    }
    cues
}

fn parse_ts(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let mut parts = s.split(':');
    let h: f64 = parts.next()?.trim().parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Serialize cues back to SRT — this is what gets burned into the render.
pub fn to_srt(cues: &[Cue]) -> String {
    let fmt = |t: f64| {
        let t = t.max(0.0);
        let h = (t / 3600.0) as u64;
        let m = ((t % 3600.0) / 60.0) as u64;
        let s = t % 60.0;
        format!("{h:02}:{m:02}:{:02},{:03}", s as u64, ((s - s.floor()) * 1000.0) as u64)
    };
    cues.iter()
        .enumerate()
        .map(|(i, c)| format!("{}\n{} --> {}\n{}\n", i + 1, fmt(c.start), fmt(c.end), c.text))
        .collect::<Vec<_>>()
        .join("\n")
}

/// libass renders an SRT through a script whose PlayResY is 288, then scales
/// that script to the video height. So every number below is a FRACTION of
/// the frame in disguise — the same settings look identical at 720p and 4K,
/// and nothing here may be scaled by the export resolution (doing so made
/// captions grow quadratically with frame height).
pub const PLAY_RES_Y: f32 = 288.0;
/// Distance from the bottom of the frame to the bottom of the text.
pub const MARGIN_V: u32 = 30;
/// Outline thickness, in the same units.
pub const OUTLINE: f32 = 2.0;

/// Caption metrics as fractions of the picture height. The preview and the
/// render both derive from this, so what you read on screen is what burns in.
pub fn metrics(size: u32) -> CaptionMetrics {
    CaptionMetrics {
        font: size as f32 / PLAY_RES_Y,
        margin_bottom: MARGIN_V as f32 / PLAY_RES_Y,
        outline: OUTLINE / PLAY_RES_Y,
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CaptionMetrics {
    /// Em size, as a fraction of picture height.
    pub font: f32,
    /// Gap below the text, as a fraction of picture height.
    pub margin_bottom: f32,
    /// Outline thickness, as a fraction of picture height.
    pub outline: f32,
}

/// The caption look, as an ffmpeg `force_style` string. Kept here so the
/// burned-in result and the preview are described by one set of numbers.
pub fn force_style(size: u32) -> String {
    format!(
        "FontName=DejaVu Sans,Fontsize={size},Bold=1,PrimaryColour=&H00FFFFFF,\
         OutlineColour=&H00000000,BorderStyle=1,Outline={},Shadow=1,Alignment=2,MarginV={MARGIN_V}",
        OUTLINE
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srt_round_trips() {
        let cues = vec![
            Cue { start: 0.0, end: 1.5, text: "Hello there".into() },
            Cue { start: 1.5, end: 4.25, text: "second line".into() },
        ];
        let srt = to_srt(&cues);
        assert!(srt.contains("00:00:00,000 --> 00:00:01,500"), "{srt}");
        let back = parse_srt(&srt);
        assert_eq!(back.len(), 2);
        for (a, b) in cues.iter().zip(back.iter()) {
            assert!((a.start - b.start).abs() < 0.002, "{a:?} vs {b:?}");
            assert!((a.end - b.end).abs() < 0.002);
            assert_eq!(a.text, b.text);
        }
    }

    #[test]
    fn parses_what_whisper_actually_emits() {
        // CRLF, an index line, multi-line text — all normal in the wild.
        let srt = "1\r\n00:00:00,000 --> 00:00:02,000\r\nAnd so my fellow\r\nAmericans\r\n\r\n\
                   2\r\n00:00:02,000 --> 00:00:05,120\r\nask not what your country can do\r\n";
        let cues = parse_srt(srt);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "And so my fellow Americans");
        assert!((cues[1].end - 5.12).abs() < 1e-6);

        // Junk in, nothing out — never a panic.
        assert!(parse_srt("").is_empty());
        assert!(parse_srt("not an srt at all").is_empty());
        assert!(parse_srt("1\n99:99 --> broken\ntext").is_empty());
    }

    #[test]
    fn the_caption_look_is_legible_over_video() {
        let s = force_style(20);
        // An outline is what makes white text readable on a bright frame.
        assert!(s.contains(&format!("Outline={OUTLINE}")) && s.contains("Bold=1"));
        assert!(s.contains("Alignment=2"), "captions sit bottom-centre");
        assert!(s.contains("Fontsize=20"), "the size setting reaches the render");
        // Resolution must NOT appear here — libass normalises for us.
        assert!(!s.contains("1080"), "caption style must be resolution-independent");
    }

    /// Where a caption actually lands in a render, as fractions of the frame:
    /// (bottom gap, text height, left edge, right edge).
    fn burned_geometry(size: u32, w: u32, h: u32) -> (f32, f32, f32, f32) {
        let dir = std::env::temp_dir();
        let srt = dir.join(format!("reel-capgeo-{}-{w}.srt", std::process::id()));
        let png = dir.join(format!("reel-capgeo-{}-{w}.png", std::process::id()));
        std::fs::write(
            &srt,
            to_srt(&[Cue { start: 0.0, end: 2.0, text: "HELLO".into() }]),
        )
        .unwrap();
        let escaped = srt.to_string_lossy().replace('\'', "\\'");
        let vf = format!(
            "subtitles='{escaped}':force_style='{}'",
            force_style(size)
        );
        let ok = Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-f", "lavfi",
                "-i", &format!("color=c=black:size={w}x{h}:rate=1:duration=1"),
                "-vf", &vf, "-frames:v", "1", &png.to_string_lossy(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "ffmpeg could not burn the caption");
        let img = image::open(&png).expect("read burned png").to_luma8();
        let _ = std::fs::remove_file(&srt);
        let _ = std::fs::remove_file(&png);

        // The frame is black, so anything bright is the caption.
        let (mut top, mut bottom, mut left, mut right) = (u32::MAX, 0u32, u32::MAX, 0u32);
        for (x, y, px) in img.enumerate_pixels() {
            if px.0[0] > 200 {
                top = top.min(y);
                bottom = bottom.max(y);
                left = left.min(x);
                right = right.max(x);
            }
        }
        assert!(bottom > 0, "no caption pixels in the render at {w}x{h}");
        (
            (h - 1 - bottom) as f32 / h as f32,
            (bottom - top + 1) as f32 / h as f32,
            left as f32 / w as f32,
            right as f32 / w as f32,
        )
    }

    /// The contract, same as effects: the preview and the render are two
    /// drawings of ONE formula. `metrics()` is that formula, so a real render
    /// has to agree with it — and, because libass normalises to PLAY_RES_Y,
    /// has to look the same at every resolution.
    ///
    /// This caught a real bug: the export scaled Fontsize by the frame
    /// height, which libass then scaled again, so 4K captions came out
    /// enormous and nothing matched the preview.
    #[test]
    fn the_burned_caption_matches_the_previewed_formula() {
        let size = 20;
        let m = metrics(size);
        let small = burned_geometry(size, 640, 360);
        let large = burned_geometry(size, 1280, 720);

        for (label, g) in [("640x360", small), ("1280x720", large)] {
            // Sits the specified distance above the bottom edge.
            assert!(
                (g.0 - m.margin_bottom).abs() < 0.02,
                "{label}: caption bottom at {:.3} of frame, formula says {:.3}",
                g.0, m.margin_bottom
            );
            // Cap height is a fraction of the em box — loose, but it pins the
            // order of magnitude, which is what actually went wrong before.
            assert!(
                g.1 > m.font * 0.4 && g.1 < m.font * 1.2,
                "{label}: caption height {:.3} not consistent with em {:.3}",
                g.1, m.font
            );
            // Centred.
            let centre = (g.2 + g.3) / 2.0;
            assert!((centre - 0.5).abs() < 0.02, "{label}: caption not centred ({centre:.3})");
        }

        // Resolution independence: doubling the frame must not change how big
        // the caption looks.
        assert!(
            (small.0 - large.0).abs() < 0.015 && (small.1 - large.1).abs() < 0.015,
            "caption geometry drifts with resolution: {small:?} vs {large:?}"
        );
    }

    /// The whole promise, end to end: real speech in, correct words out, on
    /// this machine. Nothing here is mocked — it runs the actual engine on an
    /// actual recording and reads the actual words back.
    ///
    /// The engine (~9 MB) and model (~75 MB) are fetched on first use, which
    /// is the point of the feature but too much for an offline run — so the
    /// test fetches only when REEL_TEST_NET=1, and otherwise uses whatever is
    /// already cached.
    #[test]
    fn transcribes_speech_locally() {
        let may_fetch = std::env::var("REEL_TEST_NET").as_deref() == Ok("1");
        if !may_fetch && (engine().is_none() || !Model::TinyEn.is_downloaded()) {
            eprintln!("no cached caption engine/model — set REEL_TEST_NET=1 to fetch and run");
            return;
        }
        let fixture = format!("{}/tests/speech.wav", env!("CARGO_MANIFEST_DIR"));
        let job = start(&fixture, Model::TinyEn);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let st = loop {
            let st = job.state();
            if st.finished {
                break st;
            }
            assert!(std::time::Instant::now() < deadline, "captioning hung");
            std::thread::sleep(std::time::Duration::from_millis(200));
        };
        assert!(st.error.is_none(), "captioning failed: {:?}", st.error);
        assert!(!st.cues.is_empty(), "no cues from a file that is entirely speech");

        // The fixture is the "ask not what your country can do for you" line.
        let text = st.cues.iter().map(|c| c.text.to_lowercase()).collect::<Vec<_>>().join(" ");
        assert!(text.contains("country"), "transcript missed the words: {text:?}");
        assert!(text.contains("americans"), "transcript missed the words: {text:?}");

        // Cues must be ordered and non-degenerate, or captions flash on screen.
        for w in st.cues.windows(2) {
            assert!(w[1].start >= w[0].start, "cues out of order");
        }
        for c in &st.cues {
            assert!(c.end > c.start, "zero-length cue: {c:?}");
        }

        // The timings have to be REAL. Passing whisper `-nt` once collapsed
        // every segment into a single cue running to 30 s — the words were
        // still right, so a words-only assertion missed it completely, and
        // captions sat on screen as one block for the whole video.
        let fixture_len = 11.0;
        let last = st.cues.iter().map(|c| c.end).fold(0.0, f64::max);
        assert!(
            last <= fixture_len + 1.0,
            "captions run to {last:.1}s on a {fixture_len:.0}s recording — timings are not real"
        );
        assert!(
            st.cues.len() > 1,
            "expected the speech to break into several cues, got one: {:?}",
            st.cues
        );
    }
}
