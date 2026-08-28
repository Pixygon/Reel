//! The frame server: Reel renders every frame; ffmpeg encodes.
//!
//! This is the roadmap's Phase 1.3 — the removal of the filter-graph
//! ceiling. The compositor composes each output frame from decoded layers
//! (segments, crossfades, PiP overlays, effects, fades), reads it back, and
//! pipes raw RGBA into an ffmpeg process whose only jobs are subtitles
//! burn-in, audio mux and encode.
//!
//! Audio still travels the proven filter-graph (rendered to a WAV first) —
//! per the roadmap, the mixer replaces it when the audio engine lands. The
//! burn-in stage is byte-identical with the graph path: same `burnin_filters`.
//!
//! Scene *planning* is pure and unit-tested: `plan()` answers "which layers,
//! at what opacity, from which source times, at output time T" without any
//! GPU or decoder involved. The render loop just executes the plan.

use super::compositor::Compositor;
use super::sources::SegmentReader;
use crate::edit::{OverlaySegment, Segment};
use crate::export::{self, ExportJob, ExportSettings, Overlays};
use anyhow::{anyhow, Context, Result};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;

/// One segment placed on the output timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedSegment {
    pub seg: usize,
    /// Output-time window this segment occupies (overlapping its
    /// predecessor by the crossfade length).
    pub start: f64,
    pub end: f64,
    /// Crossfade seconds INTO this segment (0 = hard cut).
    pub fade_in: f64,
}

/// Lay segments end to end, overlapping each by its transition. The same
/// arithmetic as `edit::render_duration` — asserted against it in tests.
pub fn plan(segments: &[Segment]) -> Vec<PlannedSegment> {
    let mut out = Vec::with_capacity(segments.len());
    let mut cursor = 0.0f64;
    for (k, seg) in segments.iter().enumerate() {
        let d = if k == 0 {
            0.0
        } else {
            seg.transition_in.min(segments[k - 1].duration).min(seg.duration)
        };
        let start = (cursor - d).max(0.0);
        out.push(PlannedSegment { seg: k, start, end: start + seg.duration, fade_in: d });
        cursor = start + seg.duration;
    }
    out
}

/// A base layer's opacity at output time `t`: its own to-black fades times
/// its crossfade-in ramp. (The *outgoing* side of a crossfade keeps opacity
/// 1 — the incoming layer blends over it, which is exactly what xfade=fade
/// computes: `out = B*p + A*(1-p)`.)
pub fn base_opacity(p: &PlannedSegment, seg: &Segment, t: f64) -> f32 {
    let local = t - p.start;
    let mut a = 1.0f64;
    if p.fade_in > 0.0 && local < p.fade_in {
        a *= (local / p.fade_in).clamp(0.0, 1.0);
    }
    if seg.effects.fade_in > 0.0 && local < seg.effects.fade_in {
        a *= (local / seg.effects.fade_in).clamp(0.0, 1.0);
    }
    let remain = p.end - t;
    if seg.effects.fade_out > 0.0 && remain < seg.effects.fade_out {
        a *= (remain / seg.effects.fade_out).clamp(0.0, 1.0);
    }
    a as f32
}

