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
use std::sync::Arc;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
use std::sync::Mutex;

pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

// ── DSP: biquads, compressor, K-weighting ────────────────────────────────

/// One biquad section, transposed direct form II. Coefficients from the RBJ
/// cookbook; state persists across callbacks so the filters are continuous.
#[derive(Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn norm(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, z1: 0.0, z2: 0.0 }
    }

    /// From explicit coefficients (already normalised, a0 = 1).
    pub fn from_coeffs(b: [f32; 3], a: [f32; 2]) -> Self {
        Self { b0: b[0], b1: b[1], b2: b[2], a1: a[0], a2: a[1], z1: 0.0, z2: 0.0 }
    }

    pub fn low_shelf(f0: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w = 2.0 * std::f32::consts::PI * f0 / RATE as f32;
        let (sw, cw) = (w.sin(), w.cos());
        let alpha = sw / 2.0 * (2.0f32).sqrt(); // S = 1
        let sq = 2.0 * a.sqrt() * alpha;
        Self::norm(
            a * ((a + 1.0) - (a - 1.0) * cw + sq),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cw),
            a * ((a + 1.0) - (a - 1.0) * cw - sq),
            (a + 1.0) + (a - 1.0) * cw + sq,
            -2.0 * ((a - 1.0) + (a + 1.0) * cw),
            (a + 1.0) + (a - 1.0) * cw - sq,
        )
    }

    pub fn high_shelf(f0: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w = 2.0 * std::f32::consts::PI * f0 / RATE as f32;
        let (sw, cw) = (w.sin(), w.cos());
        let alpha = sw / 2.0 * (2.0f32).sqrt();
        let sq = 2.0 * a.sqrt() * alpha;
        Self::norm(
            a * ((a + 1.0) + (a - 1.0) * cw + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cw),
            a * ((a + 1.0) + (a - 1.0) * cw - sq),
            (a + 1.0) - (a - 1.0) * cw + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * cw),
            (a + 1.0) - (a - 1.0) * cw - sq,
        )
    }

    pub fn peaking(f0: f32, q: f32, gain_db: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w = 2.0 * std::f32::consts::PI * f0 / RATE as f32;
        let (sw, cw) = (w.sin(), w.cos());
        let alpha = sw / (2.0 * q);
        Self::norm(
            1.0 + alpha * a,
            -2.0 * cw,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cw,
            1.0 - alpha / a,
        )
    }
}

/// Per-clip runtime DSP state: the EQ chain (stereo pairs) and the
/// compressor's envelope. Rebuilt whenever the plan changes; a seek keeps
/// it (a few ms of filter transient beats a state machine).
#[derive(Default)]
pub struct ClipDsp {
    eq: Vec<[Biquad; 2]>,
    comp_env: f32,
}

impl ClipDsp {
    fn for_fx(fx: &crate::edit::AudioFx) -> Self {
        let mut eq = Vec::new();
        if fx.eq_low.abs() > 0.01 {
            let b = Biquad::low_shelf(120.0, fx.eq_low.clamp(-24.0, 24.0));
            eq.push([b, b]);
        }
        if fx.eq_mid.abs() > 0.01 {
            let b = Biquad::peaking(
                fx.eq_mid_freq.clamp(100.0, 12000.0),
                1.0,
                fx.eq_mid.clamp(-24.0, 24.0),
            );
            eq.push([b, b]);
        }
        if fx.eq_high.abs() > 0.01 {
            let b = Biquad::high_shelf(8000.0, fx.eq_high.clamp(-24.0, 24.0));
            eq.push([b, b]);
        }
        Self { eq, comp_env: 0.0 }
    }
}

/// Momentary loudness per BS.1770: K-weighting (shelf + RLB high-pass, the
/// standard 48 kHz coefficients) into a 400 ms mean-square window.
pub struct LufsMeter {
    stages: [[Biquad; 2]; 2],
    ring: Vec<f32>,
    idx: usize,
    sum: f64,
}

