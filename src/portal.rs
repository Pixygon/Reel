//! Built-in Linux screen capture — no external tools. Talks to the desktop's
//! xdg-desktop-portal (the same door OBS uses): the system's own picker lets
//! the user choose whole screen / a window / a region, PipeWire delivers the
//! frames in-process, and ffmpeg encodes them to MP4 (with system audio when
//! a PulseAudio/PipeWire monitor is available).
//!
//! The portal issues a restore token on first approval; we persist it so
//! subsequent recordings skip the dialog.

#![cfg(target_os = "linux")]

use anyhow::{anyhow, bail, Result};
use ffmpeg_sidecar::command::FfmpegCommand;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn token_file() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(&std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("reel").join("screencast.token")
}

fn load_token() -> Option<String> {
    std::fs::read_to_string(token_file()).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn save_token(token: &str) {
    let f = token_file();
    if let Some(dir) = f.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(f, token);
}

// All ashpd calls share the process-wide runtime (runtime.rs) — ashpd caches
// its D-Bus connection against the first reactor it sees, so per-call
// runtimes break every call after the first.
use crate::runtime::rt;

/// Is a portal likely reachable at all (a Wayland/X11 desktop session)?
pub fn available() -> bool {
    std::env::var("DBUS_SESSION_BUS_ADDRESS").is_ok()
        && (std::env::var("WAYLAND_DISPLAY").is_ok() || std::env::var("DISPLAY").is_ok())
}

/// Interactive screenshot through the portal — the system dialog offers
/// screen / window / region. Returns the captured file (moved into `dest`).
pub fn screenshot_interactive(dest: PathBuf) -> Result<PathBuf> {
    let uri = rt().block_on(async {
        let resp = ashpd::desktop::screenshot::Screenshot::request()
            .interactive(true)
            .modal(false)
            .send()
            .await?
            .response()?;
        Ok::<_, ashpd::Error>(resp.uri().clone())
    })?;
    let src = uri
        .to_file_path()
        .map_err(|_| anyhow!("portal returned a non-file uri: {uri}"))?;
    // Rename first (same fs); fall back to copy+remove.
    if std::fs::rename(&src, &dest).is_err() {
        std::fs::copy(&src, &dest)?;
        let _ = std::fs::remove_file(&src);
    }
    Ok(dest)
}

struct NegotiatedFormat {
    width: u32,
    height: u32,
    /// ffmpeg rawvideo pix_fmt name for the negotiated SPA format.
    pix_fmt: &'static str,
}

/// Shared latest-frame slot: the PipeWire callback fills it, the encoder
/// ticker drains it at a fixed 30 fps so the output is clean CFR.
#[derive(Default)]
struct Shared {
    format: Option<NegotiatedFormat>,
    frame: Option<Vec<u8>>,
}

pub struct PortalRecorder {
    stop: pipewire::channel::Sender<()>,
    done: crossbeam_channel::Receiver<Result<PathBuf, String>>,
}

impl PortalRecorder {
    pub fn stop(self) -> Result<PathBuf> {
        let _ = self.stop.send(());
        self.done
            .recv_timeout(Duration::from_secs(15))
            .map_err(|_| anyhow!("recorder did not finish in time"))?
            .map_err(|e| anyhow!("{e}"))
    }
}

/// The system-audio monitor source, if one is discoverable.
fn audio_monitor() -> Option<String> {
    let out = Command::new("pactl").arg("get-default-sink").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let sink = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sink.is_empty()).then(|| format!("{sink}.monitor"))
}

fn spawn_encoder(f: &NegotiatedFormat, out: &Path, audio: Option<&str>) -> Result<Child> {
    let mut cmd = FfmpegCommand::new();
    cmd.args([
        "-f", "rawvideo",
        "-pix_fmt", f.pix_fmt,
        "-video_size", &format!("{}x{}", f.width, f.height),
        "-framerate", "30",
        "-i", "pipe:0",
    ]);
    if let Some(mon) = audio {
        cmd.args(["-f", "pulse", "-i", mon, "-map", "0:v", "-map", "1:a", "-c:a", "aac", "-b:a", "160k"]);
    }
    cmd.args([
        "-c:v", "libx264",
        "-preset", "veryfast",
        "-crf", "21",
        "-pix_fmt", "yuv420p",
        "-movflags", "+faststart",
        &out.to_string_lossy(),
    ]);
    let child = cmd
        .as_inner_mut()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(child)
}

