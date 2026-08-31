//! Audio waveforms for timeline clips.
//!
//! Cutting without seeing the audio is guesswork — you end up scrubbing back
//! and forth hunting for the start of a word. Every serious editor draws the
//! waveform on the clip, and it is one of the first things people notice is
//! missing.
//!
//! Peaks are computed by decoding the source to low-rate mono PCM through
//! ffmpeg (which is already a dependency) on a worker thread, then reduced to
//! one value per bucket. They are cached per source path, so a clip that gets
//! split, trimmed, moved or duplicated never pays for it twice — the drawing
//! code just reads a different window of the same array.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;

/// Buckets per second of source audio. 40 is enough to see syllables at
/// normal zoom without making the arrays large: an hour of audio is 144k
/// floats, about half a megabyte.
pub const BUCKETS_PER_SEC: f64 = 40.0;

/// Normalised peak amplitudes, one per bucket, in source order.
#[derive(Debug, Default)]
pub struct Peaks {
    pub data: Vec<f32>,
}

impl Peaks {
    /// The loudest peak in the source window `[from, to)`, sampled into
    /// `slots` buckets — exactly what the timeline needs to draw a clip of a
    /// given pixel width.
    pub fn window(&self, from: f64, to: f64, slots: usize) -> Vec<f32> {
        if self.data.is_empty() || slots == 0 || to <= from {
            return Vec::new();
        }
        let a = (from * BUCKETS_PER_SEC).max(0.0);
        let b = (to * BUCKETS_PER_SEC).min(self.data.len() as f64);
        if b <= a {
            return Vec::new();
        }
        let step = (b - a) / slots as f64;
        (0..slots)
            .map(|i| {
                let lo = (a + i as f64 * step) as usize;
                let hi = ((a + (i as f64 + 1.0) * step) as usize).max(lo + 1);
                self.data[lo.min(self.data.len() - 1)..hi.min(self.data.len())]
                    .iter()
                    .copied()
                    .fold(0.0f32, f32::max)
            })
            .collect()
    }
}

fn disk_path(source: &str) -> Option<std::path::PathBuf> {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache")
        });
    let dir = base.join("reel/waveforms");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join(format!("{}.f32", crate::proxy::file_key(source)?)))
}

/// Decode `source` to peaks — via the disk cache when this exact file was
/// decoded before (any session), so a reopened project is dressed at once.
pub fn compute(source: &str) -> Option<Peaks> {
    let cache = disk_path(source);
    if let Some(p) = &cache {
        if let Ok(bytes) = std::fs::read(p) {
            if bytes.len() >= 4 && bytes.len() % 4 == 0 {
                let data = bytes
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                return Some(Peaks { data });
            }
        }
    }
    let peaks = compute_uncached(source)?;
    if let Some(p) = &cache {
        let mut bytes = Vec::with_capacity(peaks.data.len() * 4);
        for v in &peaks.data {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let tmp = p.with_extension("part");
        if std::fs::write(&tmp, &bytes).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }
    Some(peaks)
}

fn compute_uncached(source: &str) -> Option<Peaks> {
    // 8 kHz mono 16-bit is far more than enough to draw an envelope, and
    // keeps the pipe small: ~16 KB per second of audio.
    const RATE: u32 = 8000;
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", source, "-vn", "-ac", "1", "-ar", &RATE.to_string(),
            "-f", "s16le", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let per_bucket = (RATE as f64 / BUCKETS_PER_SEC) as usize;
    let mut out = Peaks::default();
    let mut stdout = child.stdout.take()?;
    let mut buf = vec![0u8; per_bucket * 2 * 16];
    let mut bucket: Vec<i16> = Vec::with_capacity(per_bucket);
    // A pipe read can end mid-sample. The leftover byte has to survive into
    // the next read: dropping it would shift every following sample by one
    // byte and turn the rest of the waveform into noise.
    let mut odd: Option<u8> = None;
    loop {
        let n = match stdout.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut i = 0;
        if let Some(lo) = odd.take() {
            if n >= 1 {
                bucket.push(i16::from_le_bytes([lo, buf[0]]));
                i = 1;
            } else {
                odd = Some(lo);
            }
        }
        while i + 1 < n {
            bucket.push(i16::from_le_bytes([buf[i], buf[i + 1]]));
            i += 2;
        }
        if i < n {
            odd = Some(buf[n - 1]);
        }
        while bucket.len() >= per_bucket {
            let peak = bucket[..per_bucket]
                .iter()
                .map(|v| v.unsigned_abs() as f32)
                .fold(0.0, f32::max);
            out.data.push(peak / i16::MAX as f32);
            bucket.drain(..per_bucket);
        }
    }
    // Whatever is left is a partial final bucket — keep it, or every clip
    // loses up to 25 ms off its tail.
    if !bucket.is_empty() {
        let peak = bucket.iter().map(|v| v.unsigned_abs() as f32).fold(0.0, f32::max);
        out.data.push(peak / i16::MAX as f32);
    }
    let _ = child.wait();
    if out.data.is_empty() {
        return None;
    }
    // Normalise to the loudest point so a quiet recording is still legible.
    let max = out.data.iter().copied().fold(0.0f32, f32::max);
    if max > 0.0001 {
        for v in &mut out.data {
            *v /= max;
        }
    }
    Some(out)
}

/// Per-source peak cache with a background worker.
pub struct Cache {
    ready: HashMap<String, Arc<Peaks>>,
    /// Sources with no audio (or that failed) — remembered so we don't spawn
    /// a decode for them on every frame.
    barren: HashMap<String, ()>,
    pending: HashMap<String, ()>,
    tx: Sender<(String, Option<Peaks>)>,
    rx: Receiver<(String, Option<Peaks>)>,
}

impl Default for Cache {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ready: HashMap::new(),
            barren: HashMap::new(),
            pending: HashMap::new(),
            tx,
            rx,
        }
    }
}