impl Default for LufsMeter {
    fn default() -> Self {
        let shelf = Biquad::from_coeffs(
            [1.535_124_9, -2.691_696_2, 1.198_392_8],
            [-1.690_659_3, 0.732_480_77],
        );
        let hp = Biquad::from_coeffs([1.0, -2.0, 1.0], [-1.990_047_5, 0.990_072_25]);
        let n = (RATE as usize * 2) / 5; // 400 ms
        Self { stages: [[shelf, shelf], [hp, hp]], ring: vec![0.0; n], idx: 0, sum: 0.0 }
    }
}

impl LufsMeter {
    #[inline]
    fn push(&mut self, l: f32, r: f32) {
        let sl = self.stages[0][0].process(l);
        let kl = self.stages[1][0].process(sl);
        let sr = self.stages[0][1].process(r);
        let kr = self.stages[1][1].process(sr);
        let e = kl * kl + kr * kr;
        self.sum += (e - self.ring[self.idx]) as f64;
        self.ring[self.idx] = e;
        self.idx = (self.idx + 1) % self.ring.len();
    }

    /// Momentary LUFS. -inf-ish (-90) when silent.
    pub fn momentary(&self) -> f32 {
        let ms = (self.sum / self.ring.len() as f64).max(1e-12);
        (-0.691 + 10.0 * ms.log10()) as f32
    }
}

/// What the strips read: decaying peaks per bus, master peaks, momentary
/// loudness. A snapshot type — cloned out under the lock.
#[derive(Clone, Default)]
pub struct Levels {
    /// Peak per mixer bus (track), linear, decaying.
    pub buses: Vec<f32>,
    pub master: [f32; 2],
    pub lufs: f32,
}

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
    /// Pan/EQ/compressor for this clip (repair is export-only).
    pub fx: crate::edit::AudioFx,
    /// Fade shape — matched to the export's afade curve.
    pub fade_curve: crate::edit::FadeCurve,
    /// Which meter bus (track index) this clip reports to.
    pub bus: usize,
}

pub struct PlanMusic {
    pub pcm: Arc<Pcm>,
    pub start: f64,
    pub gain: f32,
    pub duck: bool,
    pub fade: f64,
    /// The cut's total length — the bed trims and fades against it.
    pub total: f64,
    /// Playback rate for fit-to-edit (1.0 = natural). The live preview
    /// resamples linearly (slightly pitched); the render uses rubberband.
    pub rate: f64,
}

#[derive(Default)]
pub struct Plan {
    pub clips: Vec<PlanClip>,
    pub music: Option<PlanMusic>,
    /// Bus (track) count for the meters; music meters separately.
    pub buses: usize,
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
            g *= self.fade_curve.shape((local / self.fade_in) as f32);
        }
        let remain = self.duration - local;
        if self.fade_out > 0.0 && remain < self.fade_out {
            g *= self.fade_curve.shape((remain / self.fade_out) as f32);
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
    /// Per-plan-clip DSP state, index-aligned with plan.clips.
    dsp: Vec<ClipDsp>,
    /// Live levels for the meter strips.
    pub levels: Levels,
    lufs: LufsMeter,
}

impl Default for MixState {
    fn default() -> Self {
        Self {
            plan: Arc::new(Plan::default()),
            pos: 0.0,
            playing: false,
            master: 1.0,
            duck_gain: 1.0,
            dsp: Vec::new(),
            levels: Levels::default(),
            lufs: LufsMeter::default(),
        }
    }
}