/// Start a portal screen recording into `out`. Blocks through the system
/// picker (call from a worker thread); returns once frames are flowing.
pub fn start_recording(out: PathBuf) -> Result<PortalRecorder> {
    use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
    use ashpd::desktop::PersistMode;

    let (session, node_id, fd) = rt().block_on(async {
        let proxy = Screencast::new().await?;
        let session = proxy.create_session().await?;
        proxy
            .select_sources(
                &session,
                CursorMode::Embedded,
                SourceType::Monitor | SourceType::Window,
                false,
                load_token().as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await?;
        let response = proxy.start(&session, None).await?.response()?;
        if let Some(tok) = response.restore_token() {
            save_token(tok);
        }
        let stream = response
            .streams()
            .first()
            .ok_or(ashpd::Error::NoResponse)?
            .clone();
        let fd = proxy.open_pipe_wire_remote(&session).await?;
        Ok::<_, ashpd::Error>((session, stream.pipe_wire_node_id(), fd))
    })?;

    let shared = Arc::new(Mutex::new(Shared::default()));
    let stopped = Arc::new(AtomicBool::new(false));
    let (stop_tx, stop_rx) = pipewire::channel::channel::<()>();
    let (done_tx, done_rx) = crossbeam_channel::bounded(1);

    let t_shared = shared.clone();
    let t_stopped = stopped.clone();
    std::thread::spawn(move || {
        // Keep the portal session object alive for the recording's duration
        // (its runtime is the process-wide one and never dies).
        let _session_keepalive = session;
        let result = run_pipewire_capture(fd, node_id, out.clone(), t_shared, t_stopped, stop_rx);
        let _ = done_tx.send(result.map_err(|e| e.to_string()));
    });

    Ok(PortalRecorder { stop: stop_tx, done: done_rx })
}

fn spa_to_ffmpeg(fmt: pipewire::spa::param::video::VideoFormat) -> Option<&'static str> {
    use pipewire::spa::param::video::VideoFormat as VF;
    match fmt {
        VF::BGRx => Some("bgr0"),
        VF::RGBx => Some("rgb0"),
        VF::BGRA => Some("bgra"),
        VF::RGBA => Some("rgba"),
        VF::xRGB => Some("0rgb"),
        VF::xBGR => Some("0bgr"),
        _ => None,
    }
}

