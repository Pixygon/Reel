//! The editor's live audio mixer — roadmap Phase 1.4, the preview half.
//!
//! Until now the preview played only the main clip's own audio through mpv:
//! per-clip gain, fades, the second clip of a crossfade, A1 clips and the
//! whole music bed (ducking included) existed only at export. That is a
//! preview that lies by omission. This mixer renders the timeline's audio
//! live: every clip with sound, the music bed, gains, fades and ducking.
//!
//! Architecture, deliberately in three separable pieces:
//!
//!  * `SampleCache` — decodes each source to 48 kHz stereo f32 in memory on
//!    worker threads (ffmpeg pipe, like waveforms but full quality).
//!  * `Plan` + `render_into` — a PURE mixing function over immutable data:
//!    plan in, samples out. Every mixing rule is unit-tested here without
//!    any audio device.
//!  * `Mixer` — the thin cpal shell: a stream whose callback calls
//!    `render_into` and a handle the app drives (play/pause/seek/master).
//!
//! The video clock stays the master: the app chases this mixer to the
//! playhead and nudges it when drift exceeds ~60 ms. Honest limitation, on
//! purpose: speed-changed clips preview PITCHED (linear resample), while the
//! render preserves pitch through atempo — noted in the UI copy until the
//! preview grows a proper stretcher.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

// ── Sample cache ─────────────────────────────────────────────────────────

/// Decoded interleaved stereo f32 at 48 kHz.
pub struct Pcm {
    pub data: Vec<f32>,
}

impl Pcm {
    pub fn frames(&self) -> usize {
        self.data.len() / CHANNELS
    }
}

fn decode(source: &str) -> Option<Pcm> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-v", "error", "-i", source, "-vn",
            "-ac", "2", "-ar", &RATE.to_string(),
            "-f", "f32le", "-",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .ok()?;
    let mut out = Vec::new();
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    let _ = child.wait();
    if out.len() < 4 {
        return None;
    }
    let mut data = Vec::with_capacity(out.len() / 4);
    for chunk in out.chunks_exact(4) {
        data.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Some(Pcm { data })
}

/// Background per-source PCM cache, same shape as the waveform cache.
pub struct SampleCache {
    ready: HashMap<String, Arc<Pcm>>,
    barren: HashMap<String, ()>,
    pending: HashMap<String, ()>,
    tx: Sender<(String, Option<Pcm>)>,
    rx: Receiver<(String, Option<Pcm>)>,
}

impl Default for SampleCache {
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

impl SampleCache {
    pub fn get(&mut self, source: &str) -> Option<Arc<Pcm>> {
        while let Ok((src, pcm)) = self.rx.try_recv() {
            self.pending.remove(&src);
            match pcm {
                Some(p) => {
                    self.ready.insert(src, Arc::new(p));
                }
                None => {
                    self.barren.insert(src, ());
                }
            }
        }
        if let Some(p) = self.ready.get(source) {
            return Some(p.clone());
        }
        if self.barren.contains_key(source) || self.pending.contains_key(source) {
            return None;
        }
        self.pending.insert(source.to_string(), ());
        let (tx, src) = (self.tx.clone(), source.to_string());
        std::thread::spawn(move || {
            let pcm = decode(&src);
            let _ = tx.send((src, pcm));
        });
        None
    }