impl MixState {
    /// Install a new plan and rebuild the aligned DSP state.
    pub fn install(&mut self, plan: Plan) {
        self.dsp = plan.clips.iter().map(|c| ClipDsp::for_fx(&c.fx)).collect();
        // Keep meter continuity across the routine plan rebuilds — zeroing
        // here made the bars flicker every 700 ms.
        if self.levels.buses.len() != plan.buses + 1 {
            self.levels.buses = vec![0.0; plan.buses + 1]; // +1: the music bus
        }
        self.plan = Arc::new(plan);
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
        // A stopped mixer shows silent meters — a frozen bar is a lie.
        state.levels.buses.iter_mut().for_each(|b| *b = 0.0);
        state.levels.master = [0.0, 0.0];
        state.levels.lufs = -90.0;
        return;
    }
    let plan = state.plan.clone();
    let dt = 1.0 / RATE as f64;
    // Ducker time constants, per sample.
    let attack = 1.0 - ((-1.0 / (0.020 * RATE as f64)) as f32).exp();
    let release = 1.0 - ((-1.0 / (0.400 * RATE as f64)) as f32).exp();
    // Compressor time constants (match the export's acompressor settings:
    // attack 20 ms, release 250 ms).
    let c_att = 1.0 - (-1.0f32 / (0.020 * RATE as f32)).exp();
    let c_rel = 1.0 - (-1.0f32 / (0.250 * RATE as f32)).exp();
    let meter_decay = 1.0 - (-1.0f32 / (0.300 * RATE as f32)).exp();
    if state.dsp.len() != plan.clips.len() {
        state.dsp = plan.clips.iter().map(|c| ClipDsp::for_fx(&c.fx)).collect();
    }
    if state.levels.buses.len() != plan.buses + 1 {
        state.levels.buses = vec![0.0; plan.buses + 1];
    }
    let mut t = state.pos;
    for frame in out.chunks_exact_mut(2) {
        let (mut l, mut r) = (0.0f32, 0.0f32);
        for (ci, c) in plan.clips.iter().enumerate() {
            let (mut cl, mut cr) = c.sample(t);
            if cl != 0.0 || cr != 0.0 || !state.dsp[ci].eq.is_empty() {
                let dsp = &mut state.dsp[ci];
                for band in &mut dsp.eq {
                    cl = band[0].process(cl);
                    cr = band[1].process(cr);
                }
                if c.fx.comp {
                    // Feed-forward peak compressor, mirroring the export's
                    // acompressor numbers in behaviour.
                    let peak = cl.abs().max(cr.abs());
                    let k = if peak > dsp.comp_env { c_att } else { c_rel };
                    dsp.comp_env += (peak - dsp.comp_env) * k;
                    let env_db = 20.0 * dsp.comp_env.max(1e-6).log10();
                    let thresh = c.fx.comp_thresh.clamp(-60.0, 0.0);
                    if env_db > thresh {
                        let over = env_db - thresh;
                        let reduce = over * (1.0 - 1.0 / c.fx.comp_ratio.clamp(1.0, 20.0));
                        let g = 10f32.powf(-reduce / 20.0);
                        cl *= g;
                        cr *= g;
                    }
                }
                let (pl, pr) = c.fx.pan_gains();
                cl *= pl;
                cr *= pr;
            }
            let peak = cl.abs().max(cr.abs());
            if let Some(b) = state.levels.buses.get_mut(c.bus) {
                *b = if peak > *b { peak } else { *b + (peak - *b) * meter_decay };
            }
            l += cl;
            r += cr;
        }
        if let Some(m) = &plan.music {
            let local = t - m.start;
            if local >= 0.0 && local < m.total {
                let idx = (local * m.rate * RATE as f64) as usize;
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
                    let (ml, mr) = (m.pcm.data[idx * 2] * g, m.pcm.data[idx * 2 + 1] * g);
                    let peak = ml.abs().max(mr.abs());
                    if let Some(b) = state.levels.buses.last_mut() {
                        *b = if peak > *b { peak } else { *b + (peak - *b) * meter_decay };
                    }
                    l += ml;
                    r += mr;
                }
            }
        }
        let (ol, or_) = ((l * state.master).clamp(-1.0, 1.0), (r * state.master).clamp(-1.0, 1.0));
        state.lufs.push(ol, or_);
        let m = &mut state.levels.master;
        m[0] = if ol.abs() > m[0] { ol.abs() } else { m[0] + (ol.abs() - m[0]) * meter_decay };
        m[1] = if or_.abs() > m[1] { or_.abs() } else { m[1] + (or_.abs() - m[1]) * meter_decay };
        frame[0] = ol;
        frame[1] = or_;
        t += dt;
    }
    state.levels.lufs = state.lufs.momentary();
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
        self.state.lock().unwrap().install(plan);
    }

    /// A snapshot of the live meters for the strips.
    pub fn levels(&self) -> Levels {
        self.state.lock().unwrap().levels.clone()
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

// ── The Windows shell ────────────────────────────────────────────────────
//
// WASAPI through cpal — the reason cpal was rejected on Linux (an ALSA-only
// backend on PipeWire desktops) doesn't apply here. Same architecture: the
// stream lives on its own thread; the app talks to shared MixState.

#[cfg(target_os = "windows")]
pub struct Mixer {
    pub state: Arc<Mutex<MixState>>,
}

#[cfg(target_os = "windows")]
impl Mixer {
    pub fn open() -> Option<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let state = Arc::new(Mutex::new(MixState::default()));
        let thread_state = state.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("reel-audio".into())
            .spawn(move || {
                let run = || -> anyhow::Result<cpal::Stream> {
                    let host = cpal::default_host();
                    let device = host
                        .default_output_device()
                        .ok_or_else(|| anyhow::anyhow!("no output device"))?;
                    let config = cpal::StreamConfig {
                        channels: CHANNELS as u16,
                        sample_rate: cpal::SampleRate(RATE),
                        buffer_size: cpal::BufferSize::Default,
                    };
                    let cb_state = thread_state.clone();
                    let stream = device.build_output_stream(
                        &config,
                        move |out: &mut [f32], _| {
                            let mut st = cb_state.lock().unwrap();
                            render_into(&mut st, out);
                        },
                        |e| log::warn!("audio mixer stream error: {e}"),
                        None,
                    )?;
                    stream.play()?;
                    Ok(stream)
                };
                match run() {
                    Ok(_stream) => {
                        let _ = ready_tx.send(true);
                        // The stream dies when dropped — park this thread
                        // for the app's lifetime to keep it alive.
                        loop {
                            std::thread::park();
                        }
                    }
                    Err(e) => {
                        log::warn!("audio mixer unavailable: {e}");
                        let _ = ready_tx.send(false);
                    }
                }
            })
            .ok()?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(true) => {
                log::info!("audio mixer: live timeline mix via WASAPI");
                Some(Self { state })
            }
            _ => None,
        }
    }

    pub fn set_plan(&self, plan: Plan) {
        self.state.lock().unwrap().install(plan);
    }

    pub fn levels(&self) -> Levels {
        self.state.lock().unwrap().levels.clone()
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

// ── Voice recording ──────────────────────────────────────────────────────
//
// A PipeWire capture stream on its own thread, same architecture as the
// mixer: Rc objects stay on the thread, the app talks to shared state. The
// recorder opens on the first punch-in and stays alive; `recording` gates
// whether arriving samples are kept.

/// Shared capture state.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Default)]
pub struct RecState {
    pub recording: bool,
    pub samples: Vec<f32>,
    /// Set when the stream is live and delivering.
    pub alive: bool,
}

#[cfg(target_os = "linux")]
pub struct Recorder {
    pub state: Arc<Mutex<RecState>>,
}

#[cfg(target_os = "linux")]
impl Recorder {
    pub fn open() -> Option<Self> {
        let state = Arc::new(Mutex::new(RecState::default()));
        let thread_state = state.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        std::thread::Builder::new()
            .name("reel-record".into())
            .spawn(move || {
                if let Err(e) = pw_capture(thread_state, ready_tx.clone()) {
                    log::warn!("voice recorder unavailable: {e}");
                    let _ = ready_tx.send(false);
                }
            })
            .ok()?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(3)) {
            Ok(true) => {
                log::info!("voice recorder: PipeWire capture ready");
                Some(Self { state })
            }
            _ => None,
        }
    }

    /// Arm: start keeping samples (cleared first).
    pub fn start(&self) {
        let mut st = self.state.lock().unwrap();
        st.samples.clear();
        st.recording = true;
    }

    /// Disarm and take what was recorded.
    pub fn stop(&self) -> Vec<f32> {
        let mut st = self.state.lock().unwrap();
        st.recording = false;
        std::mem::take(&mut st.samples)
    }

    pub fn seconds(&self) -> f64 {
        self.state.lock().unwrap().samples.len() as f64 / (RATE as usize * CHANNELS) as f64
    }
}