fn run_pipewire_capture(
    fd: std::os::fd::OwnedFd,
    node_id: u32,
    out: PathBuf,
    shared: Arc<Mutex<Shared>>,
    stopped: Arc<AtomicBool>,
    stop_rx: pipewire::channel::Receiver<()>,
) -> Result<PathBuf> {
    use pipewire as pw;
    use pw::spa;
    use spa::pod::serialize::PodSerializer;

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_fd_rc(fd, None)?;

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "reel-screen-capture",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )?;

    let cb_shared = shared.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |_, _, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else { return };
            let mut info = spa::param::video::VideoInfoRaw::default();
            if info.parse(param).is_err() {
                return;
            }
            let Some(pix_fmt) = spa_to_ffmpeg(info.format()) else {
                log::error!("unsupported screen format {:?}", info.format());
                return;
            };
            let size = info.size();
            log::info!("screen capture: {}×{} {:?}", size.width, size.height, info.format());
            cb_shared.lock().unwrap().format =
                Some(NegotiatedFormat { width: size.width, height: size.height, pix_fmt });
        })
        .process({
            let cb_shared = shared.clone();
            move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else { return };
                let datas = buffer.datas_mut();
                let Some(data) = datas.first_mut() else { return };
                let chunk_size = data.chunk().size() as usize;
                let stride = data.chunk().stride() as usize;
                let Some(bytes) = data.data() else { return };
                let mut sh = cb_shared.lock().unwrap();
                let Some(f) = &sh.format else { return };
                let (w, h) = (f.width as usize, f.height as usize);
                let row = w * 4;
                let mut frame = sh.frame.take().unwrap_or_default();
                frame.clear();
                frame.reserve(row * h);
                if stride == row || stride == 0 {
                    frame.extend_from_slice(&bytes[..(row * h).min(chunk_size.max(row * h)).min(bytes.len())]);
                    frame.resize(row * h, 0);
                } else {
                    for y in 0..h {
                        let start = y * stride;
                        if start + row <= bytes.len() {
                            frame.extend_from_slice(&bytes[start..start + row]);
                        }
                    }
                    frame.resize(row * h, 0);
                }
                sh.frame = Some(frame);
            }
        })
        .register()?;

    // Offer the formats we can hand to ffmpeg, any size, up to 60 fps.
    let obj = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
            spa::param::video::VideoFormat::xRGB,
            spa::param::video::VideoFormat::xBGR
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 16384, height: 16384 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 60, denom: 1 }
        ),
    );
    let values = PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
        .map_err(|e| anyhow!("pod serialize: {e:?}"))?
        .0
        .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).ok_or_else(|| anyhow!("bad format pod"))?];

    stream.connect(
        spa::utils::Direction::Input,
        Some(node_id),
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut params,
    )?;

    // Encoder ticker: waits for the negotiated format, then feeds ffmpeg the
    // latest frame at a steady 30 fps.
    let enc_shared = shared.clone();
    let enc_stopped = stopped.clone();
    let enc_out = out.clone();
    let encoder = std::thread::spawn(move || -> Result<()> {
        // Wait (bounded) for the first frame + format.
        let t0 = Instant::now();
        let fmt = loop {
            if enc_stopped.load(Ordering::Relaxed) {
                return Ok(());
            }
            {
                let sh = enc_shared.lock().unwrap();
                if let (Some(f), Some(_)) = (&sh.format, &sh.frame) {
                    break NegotiatedFormat { width: f.width, height: f.height, pix_fmt: f.pix_fmt };
                }
            }
            if t0.elapsed() > Duration::from_secs(20) {
                bail!("no frames arrived from the compositor");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let audio = audio_monitor();
        let mut child = spawn_encoder(&fmt, &enc_out, audio.as_deref())?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("encoder stdin missing"))?;
        let frame_len = (fmt.width * fmt.height * 4) as usize;
        let mut current: Vec<u8> = vec![0; frame_len];
        let tick = Duration::from_micros(33_333);
        let mut next = Instant::now();
        while !enc_stopped.load(Ordering::Relaxed) {
            if let Some(f) = enc_shared.lock().unwrap().frame.take() {
                if f.len() == frame_len {
                    current = f;
                }
            }
            if stdin.write_all(&current).is_err() {
                break; // encoder died; surfaced via wait() status below
            }
            next += tick;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now; // fell behind; don't burst
            }
        }
        drop(stdin); // EOF → ffmpeg finalizes the file
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
                _ => {
                    let _ = child.kill();
                    break;
                }
            }
        }
        Ok(())
    });

    // Run until the app asks us to stop.
    let loop_clone = mainloop.clone();
    let _receiver = stop_rx.attach(mainloop.loop_(), move |()| {
        loop_clone.quit();
    });
    mainloop.run();

    stopped.store(true, Ordering::Relaxed);
    drop(stream);
    match encoder.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => bail!("encoder thread panicked"),
    }
    if out.exists() {
        Ok(out)
    } else {
        bail!("recording produced no file")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder leg on its own: synthetic frames in, a valid MP4 out —
    /// exactly what the PipeWire callback feeds it during a real recording.
    #[test]
    fn encoder_turns_raw_frames_into_mp4() {
        let out = std::env::temp_dir().join(format!("reel-encoder-test-{}.mp4", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let fmt = NegotiatedFormat { width: 320, height: 240, pix_fmt: "bgr0" };
        let mut child = spawn_encoder(&fmt, &out, None).expect("spawn encoder");
        let mut stdin = child.stdin.take().expect("stdin");
        let frame = vec![0x40u8; (fmt.width * fmt.height * 4) as usize];
        for _ in 0..30 {
            stdin.write_all(&frame).expect("feed frame");
        }
        drop(stdin);
        let status = child.wait().expect("encoder exit");
        assert!(status.success(), "ffmpeg exited with {status:?}");
        let info = crate::video::decoder::probe(&out.to_string_lossy()).expect("probe output");
        assert_eq!((info.width, info.height), (320, 240));
        assert!(info.duration > 0.5, "≈1s of video, got {}", info.duration);
        let _ = std::fs::remove_file(&out);
    }
}