    pub fn is_busy(&self) -> bool {
        !self.pending.is_empty()
    }
}

// ── The plan: what sounds when ───────────────────────────────────────────

/// One sounding clip, fully resolved: where it sits, what it plays.
pub struct PlanClip {
    pub pcm: Arc<Pcm>,
    /// Timeline seconds this clip occupies.
    pub start: f64,
    pub duration: f64,
    /// Source second its audio begins at.
    pub in_point: f64,
    /// Linear gain (dB already applied).
    pub gain: f32,
    /// To-black fades, seconds (rendered as gain ramps here).
    pub fade_in: f64,
    pub fade_out: f64,
    /// Constant playback rate. Ramped clips pass their AVERAGE rate — the
    /// preview approximation; the render walks the true curve.
    pub speed: f64,
}

pub struct PlanMusic {
    pub pcm: Arc<Pcm>,
    pub start: f64,
    pub gain: f32,
    pub duck: bool,
    pub fade: f64,
    /// The cut's total length — the bed trims and fades against it.
    pub total: f64,
}

#[derive(Default)]
pub struct Plan {
    pub clips: Vec<PlanClip>,
    pub music: Option<PlanMusic>,
}

pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

impl PlanClip {
    /// This clip's stereo sample at timeline time `t`, fades applied.
    fn sample(&self, t: f64) -> (f32, f32) {
        let local = t - self.start;
        if local < 0.0 || local >= self.duration {
            return (0.0, 0.0);
        }
        let src = self.in_point + local * self.speed;
        let idx = (src * RATE as f64) as usize;
        let n = self.pcm.frames();
        if idx >= n {
            return (0.0, 0.0);
        }
        let mut g = self.gain;
        if self.fade_in > 0.0 && local < self.fade_in {
            g *= (local / self.fade_in) as f32;
        }
        let remain = self.duration - local;
        if self.fade_out > 0.0 && remain < self.fade_out {
            g *= (remain / self.fade_out) as f32;
        }
        (self.pcm.data[idx * 2] * g, self.pcm.data[idx * 2 + 1] * g)
    }
}

/// The whole mixer's mutable state, owned by the audio callback and driven
/// by the app through a mutex (locked briefly on both sides).
pub struct MixState {
    pub plan: Arc<Plan>,
    /// Timeline position of the NEXT sample the callback will render.
    pub pos: f64,
    pub playing: bool,
    /// Master volume, linear.
    pub master: f32,
    /// The ducker's smoothed gain-reduction state (1.0 = no reduction).
    duck_gain: f32,
}

impl Default for MixState {
    fn default() -> Self {
        Self {
            plan: Arc::new(Plan::default()),
            pos: 0.0,
            playing: false,
            master: 1.0,
            duck_gain: 1.0,
        }
    }
}

/// Fill `out` (interleaved stereo) from the plan, advancing `pos`.
///
/// The ducker mirrors the render's sidechain in spirit: the cut's own level,
/// smoothed with fast attack and slow release, pushes the music down. Not
/// the same DSP as `sidechaincompress`, but the same behaviour: music dives
/// when the cut speaks and swells back when it stops.
pub fn render_into(state: &mut MixState, out: &mut [f32]) {
    if !state.playing {
        out.fill(0.0);
        return;
    }
    let plan = state.plan.clone();
    let dt = 1.0 / RATE as f64;
    // Ducker time constants, per sample.
    let attack = 1.0 - ((-1.0 / (0.020 * RATE as f64)) as f32).exp();
    let release = 1.0 - ((-1.0 / (0.400 * RATE as f64)) as f32).exp();
    let mut t = state.pos;
    for frame in out.chunks_exact_mut(2) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for c in &plan.clips {
            let (cl, cr) = c.sample(t);
            l += cl;
            r += cr;
        }
        if let Some(m) = &plan.music {
            let local = t - m.start;
            if local >= 0.0 && local < m.total {
                let idx = (local * RATE as f64) as usize;
                if idx < m.pcm.frames() {
                    let mut g = m.gain;
                    if m.fade > 0.0 && m.total > m.fade * 2.0 {
                        if local < m.fade {
                            g *= (local / m.fade) as f32;
                        }
                        let remain = m.total - local;
                        if remain < m.fade {
                            g *= (remain / m.fade) as f32;
                        }
                    }
                    if m.duck {
                        // Level of the cut right now → target reduction.
                        let level = (l.abs() + r.abs()) * 0.5;
                        let target = if level > 0.03 { 0.25 } else { 1.0 };
                        let k = if target < state.duck_gain { attack } else { release };
                        state.duck_gain += (target - state.duck_gain) * k;
                        g *= state.duck_gain;
                    }
                    l += m.pcm.data[idx * 2] * g;
                    r += m.pcm.data[idx * 2 + 1] * g;
                }
            }
        }
        frame[0] = (l * state.master).clamp(-1.0, 1.0);
        frame[1] = (r * state.master).clamp(-1.0, 1.0);
        t += dt;
    }
    state.pos = t;
}

