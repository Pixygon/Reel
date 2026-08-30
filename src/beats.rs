//! Beat detection — markers you can cut to.
//!
//! Energy-flux onset detection: decode a fine envelope, take the positive
//! energy difference between neighbouring frames, and pick the peaks that
//! stand above the local average. No tempo model, no phase tracking — just
//! "where do hits land", which is what beat-snapped cutting needs.
//!
//! The detector is pure and unit-tested on synthetic click tracks; ffmpeg
//! only supplies the envelope.

use anyhow::{anyhow, Result};
use std::io::Read;

/// Envelope resolution — 100 frames/sec puts a beat within ±10 ms.
const FRAMES_PER_SEC: f64 = 100.0;
/// Two hits closer than this are one beat (240 BPM ceiling).
const MIN_SPACING: f64 = 0.25;

/// Detect beat times (seconds) in a media file's audio.
pub fn detect(source: &str) -> Result<Vec<f64>> {
    let env = envelope(source)?;
    if env.len() < 10 {
        return Err(anyhow!("not enough audio to find beats in"));
    }
    Ok(onsets(&env, FRAMES_PER_SEC))
}

/// The pure detector: RMS envelope in, beat times out.
pub fn onsets(env: &[f32], per_sec: f64) -> Vec<f64> {
    if env.len() < 4 {
        return Vec::new();
    }
    // Positive energy flux — rises only; a decaying tail is not an onset.
    let flux: Vec<f32> = std::iter::once(0.0)
        .chain(env.windows(2).map(|w| (w[1] - w[0]).max(0.0)))
        .collect();
    // Adaptive threshold: the local mean over ±0.5 s, scaled up, plus a
    // floor so silence stays beatless.
    let half = (per_sec * 0.5) as usize;
    let peak = flux.iter().cloned().fold(0.0f32, f32::max);
    let floor = peak * 0.1 + 1e-4;
    let mut beats = Vec::new();
    let mut last = f64::NEG_INFINITY;
    for i in 1..flux.len() - 1 {
        let a = i.saturating_sub(half);
        let b = (i + half).min(flux.len());
        let mean: f32 = flux[a..b].iter().sum::<f32>() / (b - a) as f32;
        let thresh = mean * 2.0 + floor;
        let is_peak = flux[i] > thresh && flux[i] >= flux[i - 1] && flux[i] >= flux[i + 1];
        let t = i as f64 / per_sec;
        if is_peak && t - last >= MIN_SPACING {
            beats.push(t);
            last = t;
        }
    }
    beats
}

/// Decode `source` to a 100 Hz RMS envelope (8 kHz mono s16 pipe, like the
/// waveform decoder).
fn envelope(source: &str) -> Result<Vec<f32>> {
    const RATE: usize = 8000;
    let mut child = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", source, "-vn",
            "-ac", "1", "-ar", &RATE.to_string(),
            "-f", "s16le", "-",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow!("could not start ffmpeg: {e}"))?;
    let mut raw = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("no pipe"))?
        .read_to_end(&mut raw)
        .map_err(|e| anyhow!("decode read failed: {e}"))?;
    let _ = child.wait();
    let per_frame = (RATE as f64 / FRAMES_PER_SEC) as usize; // 80 samples
    let mut env = Vec::with_capacity(raw.len() / 2 / per_frame + 1);
    let mut acc = 0.0f64;
    let mut n = 0usize;
    for chunk in raw.chunks_exact(2) {
        let v = i16::from_le_bytes([chunk[0], chunk[1]]) as f64 / 32768.0;
        acc += v * v;
        n += 1;
        if n == per_frame {
            env.push((acc / n as f64).sqrt() as f32);
            acc = 0.0;
            n = 0;
        }
    }
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic click track at 120 BPM: bursts every 0.5 s over a quiet
    /// floor. The detector finds each click, once, and nothing else.
    #[test]
    fn clicks_at_120_bpm_are_each_found_once() {
        let per_sec = 100.0;
        let mut env = vec![0.01f32; 900]; // 9 s of near-silence
        // First click at 0.5 s — a hit at exactly t=0 has no rise to see.
        for beat in 1..=16 {
            let at = (beat as f64 * 0.5 * per_sec) as usize;
            for (j, v) in [0.9f32, 0.7, 0.4, 0.2].iter().enumerate() {
                if at + j < env.len() {
                    env[at + j] = *v;
                }
            }
        }
        let beats = onsets(&env, per_sec);
        assert_eq!(beats.len(), 16, "16 clicks → 16 beats, got {beats:?}");
        for (i, t) in beats.iter().enumerate() {
            let want = (i + 1) as f64 * 0.5;
            assert!((t - want).abs() < 0.03, "beat {i} at {t}, expected {want}");
        }
        // Silence has no beats.
        assert!(onsets(&vec![0.01f32; 900], per_sec).is_empty());
    }

    /// End to end through real ffmpeg: a generated metronome renders, and
    /// the detector reads its tempo back.
    #[test]
    fn a_real_metronome_reads_back_at_its_tempo() {
        let dir = std::env::temp_dir();
        let src = dir.join(format!("reel-beats-{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&src);
        // 60 ms 880 Hz bursts every 0.5 s for 4 s.
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i",
                   "sine=frequency=880:duration=4,volume=volume='lt(mod(t,0.5),0.06)':eval=frame",
                   &src.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let beats = detect(&src.to_string_lossy()).expect("detect");
        assert!(
            (7..=9).contains(&beats.len()),
            "4 s at 120 BPM ≈ 8 beats, got {}: {beats:?}",
            beats.len()
        );
        for pair in beats.windows(2) {
            assert!(
                (pair[1] - pair[0] - 0.5).abs() < 0.06,
                "uneven spacing: {beats:?}"
            );
        }
        let _ = std::fs::remove_file(&src);
    }
}
