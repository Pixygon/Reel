//! Video decode for v0.1: drive the system ffmpeg as a subprocess, piping raw
//! RGBA frames into a bounded channel (backpressure keeps decode ~in step with
//! playback). Metadata comes from ffprobe. The roadmap replaces this hot path
//! with libmpv / libav + libplacebo for the beat-VLC performance bar; the
//! Player API above it does not change when that happens.

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, Receiver};
use ffmpeg_sidecar::command::FfmpegCommand;
use ffmpeg_sidecar::event::FfmpegEvent;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: f64, // seconds
}

pub struct Frame {
    pub data: Vec<u8>, // RGBA8, width*height*4
    pub width: u32,
    pub height: u32,
    pub pts: f64, // seconds from start of file
}

/// Probe a media file for dimensions, frame rate and duration via ffprobe.
pub fn probe(path: &str) -> Result<VideoInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height,avg_frame_rate:format=duration",
            "-of", "json",
            path,
        ])
        .output()
        .map_err(|e| anyhow!("ffprobe failed to start ({e}); is ffmpeg installed?"))?;
    if !out.status.success() {
        return Err(anyhow!("ffprobe error: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let stream = v["streams"].get(0).ok_or_else(|| anyhow!("no video stream in {path}"))?;
    let width = stream["width"].as_u64().unwrap_or(0) as u32;
    let height = stream["height"].as_u64().unwrap_or(0) as u32;
    let fps = parse_ratio(stream["avg_frame_rate"].as_str().unwrap_or("0/1")).unwrap_or(30.0);
    let duration = v["format"]["duration"]
        .as_str()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);
    if width == 0 || height == 0 {
        return Err(anyhow!("could not read video dimensions for {path}"));
    }
    Ok(VideoInfo { width, height, fps: if fps > 0.0 { fps } else { 30.0 }, duration })
}

fn parse_ratio(s: &str) -> Option<f64> {
    let (n, d) = s.split_once('/')?;
    let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
    if d == 0.0 { None } else { Some(n / d) }
}

/// A running decode: a receiver of frames plus a stop flag to end it early
/// (e.g. on seek). Dropping the DecodeHandle signals the thread to stop.
pub struct DecodeHandle {
    pub rx: Receiver<Frame>,
    stop: Arc<AtomicBool>,
}

impl Drop for DecodeHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Spawn ffmpeg from `start` seconds, streaming RGBA frames. Input seeking
/// (`-ss` before `-i`) is fast — ffmpeg jumps to the nearest keyframe.
pub fn spawn(path: &str, start: f64, info: &VideoInfo) -> Result<DecodeHandle> {
    let (tx, rx) = bounded::<Frame>(8); // small buffer → decode stays near realtime
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let path = path.to_string();
    let fps = info.fps;

    std::thread::spawn(move || {
        let mut cmd = FfmpegCommand::new();
        if start > 0.0 {
            cmd.args(["-ss", &format!("{start:.3}")]);
        }
        cmd.input(&path).format("rawvideo").pix_fmt("rgba").output("-");

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                log::error!("ffmpeg spawn failed: {e}");
                return;
            }
        };
        let iter = match child.iter() {
            Ok(i) => i,
            Err(e) => {
                log::error!("ffmpeg iter failed: {e}");
                return;
            }
        };

        let mut index: u64 = 0;
        for event in iter {
            if stop_thread.load(Ordering::Relaxed) {
                let _ = child.kill();
                break;
            }
            if let FfmpegEvent::OutputFrame(frame) = event {
                let pts = start + (index as f64) / fps;
                index += 1;
                let f = Frame { data: frame.data, width: frame.width, height: frame.height, pts };
                // Blocking send applies backpressure; bail if the consumer is gone.
                if tx.send(f).is_err() {
                    let _ = child.kill();
                    break;
                }
            }
        }
    });

    Ok(DecodeHandle { rx, stop })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        format!("{}/tests/fixture.mp4", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn probe_reads_dimensions_fps_and_duration() {
        let info = probe(&fixture()).expect("probe fixture");
        assert_eq!(info.width, 320);
        assert_eq!(info.height, 240);
        assert!((info.fps - 30.0).abs() < 0.5, "fps ~30, got {}", info.fps);
        assert!(info.duration > 1.5 && info.duration < 2.5, "≈2s, got {}", info.duration);
    }

    #[test]
    fn decodes_rgba_frames_of_the_right_size() {
        let info = probe(&fixture()).expect("probe");
        let handle = spawn(&fixture(), 0.0, &info).expect("spawn decode");
        // Pull a handful of frames; each must be exactly width*height*4 RGBA bytes.
        let expected = (info.width * info.height * 4) as usize;
        let mut got = 0;
        for _ in 0..5 {
            match handle.rx.recv_timeout(std::time::Duration::from_secs(10)) {
                Ok(frame) => {
                    assert_eq!(frame.width, 320);
                    assert_eq!(frame.height, 240);
                    assert_eq!(frame.data.len(), expected, "RGBA frame size");
                    assert!(frame.pts >= 0.0);
                    got += 1;
                }
                Err(_) => break,
            }
        }
        assert!(got >= 3, "expected several frames, got {got}");
    }
}