// ── The PipeWire shell ───────────────────────────────────────────────────
//
// Native PipeWire playback via the pipewire-rs dependency capture already
// uses. Deliberately NOT cpal: cpal's Linux backend is ALSA, and a PipeWire
// desktop without the pipewire-alsa shim (this machine, for one) has no
// working ALSA route — the mixer would be silently inert exactly where
// Reel lives. The stream objects are Rc-based and !Send, so everything
// PipeWire happens on one dedicated thread; the app talks only to the
// shared MixState.

#[cfg(target_os = "linux")]
pub struct Mixer {
    pub state: Arc<Mutex<MixState>>,
}

#[cfg(target_os = "linux")]
impl Mixer {
    pub fn open() -> Option<Self> {
        let state = Arc::new(Mutex::new(MixState::default()));
        let thread_state = state.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("reel-audio".into())
            .spawn(move || {
                if let Err(e) = pw_playback(thread_state, ready_tx.clone()) {
                    log::warn!("audio mixer unavailable: {e}");
                    let _ = ready_tx.send(false);
                }
            })
            .ok()?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(true) => {
                log::info!("audio mixer: live timeline mix via PipeWire");
                Some(Self { state })
            }
            _ => None,
        }
    }

    pub fn set_plan(&self, plan: Plan) {
        self.state.lock().unwrap().plan = Arc::new(plan);
    }

    pub fn set_playing(&self, playing: bool) {
        self.state.lock().unwrap().playing = playing;
    }

    pub fn set_master(&self, master: f32) {
        self.state.lock().unwrap().master = master.clamp(0.0, 2.0);
    }

    pub fn position(&self) -> f64 {
        self.state.lock().unwrap().pos
    }

    pub fn seek(&self, t: f64) {
        self.state.lock().unwrap().pos = t.max(0.0);
    }
}