/// Start a timeline render through the frame server. Fails fast (before any
/// thread spawns) when no GPU adapter exists, so the caller can fall back.
pub fn start_timeline(
    segments: &[Segment],
    output: &str,
    settings: &ExportSettings,
    project: (u32, u32, f64),
    overlays: &Overlays<'_>,
) -> Result<ExportJob> {
    if segments.is_empty() {
        return Err(anyhow!("the timeline is empty"));
    }
    if std::path::Path::new(output).exists() {
        return Err(anyhow!("output already exists: {output}"));
    }
    let comp = Compositor::headless()?; // no GPU → caller falls back to the graph
    log::info!("render path: frame server (Reel composites, ffmpeg encodes)");

    let target = export::render_target(project, settings);
    let total = crate::edit::render_duration(segments);
    let with_audio = segments.iter().all(|seg| export::has_audio_stream(&seg.source));
    let burnin = export::burnin_filters(output, overlays, target)?;

    // Overlay boxes need the source's aspect to size their decode; probe now,
    // on the calling thread, so failures surface before the job exists.
    let planned_overlays: Vec<(OverlaySegment, u32, u32)> = overlays
        .overlays
        .iter()
        .map(|o| {
            let info = crate::video::decoder::probe(&o.source)
                .with_context(|| format!("could not probe overlay source {}", o.source))?;
            let bw = (((o.pip.scale.clamp(0.02, 1.0) as f64) * target.0 as f64 / 2.0).round()
                * 2.0) as u32;
            let bh = (((bw as f64 * info.height as f64 / info.width.max(1) as f64) / 2.0).round()
                * 2.0)
                .max(2.0) as u32;
            Ok((o.clone(), bw.max(2), bh))
        })
        .collect::<Result<_>>()?;

    let audio_args = export::build_timeline_audio_wav_args(
        segments,
        with_audio,
        overlays.music,
        &format!("{output}.audio.wav"),
    );

    let (job, state, cancel) = ExportJob::manual(output);
    let segments = segments.to_vec();
    let settings = settings.clone();
    let output = output.to_string();

    std::thread::spawn(move || {
        let result = run(
            comp, &segments, &output, &settings, target, total, planned_overlays, audio_args,
            burnin, &state, &cancel,
        );
        let mut st = state.lock().unwrap();
        st.finished = true;
        st.error = result.err().map(|e| e.to_string());
        if st.error.is_none() {
            st.fraction = 1.0;
        }
    });
    Ok(job)
}