impl Cache {
    /// Peaks for `source`, starting a decode in the background if this is the
    /// first time we've been asked. Returns None until they're ready — the
    /// timeline simply draws the clip without a waveform until then.
    pub fn get(&mut self, source: &str) -> Option<Arc<Peaks>> {
        self.drain();
        if let Some(p) = self.ready.get(source) {
            return Some(p.clone());
        }
        if self.barren.contains_key(source) || self.pending.contains_key(source) {
            return None;
        }
        self.pending.insert(source.to_string(), ());
        let (tx, src) = (self.tx.clone(), source.to_string());
        std::thread::spawn(move || {
            let peaks = compute(&src);
            let _ = tx.send((src, peaks));
        });
        None
    }

    /// Is a decode still running? The UI keeps repainting while so.
    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }

    fn drain(&mut self) {
        while let Ok((src, peaks)) = self.rx.try_recv() {
            self.pending.remove(&src);
            match peaks {
                Some(p) => {
                    self.ready.insert(src, Arc::new(p));
                }
                None => {
                    self.barren.insert(src, ());
                }
            }
        }
    }
}

/// The lag (in buckets) at which `b` best lines up inside `a`, by
/// normalised cross-correlation of the two envelopes: `b[i] ≈ a[i + lag]`.
/// This is how two cameras (or a camera and a recorder) that heard the same
/// room get synced without clap sticks.
pub fn best_lag(a: &[f32], b: &[f32], max_lag: usize) -> Option<(isize, f32)> {
    if a.len() < 8 || b.len() < 8 {
        return None;
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    let (ma, mb) = (mean(a), mean(b));
    let az: Vec<f32> = a.iter().map(|v| v - ma).collect();
    let bz: Vec<f32> = b.iter().map(|v| v - mb).collect();
    let max_lag = max_lag as isize;
    let mut best = (0isize, f32::MIN);
    for lag in -max_lag..=max_lag {
        let mut dot = 0.0f32;
        let mut na = 0.0f32;
        let mut nb = 0.0f32;
        let mut n = 0u32;
        for i in 0..bz.len() {
            let j = i as isize + lag;
            if j < 0 || j >= az.len() as isize {
                continue;
            }
            let (x, y) = (az[j as usize], bz[i]);
            dot += x * y;
            na += x * x;
            nb += y * y;
            n += 1;
        }
        // Demand a real overlap, or a sliver at the extremes wins by luck.
        if n < 40 {
            continue;
        }
        let denom = (na * nb).sqrt();
        if denom < 1e-9 {
            continue;
        }
        let score = dot / denom;
        if score > best.1 {
            best = (lag, score);
        }
    }
    (best.1 > f32::MIN).then_some(best)
}

/// The quietest window of at least `min_len` seconds in a peaks envelope —
/// where the room breathes without anyone talking. Returns (start_secs,
/// len_secs). Pure; the room-tone sampler is built on it.
pub fn quietest_span(peaks: &[f32], per_sec: f64, min_len: f64) -> Option<(f64, f64)> {
    let win = (min_len * per_sec).ceil() as usize;
    if peaks.len() < win || win == 0 {
        return None;
    }
    let mut sum: f32 = peaks[..win].iter().sum();
    let mut best = (0usize, sum);
    for i in win..peaks.len() {
        sum += peaks[i] - peaks[i - win];
        if sum < best.1 {
            best = (i - win + 1, sum);
        }
    }
    Some((best.0 as f64 / per_sec, min_len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The quietest-span finder lands on the actual hole in the envelope.
    #[test]
    fn quietest_span_finds_the_silence() {
        let per_sec = 40.0;
        let mut env = vec![0.8f32; 400]; // 10 s of loud
        for v in env.iter_mut().skip(240).take(48) {
            *v = 0.02; // 6.0–7.2 s: the room breathing
        }
        let (start, len) = quietest_span(&env, per_sec, 0.8).expect("span");
        assert!((start - 6.0).abs() < 0.3, "found {start}, wanted ~6.0");
        assert!((len - 0.8).abs() < 1e-9);
        assert!(quietest_span(&env[..10], per_sec, 0.8).is_none(), "too short = none");
    }

    /// The disk cache is real: a second compute of the same file reads the
    /// cached array instead of decoding again. Proven by planting a marker
    /// value in the cache file and getting it back.
    #[test]
    fn peaks_persist_on_disk_across_computes() {
        let wav = std::env::temp_dir().join(format!("reel-wavecache-{}.wav", std::process::id()));
        assert!(std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i",
                   "sine=frequency=440:duration=1", &wav.to_string_lossy()])
            .status().map(|s| s.success()).unwrap_or(false));
        let src = wav.to_string_lossy().into_owned();
        let cache = disk_path(&src).expect("cache path");
        let _ = std::fs::remove_file(&cache);
        let first = compute(&src).expect("decode");
        assert!(cache.exists(), "compute must leave the peaks on disk");
        // Plant a marker: if the next compute returns it, the cache was read.
        let mut marked = first.data.clone();
        marked[0] = 0.123_456;
        let bytes: Vec<u8> = marked.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(&cache, bytes).unwrap();
        let second = compute(&src).expect("cached read");
        assert_eq!(second.data[0], 0.123_456, "second compute must hit the disk cache");
        assert_eq!(second.data.len(), first.data.len());
        let _ = std::fs::remove_file(&wav);
        let _ = std::fs::remove_file(&cache);
    }

    /// The envelope has to actually follow the sound, or it is decoration.
    /// This builds audio that is silent, then loud, then silent, and checks
    /// the peaks say so.
    #[test]
    fn peaks_follow_the_sound() {
        let wav = std::env::temp_dir().join(format!("reel-wave-{}.wav", std::process::id()));
        let ok = Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-f", "lavfi",
                "-i", "sine=frequency=440:sample_rate=8000:duration=3",
                // Loud only in the middle second.
                "-af", "volume=volume='between(t,1,2)':eval=frame",
                &wav.to_string_lossy(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "could not build the waveform fixture");

        let peaks = compute(&wav.to_string_lossy()).expect("peaks");
        let _ = std::fs::remove_file(&wav);

        // ~40 buckets per second over 3 seconds.
        assert!(peaks.data.len() > 100, "too few buckets: {}", peaks.data.len());
        let quiet = peaks.window(0.2, 0.8, 8);
        let loud = peaks.window(1.2, 1.8, 8);
        let quiet_max = quiet.iter().copied().fold(0.0f32, f32::max);
        let loud_min = loud.iter().copied().fold(1.0f32, f32::min);
        assert!(
            loud_min > 0.5 && quiet_max < 0.2,
            "envelope does not follow the audio: quiet max {quiet_max:.2}, loud min {loud_min:.2}"
        );
    }

    /// The correlator's contract: a delayed copy of the same envelope is
    /// found at exactly its delay, with a near-perfect score.
    #[test]
    fn best_lag_finds_a_known_offset() {
        // A wandering envelope with real structure.
        let a: Vec<f32> = (0..2000)
            .map(|i| ((i as f32 * 0.037).sin() * (i as f32 * 0.011).cos()).abs())
            .collect();
        // b = a delayed by 173 buckets (b[i] = a[i + 173] ... b starts later
        // in the event, i.e. b[0] happens at a's bucket 173).
        let b: Vec<f32> = a[173..1800].to_vec();
        let (lag, score) = best_lag(&a, &b, 400).expect("a lag");
        assert_eq!(lag, 173, "found lag {lag}");
        assert!(score > 0.99, "score {score}");
        // And the mirror case: b PADDED with leading quiet finds a negative lag.
        let mut c = vec![0.0f32; 90];
        c.extend_from_slice(&a[..1200]);
        let (lag2, _) = best_lag(&a, &c, 400).expect("a lag");
        assert_eq!(lag2, -90);
    }

    #[test]
    fn a_window_always_fills_exactly_the_slots_asked_for() {
        let p = Peaks { data: (0..400).map(|i| (i % 100) as f32 / 100.0).collect() };
        assert_eq!(p.window(0.0, 10.0, 64).len(), 64);
        assert_eq!(p.window(1.0, 2.0, 7).len(), 7);
        // Degenerate asks must not panic or index out of range.
        assert!(p.window(0.0, 0.0, 10).is_empty());
        assert!(p.window(5.0, 1.0, 10).is_empty());
        assert!(p.window(0.0, 10.0, 0).is_empty());
        assert!(Peaks::default().window(0.0, 5.0, 10).is_empty());
        // A window past the end clamps instead of reading off the array.
        assert_eq!(p.window(0.0, 9999.0, 16).len(), 16);
    }
}