/// The audio thread: a PipeWire output stream whose process callback pulls
/// samples out of `render_into`. Runs the main loop until the process dies.
#[cfg(target_os = "linux")]
fn pw_playback(
    state: Arc<Mutex<MixState>>,
    ready: Sender<bool>,
) -> anyhow::Result<()> {
    use anyhow::anyhow;
    use pipewire as pw;
    use pw::spa;
    use spa::pod::serialize::PodSerializer;

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let stream = pw::stream::StreamRc::new(
        core,
        "reel-timeline-mix",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Playback",
            *pw::keys::MEDIA_ROLE => "Production",
            *pw::keys::APP_NAME => "Reel",
        },
    )?;

    let cb_state = state.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .process(move |stream, _| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(data) = datas.first_mut() else { return };
            let stride = std::mem::size_of::<f32>() * CHANNELS;
            let Some(slice) = data.data() else { return };
            let frames = slice.len() / stride;
            if frames == 0 {
                return;
            }
            // Render straight into the mapped buffer as f32 pairs.
            let out = unsafe {
                std::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut f32, frames * CHANNELS)
            };
            {
                let mut st = cb_state.lock().unwrap();
                render_into(&mut st, out);
            }
            let chunk = data.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (frames * stride) as u32;
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(RATE);
    audio_info.set_channels(CHANNELS as u32);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &spa::pod::Value::Object(obj))
            .map_err(|e| anyhow!("pod serialize: {e:?}"))?
            .0
            .into_inner();
    let mut params =
        [spa::pod::Pod::from_bytes(&values).ok_or_else(|| anyhow!("bad audio format pod"))?];

    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    let _ = ready.send(true);
    mainloop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(seconds: f64, amplitude: f32) -> Arc<Pcm> {
        let n = (seconds * RATE as f64) as usize;
        let mut data = Vec::with_capacity(n * 2);
        for _ in 0..n {
            data.push(amplitude);
            data.push(amplitude);
        }
        Arc::new(Pcm { data })
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|v| v * v).sum::<f32>() / buf.len() as f32).sqrt()
    }

    fn pull(state: &mut MixState, at: f64, seconds: f64) -> Vec<f32> {
        state.pos = at;
        state.playing = true;
        let mut out = vec![0.0f32; (seconds * RATE as f64) as usize * 2];
        render_into(state, &mut out);
        out
    }

    /// The mixing rules, without any audio device: clips sound only inside
    /// their windows, gain applies, gaps are silent.
    #[test]
    fn clips_sound_in_their_windows_and_nowhere_else() {
        let plan = Plan {
            clips: vec![
                PlanClip {
                    pcm: tone(10.0, 0.5),
                    start: 1.0,
                    duration: 2.0,
                    in_point: 0.0,
                    gain: db_to_gain(-6.0),
                    fade_in: 0.0,
                    fade_out: 0.0,
                    speed: 1.0,
                },
            ],
            music: None,
        };
        let mut st = MixState { plan: Arc::new(plan), ..Default::default() };

        assert!(rms(&pull(&mut st, 0.2, 0.5)) < 1e-6, "before the clip: silence");
        let inside = rms(&pull(&mut st, 1.5, 0.5));
        let want = 0.5 * db_to_gain(-6.0);
        assert!((inside - want).abs() < 0.01, "inside: tone at -6 dB, got {inside}");
        assert!(rms(&pull(&mut st, 3.5, 0.5)) < 1e-6, "after the clip: silence");

        // Paused, the mixer is silent no matter where it points.
        st.playing = false;
        let mut out = vec![1.0f32; 96];
        render_into(&mut st, &mut out);
        assert!(out.iter().all(|v| *v == 0.0));
    }

    /// Fades ramp the clip's gain inside the clip.
    #[test]
    fn fades_ramp_at_the_edges() {
        let plan = Plan {
            clips: vec![PlanClip {
                pcm: tone(10.0, 0.8),
                start: 0.0,
                duration: 4.0,
                in_point: 0.0,
                gain: 1.0,
                fade_in: 1.0,
                fade_out: 1.0,
                speed: 1.0,
            }],
            music: None,
        };
        let mut st = MixState { plan: Arc::new(plan), ..Default::default() };
        let early = rms(&pull(&mut st, 0.05, 0.1));
        let mid = rms(&pull(&mut st, 2.0, 0.1));
        let late = rms(&pull(&mut st, 3.85, 0.1));
        assert!(early < mid * 0.3, "fade-in starts quiet ({early} vs {mid})");
        assert!(late < mid * 0.3, "fade-out ends quiet ({late} vs {mid})");
        assert!((mid - 0.8).abs() < 0.02);
    }

    /// The point of the ducker: music dives when the cut speaks, and comes
    /// back when it stops — same behaviour the render's sidechain shows.
    #[test]
    fn music_ducks_live_under_the_cut() {
        let plan = Plan {
            // Speech only in 2..4 s.
            clips: vec![PlanClip {
                pcm: tone(10.0, 0.6),
                start: 2.0,
                duration: 2.0,
                in_point: 0.0,
                gain: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
            }],
            music: Some(PlanMusic {
                pcm: tone(10.0, 0.4),
                start: 0.0,
                gain: 1.0,
                duck: true,
                fade: 0.0,
                total: 8.0,
            }),
        };
        let mut st = MixState { plan: Arc::new(plan), ..Default::default() };
        let solo = rms(&pull(&mut st, 0.5, 0.5)); // music alone
        // Run THROUGH the speech so the ducker settles, then measure its tail.
        st.pos = 2.0;
        st.playing = true;
        let mut warm = vec![0.0f32; (1.0 * RATE as f64) as usize * 2];
        render_into(&mut st, &mut warm);
        let under = rms(&pull(&mut st, 3.0, 0.5)); // speech + ducked music
        // And measure recovery well after the speech.
        st.pos = 4.0;
        let mut rec = vec![0.0f32; (2.0 * RATE as f64) as usize * 2];
        render_into(&mut st, &mut rec);
        let after = rms(&pull(&mut st, 6.5, 0.5));

        assert!((solo - 0.4).abs() < 0.02, "music alone plays at its level, got {solo}");
        // Under speech: 0.6 speech + ~0.1 ducked music ≈ 0.7; UNducked would be 1.0.
        assert!(under < 0.85, "music failed to duck under the cut (mix rms {under})");
        assert!((after - 0.4).abs() < 0.05, "music must recover after speech, got {after}");
    }
}