#[allow(clippy::too_many_arguments)]
fn run(
    comp: Compositor,
    segments: &[Segment],
    output: &str,
    settings: &ExportSettings,
    target: (u32, u32, f64),
    total: f64,
    planned_overlays: Vec<(OverlaySegment, u32, u32)>,
    audio_args: Option<Vec<String>>,
    burnin: Vec<String>,
    state: &std::sync::Arc<std::sync::Mutex<export::ExportState>>,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    let (tw, th, tfps) = target;
    let plan = plan(segments);
    let frames = ((total * tfps).round() as u64).max(1);

    // ── Audio first (the proven graph, to a WAV the encoder muxes) ──────
    let wav = format!("{output}.audio.wav");
    let mut have_audio = false;
    if let Some(args) = audio_args {
        let st = Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("audio pass failed to start")?;
        if !st.success() {
            return Err(anyhow!("the audio pass failed"));
        }
        have_audio = true;
    }
    if cancel.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(&wav);
        return Err(anyhow!("cancelled"));
    }

    // ── The encoder: raw frames in, finished file out ───────────────────
    let vcodec = if matches!(
        settings.codec,
        export::Codec::H265 | export::Codec::Av1 | export::Codec::Vp9
    ) {
        settings.codec
    } else {
        export::Codec::H264
    };
    let mut enc: Vec<String> = vec![
        "-y".into(),
        "-f".into(), "rawvideo".into(),
        "-pix_fmt".into(), "rgba".into(),
        "-s".into(), format!("{tw}x{th}"),
        "-r".into(), format!("{tfps:.4}"),
        "-i".into(), "-".into(),
    ];
    if have_audio {
        enc.extend(["-i".into(), wav.clone()]);
    }
    if !burnin.is_empty() {
        enc.extend(["-vf".into(), burnin.join(",")]);
    }
    enc.extend(["-map".into(), "0:v".into()]);
    if have_audio {
        enc.extend(["-map".into(), "1:a".into(), "-shortest".into()]);
    }
    enc.extend(export::video_encoder_args(vcodec, settings.quality, settings.hardware));
    if have_audio {
        let (codec, kbps) = if settings.codec == export::Codec::Vp9 {
            ("libopus", 128)
        } else {
            ("aac", 160)
        };
        enc.extend(["-c:a".into(), codec.into(), "-b:a".into(), format!("{kbps}k")]);
    }
    if settings.codec != export::Codec::Vp9 {
        enc.extend(["-movflags".into(), "+faststart".into()]);
    }
    enc.push(output.into());

    let mut encoder = Command::new("ffmpeg")
        .args(&enc)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("encoder failed to start")?;
    let mut enc_in = encoder.stdin.take().ok_or_else(|| anyhow!("encoder has no stdin"))?;
    let mut enc_err = encoder.stderr.take();

    // ── The frame loop ──────────────────────────────────────────────────
    struct Active {
        reader: SegmentReader,
        tex: wgpu::Texture,
        view: wgpu::TextureView,
        buf: Vec<u8>,
    }
    let mk_tex = |w: u32, h: u32| {
        let tex = comp.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("reel-fs-layer"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    };
    let write_tex = |tex: &wgpu::Texture, w: u32, h: u32, rgba: &[u8]| {
        comp.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    };

    let mut base_active: Vec<Option<Active>> = (0..segments.len()).map(|_| None).collect();
    let mut ov_active: Vec<Option<Active>> = (0..planned_overlays.len()).map(|_| None).collect();
    let out_tex = comp.target(tw, th);
    let started = std::time::Instant::now();

    let result = (|| -> Result<()> {
        for f in 0..frames {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow!("cancelled"));
            }
            let t = (f as f64 + 0.5) / tfps;
            let mut layers: Vec<super::Layer> = Vec::new();

            // Base segments (two during a crossfade, in timeline order).
            for p in &plan {
                if t < p.start || t >= p.end {
                    // Retire finished readers so their processes close.
                    if t >= p.end {
                        base_active[p.seg] = None;
                    }
                    continue;
                }
                let seg = &segments[p.seg];
                let slot = &mut base_active[p.seg];
                if slot.is_none() {
                    let fit = settings.fit.chain(tw, th, &p.seg.to_string());
                    let reader = SegmentReader::open(
                        &seg.source,
                        seg.in_point,
                        seg.duration,
                        seg.speed.clamp(0.05, 20.0) as f64,
                        &fit,
                        (tw, th, tfps),
                    )?;
                    let (tex, view) = mk_tex(tw, th);
                    *slot = Some(Active { reader, tex, view, buf: Vec::new() });
                }
                let a = slot.as_mut().unwrap();
                if !a.reader.next_into(&mut a.buf) {
                    return Err(anyhow!("no frames decoded from {}", seg.source));
                }
                write_tex(&a.tex, tw, th, &a.buf);
                // Keyframes: every animated parameter re-evaluated for THIS
                // frame — the whole point of frame-serving the render.
                let (fx, key_opacity) = seg.animated(t - p.start);
                layers.push(super::Layer {
                    view: a.view.clone(),
                    rect: [0.0, 0.0, 1.0, 1.0],
                    opacity: base_opacity(p, seg, t) * key_opacity,
                    effects: fx,
                    use_src_alpha: false,
                });
            }

            // PiP overlays, on top, in their own windows.
            for (i, (o, bw, bh)) in planned_overlays.iter().enumerate() {
                if t < o.at || t >= o.at + o.duration {
                    if t >= o.at + o.duration {
                        ov_active[i] = None;
                    }
                    continue;
                }
                let slot = &mut ov_active[i];
                if slot.is_none() {
                    let fit = super::sources::overlay_fit_chain(*bw, *bh);
                    let reader = SegmentReader::open(
                        &o.source,
                        o.in_point,
                        o.duration,
                        1.0,
                        &fit,
                        (*bw, *bh, tfps),
                    )?;
                    let (tex, view) = mk_tex(*bw, *bh);
                    *slot = Some(Active { reader, tex, view, buf: Vec::new() });
                }
                let a = slot.as_mut().unwrap();
                if !a.reader.next_into(&mut a.buf) {
                    return Err(anyhow!("no frames decoded from overlay {}", o.source));
                }
                write_tex(&a.tex, *bw, *bh, &a.buf);
                // Animated placement: position and scale re-evaluated per
                // frame. The decode box stays at the base scale; an animated
                // scale stretches the texture, which is visually fine for
                // the ranges a PiP moves through.
                let (pip, op) = o.animated(t - o.at);
                let scale_ratio = (pip.scale / o.pip.scale.max(0.02)).max(0.01);
                let (wf, hf) = (
                    *bw as f32 / tw as f32 * scale_ratio,
                    *bh as f32 / th as f32 * scale_ratio,
                );
                layers.push(super::Layer {
                    view: a.view.clone(),
                    rect: [
                        pip.x - wf / 2.0,
                        pip.y - hf / 2.0,
                        pip.x + wf / 2.0,
                        pip.y + hf / 2.0,
                    ],
                    opacity: op,
                    effects: Default::default(),
                    use_src_alpha: false,
                });
            }

            comp.render(&super::Scene { layers }, &out_tex);
            let rgba = comp.read_back(&out_tex);
            enc_in
                .write_all(&rgba)
                .map_err(|_| anyhow!("the encoder stopped accepting frames"))?;

            if f % 8 == 0 {
                let mut st = state.lock().unwrap();
                st.fraction = f as f32 / frames as f32;
                st.speed = (t / started.elapsed().as_secs_f64().max(0.001)) as f32;
            }
        }
        Ok(())
    })();

    drop(enc_in);
    let status = encoder.wait().context("encoder did not exit")?;
    let _ = std::fs::remove_file(&wav);
    result?;
    if !status.success() {
        let mut err = String::new();
        if let Some(e) = enc_err.as_mut() {
            let _ = e.read_to_string(&mut err);
        }
        return Err(anyhow!(
            "encode failed: {}",
            err.lines().last().unwrap_or("unknown encoder error")
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Effects;

    fn seg(dur: f64, fade: f64) -> Segment {
        Segment {
            source: "s".into(),
            in_point: 0.0,
            duration: dur,
            effects: Effects::default(),
            transition_in: fade,
            gain_db: 0.0,
            speed: 1.0,
            keys: Vec::new(),
        }
    }

    /// The plan must agree with `render_duration` — they encode the same
    /// overlap arithmetic, and playback already trusts render_duration.
    #[test]
    fn the_plan_agrees_with_render_duration() {
        let cases = vec![
            vec![seg(4.0, 0.0)],
            vec![seg(4.0, 0.0), seg(3.0, 0.0)],
            vec![seg(4.0, 0.0), seg(3.0, 1.0)],
            vec![seg(2.0, 0.0), seg(2.0, 0.5), seg(2.0, 1.5)],
            // A transition longer than either clip clamps.
            vec![seg(1.0, 0.0), seg(0.6, 5.0)],
        ];
        for segs in cases {
            let p = plan(&segs);
            let end = p.last().map(|s| s.end).unwrap_or(0.0);
            let want = crate::edit::render_duration(&segs);
            assert!(
                (end - want).abs() < 1e-9,
                "plan ends at {end}, render_duration says {want} for {segs:?}"
            );
            // Every segment occupies exactly its duration.
            for (pl, sg) in p.iter().zip(&segs) {
                assert!((pl.end - pl.start - sg.duration).abs() < 1e-9);
            }
        }
    }

    /// During a crossfade the incoming layer ramps 0→1 while the outgoing
    /// stays at 1 — the compositor's `over` then equals xfade's mix.
    #[test]
    fn crossfade_opacity_ramps_the_incoming_layer_only() {
        let segs = vec![seg(4.0, 0.0), seg(3.0, 1.0)];
        let p = plan(&segs);
        // Overlap runs 3.0..4.0 in output time.
        assert!((p[1].start - 3.0).abs() < 1e-9);
        let a = base_opacity(&p[0], &segs[0], 3.5);
        let b = base_opacity(&p[1], &segs[1], 3.5);
        assert!((a - 1.0).abs() < 1e-6, "outgoing layer must stay opaque, got {a}");
        assert!((b - 0.5).abs() < 1e-6, "incoming layer at mid-fade should be 0.5, got {b}");
        assert!(base_opacity(&p[1], &segs[1], 3.0) < 0.01);
        assert!(base_opacity(&p[1], &segs[1], 3.999) > 0.98);
    }

    /// To-black fades multiply in: a clip fading out during nothing special
    /// dims, and the maths lands the midpoints.
    #[test]
    fn to_black_fades_land_their_midpoints() {
        let mut s0 = seg(4.0, 0.0);
        s0.effects.fade_out = 2.0;
        let segs = vec![s0];
        let p = plan(&segs);
        assert!((base_opacity(&p[0], &segs[0], 1.0) - 1.0).abs() < 1e-6);
        assert!((base_opacity(&p[0], &segs[0], 3.0) - 0.5).abs() < 1e-6);
        assert!(base_opacity(&p[0], &segs[0], 3.99) < 0.02);
    }
}