/// Write interleaved stereo f32 as a 16-bit PCM WAV — the recording's file
/// on disk. Hand-rolled 44-byte header; no dependency needed.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn write_wav(path: &std::path::Path, samples: &[f32]) -> std::io::Result<()> {
    use std::io::Write;
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = RATE * CHANNELS as u32 * 2;
    out.write_all(b"RIFF")?;
    out.write_all(&(36 + data_len).to_le_bytes())?;
    out.write_all(b"WAVEfmt ")?;
    out.write_all(&16u32.to_le_bytes())?;
    out.write_all(&1u16.to_le_bytes())?; // PCM
    out.write_all(&(CHANNELS as u16).to_le_bytes())?;
    out.write_all(&RATE.to_le_bytes())?;
    out.write_all(&byte_rate.to_le_bytes())?;
    out.write_all(&((CHANNELS * 2) as u16).to_le_bytes())?; // block align
    out.write_all(&16u16.to_le_bytes())?; // bits
    out.write_all(b"data")?;
    out.write_all(&data_len.to_le_bytes())?;
    for v in samples {
        out.write_all(&((v.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn pw_capture(state: Arc<Mutex<RecState>>, ready: Sender<bool>) -> anyhow::Result<()> {
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
        "reel-voice-record",
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Audio",
            *pw::keys::MEDIA_CATEGORY => "Capture",
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
            let n_bytes = data.chunk().size() as usize;
            let Some(slice) = data.data() else { return };
            let mut st = cb_state.lock().unwrap();
            st.alive = true;
            if !st.recording {
                return;
            }
            let floats = &slice[..n_bytes.min(slice.len())];
            for chunk in floats.chunks_exact(4) {
                st.samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
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
        spa::utils::Direction::Input,
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
                    fx: Default::default(),
                    fade_curve: Default::default(),
                    bus: 0,
                },
            ],
            music: None,
            buses: 1,
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

    /// Fade curves shape the ramp the way afade does: halfway through a
    /// fade-in, Smooth (qsin) sits above Linear, and Exp far below — the
    /// same ordering the export renders.
    #[test]
    fn fade_curves_shape_the_ramp() {
        use crate::edit::FadeCurve;
        let mid = |curve: FadeCurve| -> f32 {
            let mut plan = clip_with_fx(tone(10.0, 0.8), Default::default());
            plan.clips[0].fade_in = 2.0;
            plan.clips[0].fade_curve = curve;
            let mut st = MixState::default();
            st.install(plan);
            let (l, _) = channel_rms(&pull(&mut st, 1.0, 0.05));
            l
        };
        let (lin, smooth, exp) = (mid(FadeCurve::Linear), mid(FadeCurve::Smooth), mid(FadeCurve::Exp));
        assert!((lin - 0.4).abs() < 0.02, "linear midpoint = half level, got {lin}");
        assert!(smooth > lin + 0.1, "qsin rises faster: {smooth} vs {lin}");
        assert!(exp < lin * 0.2, "exp stays low till late: {exp} vs {lin}");
        // And they all match the formula the export's afade uses.
        assert!((FadeCurve::Smooth.shape(0.5) - (std::f32::consts::FRAC_PI_2 * 0.5).sin()).abs() < 1e-6);
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
                fx: Default::default(),
                fade_curve: Default::default(),
                bus: 0,
            }],
            music: None,
            buses: 1,
        };
        let mut st = MixState { plan: Arc::new(plan), ..Default::default() };
        let early = rms(&pull(&mut st, 0.05, 0.1));
        let mid = rms(&pull(&mut st, 2.0, 0.1));
        let late = rms(&pull(&mut st, 3.85, 0.1));
        assert!(early < mid * 0.3, "fade-in starts quiet ({early} vs {mid})");
        assert!(late < mid * 0.3, "fade-out ends quiet ({late} vs {mid})");
        assert!((mid - 0.8).abs() < 0.02);
    }

    fn sine(seconds: f64, freq: f64, amplitude: f32) -> Arc<Pcm> {
        let n = (seconds * RATE as f64) as usize;
        let mut data = Vec::with_capacity(n * 2);
        for i in 0..n {
            let v = amplitude * (2.0 * std::f64::consts::PI * freq * i as f64 / RATE as f64).sin() as f32;
            data.push(v);
            data.push(v);
        }
        Arc::new(Pcm { data })
    }

    fn clip_with_fx(pcm: Arc<Pcm>, fx: crate::edit::AudioFx) -> Plan {
        Plan {
            clips: vec![PlanClip {
                pcm,
                start: 0.0,
                duration: 8.0,
                in_point: 0.0,
                gain: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
                speed: 1.0,
                fx,
                fade_curve: Default::default(),
                bus: 0,
            }],
            music: None,
            buses: 1,
        }
    }

    fn channel_rms(buf: &[f32]) -> (f32, f32) {
        let (mut l, mut r) = (0.0f64, 0.0f64);
        for fr in buf.chunks_exact(2) {
            l += (fr[0] * fr[0]) as f64;
            r += (fr[1] * fr[1]) as f64;
        }
        let n = (buf.len() / 2) as f64;
        ((l / n).sqrt() as f32, (r / n).sqrt() as f32)
    }

    /// Pan is a balance law: full right silences the left channel and
    /// leaves the right untouched; centre changes nothing.
    #[test]
    fn pan_moves_the_sound_without_touching_the_centre() {
        let fx = crate::edit::AudioFx { pan: 1.0, ..Default::default() };
        let mut st = MixState::default();
        st.install(clip_with_fx(tone(10.0, 0.5), fx));
        let out = pull(&mut st, 1.0, 0.5);
        let (l, r) = channel_rms(&out);
        assert!(l < 1e-4, "full right: left silent, got {l}");
        assert!((r - 0.5).abs() < 0.01, "right unchanged, got {r}");

        let mut st = MixState::default();
        st.install(clip_with_fx(tone(10.0, 0.5), Default::default()));
        let (l, r) = channel_rms(&pull(&mut st, 1.0, 0.5));
        assert!((l - 0.5).abs() < 0.01 && (r - 0.5).abs() < 0.01, "centre is untouched");
    }

    /// The EQ actually equalises: a low shelf cut takes a 100 Hz tone down
    /// by roughly its dB and leaves a 5 kHz tone alone.
    #[test]
    fn the_low_shelf_cuts_lows_and_spares_highs() {
        let fx = crate::edit::AudioFx { eq_low: -12.0, ..Default::default() };
        // Deep below the 120 Hz corner the shelf shows its full depth.
        let mut st = MixState::default();
        st.install(clip_with_fx(sine(8.0, 40.0, 0.4), fx));
        let _ = pull(&mut st, 0.0, 1.0); // let the filter settle
        let (low, _) = channel_rms(&pull(&mut st, 1.0, 1.0));
        let expect = 0.4 / std::f32::consts::SQRT_2; // sine rms
        let cut_db = 20.0 * (low / expect).log10();
        assert!(
            (-14.0..=-9.5).contains(&cut_db),
            "40 Hz through a -12 dB shelf at 120 Hz should drop ~12 dB, got {cut_db:.1}"
        );

        let mut st = MixState::default();
        st.install(clip_with_fx(sine(8.0, 5000.0, 0.4), fx));
        let _ = pull(&mut st, 0.0, 1.0);
        let (high, _) = channel_rms(&pull(&mut st, 1.0, 1.0));
        let high_db = 20.0 * (high / expect).log10();
        assert!(high_db.abs() < 1.0, "5 kHz must pass the low shelf, moved {high_db:.1} dB");
    }

    /// The compressor squeezes above the threshold and idles below it.
    #[test]
    fn the_compressor_reduces_loud_and_ignores_quiet() {
        let fx = crate::edit::AudioFx {
            comp: true,
            comp_thresh: -18.0,
            comp_ratio: 4.0,
            ..Default::default()
        };
        // Loud: -4 dBFS peak → 14 dB over → ~10.5 dB reduction expected.
        let mut st = MixState::default();
        st.install(clip_with_fx(sine(8.0, 500.0, 0.63), fx));
        let _ = pull(&mut st, 0.0, 1.0);
        let (loud, _) = channel_rms(&pull(&mut st, 1.0, 1.0));
        let loud_db = 20.0 * (loud / (0.63 / std::f32::consts::SQRT_2)).log10();
        assert!(
            (-13.0..=-7.0).contains(&loud_db),
            "loud tone should compress ~10 dB, moved {loud_db:.1}"
        );
        // Quiet: -30 dBFS → untouched.
        let mut st = MixState::default();
        st.install(clip_with_fx(sine(8.0, 500.0, 0.0316), fx));
        let _ = pull(&mut st, 0.0, 1.0);
        let (quiet, _) = channel_rms(&pull(&mut st, 1.0, 1.0));
        let quiet_db = 20.0 * (quiet / (0.0316 / std::f32::consts::SQRT_2)).log10();
        assert!(quiet_db.abs() < 1.0, "below threshold nothing happens, moved {quiet_db:.1}");
    }

    /// The LUFS meter reads a known sine at its known loudness: a 997 Hz
    /// stereo tone at -12 dBFS is about -12.7 LUFS by BS.1770 arithmetic.
    #[test]
    fn the_lufs_meter_is_calibrated() {
        let mut st = MixState::default();
        st.install(clip_with_fx(sine(8.0, 997.0, 0.25), Default::default()));
        let _ = pull(&mut st, 0.0, 2.0);
        let lufs = st.levels.lufs;
        assert!(
            (lufs + 12.73).abs() < 1.5,
            "997 Hz @ -12 dBFS should read ≈ -12.7 LUFS, got {lufs:.1}"
        );
        // And the meters saw the signal.
        assert!(st.levels.buses[0] > 0.2, "bus meter moved");
        assert!(st.levels.master[0] > 0.2, "master meter moved");
    }

    /// The WAV writer produces a file ffmpeg agrees about: right rate,
    /// right channel count, right duration, right content level.
    #[test]
    fn the_wav_writer_writes_real_wavs() {
        let path = std::env::temp_dir().join(format!("reel-recwav-{}.wav", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // 1 s of 0.5-amplitude stereo.
        let samples: Vec<f32> = (0..RATE as usize * 2).map(|_| 0.5).collect();
        write_wav(&path, &samples).expect("write wav");
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=sample_rate,channels",
                   "-show_entries", "format=duration", "-of", "csv", &path.to_string_lossy()])
            .output()
            .expect("ffprobe");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("48000,2"), "rate/channels wrong: {text}");
        let dur: f64 = text
            .lines()
            .find_map(|l| l.strip_prefix("format,"))
            .and_then(|v| v.trim().parse().ok())
            .expect("duration");
        assert!((dur - 1.0).abs() < 0.01, "1 s written, {dur} read");
        let _ = std::fs::remove_file(&path);
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
                fx: Default::default(),
                fade_curve: Default::default(),
                bus: 0,
            }],
            music: Some(PlanMusic {
                pcm: tone(10.0, 0.4),
                start: 0.0,
                gain: 1.0,
                duck: true,
                fade: 0.0,
                total: 8.0,
                rate: 1.0,
            }),
            buses: 1,
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
