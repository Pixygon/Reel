# Reel — the road to best

The bar, stated plainly: **the editor that replaces Premiere. The audio
editor that makes a DAW unnecessary for video work. Image editing that
rivals Photoshop for what creators actually do.** One door for all media,
Linux first, local-first forever.

This document is ordered by **dependency, not by date**. There are no time
estimates anywhere in it, deliberately: the order is what matters, because
almost everything in phases 3–5 is cheap once the engine work in phases 1–2
exists, and almost everything is impossible-or-hacky without it.

## How to read this

Reel today renders exports by *compiling the edit into an ffmpeg
filter-graph string*. That approach carried us remarkably far — cuts,
crossfades, effects, captions, titles, PiP, speed, music ducking are all
live and pixel-tested — but it has a ceiling: every feature must be
expressible as a static ffmpeg filter chain, and the preview must then
*imitate* that chain and be tested against it. The parity tests exist
because preview and render are two implementations of one idea.

The single most important decision in this roadmap is to **remove that
ceiling**: make Reel's own GPU compositor the renderer, and demote ffmpeg
to decode and encode. Then the preview *is* the render — parity stops being
a test and becomes a property — and keyframes, ramps, masks, tracking,
layered images and live PiP all become one code path. Nearly everything
else sequences behind that.

## Principles that hold at every phase

These are not features; they are the constitution. A feature that violates
one of these is wrong even if it works.

- **Local-first, forever.** No accounts, no cloud calls, no telemetry. AI
  features (captions today; more below) fetch open models to
  `~/.cache/reel` and run on this machine. Working on a plane is a test
  case, not an edge case.
- **The preview never lies.** Whatever is on screen is what exports. Until
  the frame-server lands this is enforced by pixel-comparison tests; after
  it, by construction.
- **Never lose work.** Autosave is already the only save. That extends to:
  crash recovery, versioned project history, and no destructive operation
  without undo — in the UI *and* the CLI.
- **Measured, not guessed.** Every performance claim comes from `REEL_PERF`
  or the `timing!` macro. Budgets (below) are tested, not aspired to.
- **Everything the GUI can do, the CLI can do.** One `COMMANDS` table is
  parser + help + machine spec. A feature without a CLI verb is unfinished.
  Agents are first-class users.
- **Every promise has a test that exercises the real thing** — real ffmpeg,
  real whisper, real pixels measured in real renders, layout probed under
  Xvfb. A feature whose test mocks the interesting part is untested.
- **Honest docs.** README and this file describe reality. A checked box
  means shipped and verified.

### Performance budgets (standing, tested)

- Cold open: window < 400 ms, first frame < 1 s (today: ~300 ms / ~860 ms).
- Scrub latency (click → frame on screen): < 50 ms on mpv path.
- Timeline interaction at 60 fps with 1,000+ clips (today: 1,800 clips open
  in ~1 s; keep it).
- Idle = zero CPU: `ControlFlow::Wait` discipline, no busy loops, ever.
- Export ≥ realtime on hardware encoders for 1080p H.264 cuts.

---

## Phase 0 — Landed (the foundation this stands on)

Compressed inventory; details in git history and CLAUDE.md.

- [x] **Playback**: libmpv (dlopen'd, never linked) behind the `video::Player`
      seam; ffmpeg-subprocess universal fallback; hardware decode upgrade
      after first second; render-at-display-size (4K60: 15.1 → 4.0 ms/frame);
      Reel's own wgpu draw pass; J-K-L shuttle with true reverse; frame step;
      A-B loop; fullscreen; fading overlay chrome; audio visualizers.
- [x] **One door**: video, audio (cover art), images (SVG included,
      GPU-limit-aware), screenshots and built-in portal/PipeWire screen
      recording all open through `app.open()`.
- [x] **Editor**: multi-track timeline (V1/A1 + on-demand overlay track),
      trim/move/split/ripple/close-gaps, snapping, undo/redo, autosave
      (no Save button), markers, copy/paste/duplicate (paste = insert),
      in/out range, per-clip effects (exposure/contrast/saturation/fades/
      zoom-pan reframe) with preview=render parity tests, crossfades,
      per-clip gain + speed (0.25–4×, audio tempo matched), PiP overlay
      (drag-to-place, fraction-exact), waveforms and thumbnails on clips.
- [x] **Captions**: whisper.cpp subprocess, engine+model auto-fetched, cues
      mapped through the edit, styled burn-in, preview matches render
      (PlayResY=288 lesson), SRT export. Nothing uploaded, ever.
- [x] **Titles**: drag-placed, frame-fraction geometry, ASS-rendered,
      resolution-independent by test.
- [x] **Audio**: music bed with sidechain ducking (verified by band-level
      measurement in real renders), per-clip gain.
- [x] **Export**: H.264/265/AV1/VP9/remux + audio/image outputs, CRF tiers,
      social presets (fit modes incl. blurred-fill), trial-probed hardware
      encoders (NVENC/VideoToolbox), render queue, timeline export through
      one filter graph with normalization for mixed sources.
- [x] **CLI**: 22 headless verbs, one-table design, `--json` everywhere,
      `reel commands --json` machine spec, AGENTS.md + docs/CLI.md + /cli.
- [x] **FOSS**: public repo, MIT, no telemetry. Site, installer, Arch repo,
      CDN releases, latest.json flow.

Known debts carried forward: PiP previews as a still; transitions preview as
a cut; no Wayland file-drop (upstream); Sponsors not yet enabled; macOS
build does not exist yet.

---

## Phase 1 — The Engine: own the frame, own the sound

Everything after this phase composes on top of it. This is the deepest work
in the roadmap and the least visible — and it is what separates "a neat tool"
from "replaces Premiere".

### 1.1 The GPU compositor (the frame graph)

The current `video_pass` draws *one* textured quad with an effect uniform.
It becomes a real compositor:

- [x] The core (`engine/compositor.rs` + `compose.wgsl`): N layers → placed
      rect → per-layer effects (the `apply_reference` formula, mirrored) →
      opacity → premultiplied blend over black, rendered headless or on the
      app's device, with 256-byte-aware readback. Blend modes beyond normal
      (add/multiply/screen), rotation, and masks are still open.
- [x] Every animatable parameter addressable by (clip id, `Param`) — the
      addressing keyframes animate and `reel keyframe` reads/writes.
- [ ] Masks as first-class layer inputs: rectangle/ellipse/bezier, feather,
      invert. (Drawn masks; tracked masks come with tracking in Phase 3.)
- [ ] Scopes taps: the compositor can cheaply hand back histogram /
      waveform / vectorscope data for any node's output. UI in Phase 3;
      the tap belongs to the engine.
- [ ] 10-bit and float16 pipeline end-to-end: textures, blends, effects in
      linear light where correct (effects already know the sRGB dance);
      dither only at output.

### 1.2 Multi-stream decode

One mpv instance decodes one source. The editor needs frames from several
sources at once (PiP live, transitions previewing both sides, multicam):

- [x] The preview half: a pool of secondary decoders (`overlay_previews`,
      muted mpv instances chasing the timeline clock) — **PiP plays live in
      the preview**. The general `FrameSource` trait with LRU/prefetch is
      still the fuller form of this.
- [ ] Zero-copy where the platform allows (VA-API/dmabuf → Vulkan import;
      D3D11 shared textures on Windows), CPU upload as the universal
      fallback. The seam is the trait; the copies are an implementation
      detail to be eliminated per-platform.
- [ ] A frame cache keyed (source, time, size) so scrubbing back over
      recently shown footage is instant.
- [ ] Proxy generation: background-transcode heavy sources (H.265 4K, long
      GOP) to cheap editing proxies; automatic, invisible, and always
      re-linked to full-res at export. This is how a laptop edits 4K.

### 1.3 The frame-server renderer

- [x] Export = the compositor rendering the timeline frame-by-frame, piping
      raw frames into ffmpeg **only to encode**. Audio pre-renders through
      the proven filter graph to a WAV the encoder muxes (the mixer takes
      this over in 1.4). Captions/titles burn at the encode stage via the
      shared `burnin_filters`. **The frame server is the default renderer**;
      measured 1.47× realtime on 1080p30 H.264/NVENC.
- [x] The filter-graph compiler stays for `convert` and as the no-GPU
      fallback (`REEL_RENDER=graph` forces it; it warns when keyframes
      would be dropped) — retired from default timeline duty.
- [x] The parity flip: the whole pixel-measuring suite (cuts, stream order,
      captions, titles, overlays, speed, crossfades, ducking) runs through
      the frame server, plus an explicit both-paths-agree test.
- [x] Render is cancellable, progress-reported, queueable — same `ExportJob`
      plumbing.

### 1.4 The audio engine

mpv currently plays whichever single source is loaded; export audio is an
ffmpeg graph. Neither can express "the timeline, mixed, live":

- [x] The preview mixer (`audio.rs`): a pure, unit-tested mix core (clips
      with gain/fades/speed, the music bed, live ducking) pulled by a
      NATIVE PipeWire output stream — not cpal, whose ALSA backend is
      silently inert on shim-less PipeWire desktops. Editor audio now plays
      the whole timeline: every sounding clip, A1, the bed, ducked live.
      Still open toward the full 1.4: per-track strips/solo, insert
      effects, the mixer as the EXPORT sink (export audio remains the
      proven wav graph), and pitch-preserving preview of speed changes
      (previews pitched today; the render uses atempo).
- [x] Editor playback keeps video as the clock; the mixer chases the
      playhead and nudges only past 80 ms drift. mpv remains the
      player-mode engine, and speaks again the moment you leave the editor.
- [ ] Waveform/loudness taps for meters (UI in Phase 4; taps live here).
- [ ] Latency-compensated so inserted effects don't skew sync.

### 1.5 Color management

- [x] Sources probed for their transfer curve; PQ (HDR10) and HLG footage
      is tone-mapped to BT.709 at decode through libplacebo (BT.2408
      203-nit reference), before any scaling — pinned by a pixel test on
      tagged PQ fixtures. The classic zscale+tonemap chain was rejected
      after byte-level inspection showed its final transfer encode silently
      no-ops on float RGB. Skipped gracefully on ffmpeg builds without
      libplacebo.
- [ ] Pass-through HDR→HDR export for the codecs that carry it; primaries-
      aware working space beyond the decode-side mapping.
- [ ] Display: detect/assume sRGB display initially; wide-gamut display
      support later — the seam (working space → display transform) exists
      from day one.

**Exit criteria for Phase 1**: ~~PiP plays live in the preview~~ ✓.
~~Export runs through the frame server with the parity suite green~~ ✓.
~~A crossfade previews as a crossfade~~ ✓. ~~4K edits smoothly via
proxies~~ ✓. ~~Multitrack audio mixes live under the preview~~ ✓ (the
PipeWire mixer). Phase 1's exit criteria are met; the remaining line items
(zero-copy, blend modes, masks, scopes taps, 10-bit, HDR pass-through,
export-side mixer) continue as deepening work.

---

## Phase 2 — Time: keyframes, ramps, and animation

The engine gives us addressable parameters; this phase makes them move.

- [x] **Keyframes** on the effect stack, reframe and PiP geometry + opacity —
      one system (`Clip.keys`, `eval_keys`), linear/hold/ease, evaluated per
      frame by preview AND render through one call site, proven by a
      rendered-ramp pixel test. Still to extend: gain/pan and title
      parameters, and bezier handles beyond the ease curve.
- [x] Curve editor v1: the animated curve drawn live in the clip panel —
      drag keys in time and value, double-click to add, right-click to
      remove, playhead line synced to the preview — plus the Animate panel
      and painted diamonds on timeline clips. Still open: the full-width
      timeline lane variant and bezier handle editing.
- [x] **Speed ramps**: a `speed` keyframe track remaps clip time — the
      clip's slot stays fixed, the source consumed becomes the curve's
      integral (piecewise analytic, one function shared by picture, sound
      and the caption/waveform mappings). Video walks the integral exactly
      per frame through a native-rate reader; audio approximates per
      keyframe-interval at the interval's true average tempo, so it fills
      the slot to the sample. Proven by a render whose source encodes its
      own time in luminance. Nearest-frame for now — frame-blend and
      optical flow remain as quality tiers.
- [ ] **Animated titles**: position/opacity/size keyframes + a starter set
      of motion presets (fade, slide, pop, typewriter). Presets are just
      keyframe templates — no second system.
- [ ] Ken Burns for stills (a transform preset over a still's duration).
- [x] CLI: `reel keyframe` (set/list/remove, `--interp linear|hold|ease`,
      timeline-time addressing) — animations fully scriptable.
- [x] Tests: `a_keyframed_ramp_lands_its_midpoints_in_the_render` measures
      real output frames at three points of a ramp; eval unit tests pin
      clamping, hold and ease. (The speed-ramp sync test arrives with
      ramps.)

---

## Phase 3 — The full NLE (Premiere parity, then past it)

With engine + keyframes, these are features, not architecture. Ordered
within the phase by editor-workflow importance:

### 3.1 Edit mechanics
- [x] Roll, slip and slide edits — invariant-tested (roll and slide never
      change the total; slip never moves the clip), driven by modifier
      drags (Ctrl+edge / Alt+body / Ctrl+Alt+body) and `reel roll/slip/
      slide` in the CLI.
- [ ] Multi-select (click-drag lasso, shift-click), group move/delete,
      track targeting for paste/insert.
- [ ] Explicitly linked A/V clips (cut together, trim together, unlinkable).
- [ ] Unlimited tracks of every kind; track headers (name, lock, mute/solo,
      target); track resize.
- [ ] Compound clips (nest a sequence as a clip; open-in-place to edit).
- [ ] Adjustment layers: an effect stack over a time range, applied to
      everything below — trivially expressible in the frame graph.
- [ ] Insert/overwrite edit modes from the source side; three-point editing.

### 3.2 Media management
- [ ] Media pool: bins/folders, thumbnails, metadata (resolution, codec,
      fps, duration, date), search-as-you-type.
- [ ] Relink flow for moved/missing media (the .reel stores paths; offer
      per-folder relink); "offline" placeholder rendering, never a crash.
- [ ] Source monitor: preview any pool item with in/out before it touches
      the timeline.
- [ ] **Multicam**: sync by audio waveform correlation (we already decode
      peaks), cut between angles live with number keys.

### 3.3 The look
- [ ] Curves (master + per-channel), levels, HSL qualifiers, white balance,
      LUT loading (.cube) — all as frame-graph effect nodes, all keyframable
      by construction.
- [x] Scopes v1: live RGB histogram + luma waveform from the preview
      frame. Vectorscope and full RGB-parade waveform still open.
- [x] **Chroma key**: chroma-weighted distance + despill + soft edge, one
      shader block in BOTH pipelines (preview and frame server), with
      reach/soften controls, CLI flags, and a pixel test compositing a
      red-on-green overlay over a blue base. The PiP inset now draws
      through the video pass, so a keyed inset previews keyed.
- [ ] Masks on any effect (from 1.1) + **point tracking** to drive them
      (track a region, attach a mask/PiP/title to the track). Classic
      template matching first; optical flow upgrade later.
- [ ] Stabilization (vidstab two-pass or own path through the tracker).
- [ ] Blur/sharpen/glow/vignette/grain — the bread-and-butter stack.
- [ ] Transition library beyond crossfade: dip-to-color, wipe, slide, zoom,
      blur-through — each a two-input frame-graph node, previewed live.

### 3.4 Delivery
- [ ] Per-platform publish presets grow into a **publish panel**: filename
      templating, burn-in toggles (captions on/off per output), several
      outputs from one timeline in one pass.
- [x] Chapters from markers — an ffmetadata sidecar muxed by the frame
      server's encoder; ffprobe-verified. (YouTube chapter text export
      still open.)
- [ ] Watch-folder / hot-render mode via CLI (`reel render --watch` is the
      agent-era version of Adobe Media Encoder).
- [x] Frame export: the composed edit at the playhead — effects, overlays,
      animation — rendered to PNG through `render_still`, from the editor
      button or `reel frame` (which also grabs frames from plain media).

### 3.5 Captions, matured
- [ ] Word-level timestamps (whisper supports it) → karaoke/word-pop styles
      and exact text-based editing (Phase 4.4).
- [ ] Caption editor: click a cue to fix wording/timing; styles (font,
      color, background pill, position presets); per-cue overrides.
- [ ] Import/export SRT/VTT/ASS; translation-ready structure (cue text is
      data, styles are separate).

**Exit criteria for Phase 3**: an editor who cuts talking-head + b-roll +
music videos for a living can move from Premiere and lose nothing they
touch weekly — and gains local captions, honest performance, and a CLI.

---

## Phase 4 — Audio: the best audio editor a video editor ever shipped

The engine (1.4) makes these tractable; captions make some of them unique.

- [ ] **Mixer panel**: per-track strips (fader, pan, mute/solo, meters),
      master strip with true-peak + LUFS meters.
- [ ] Insert effects per track/clip: parametric EQ (with spectrum overlay),
      compressor, limiter, gate, de-esser. Visual, keyframable, and each
      one verified by measurement tests (band levels, gain reduction), the
      way the ducker already is.
- [ ] **Repair suite**, all local: broadband noise reduction (RNNoise or
      spectral gating), de-hum (notch at mains + harmonics), de-click,
      de-ess. One-button "Fix voice" chain with sensible defaults.
- [ ] **Loudness delivery**: EBU R128 / platform targets (−14 LUFS YouTube,
      etc.) — measure, then normalize the master automatically per preset.
      A render test asserts the delivered integrated loudness.
- [ ] Clip fade handles with curve choice (linear/equal-power/exp) directly
      on the timeline; crossfade by overlap, as video does.
- [ ] **Silence removal**: detect gaps, tighten to a rhythm (podcast jump-cut
      editing in one command; `reel tighten` in the CLI).
- [ ] **Filler-word removal**: captions know where every "um" is —
      one-click remove-with-ripple, review list before applying.
- [ ] Beat detection on the music bed → beat snap for cuts and markers.
- [ ] Time-stretch without pitch (rubberband-class) for music fitting;
      "fit music to edit length" as a command.
- [ ] Voice recording straight into a track (cpal input), with punch-in.
- [ ] Room tone generation (sample a quiet span, fill gaps with it).

**Exit criteria**: a podcast or voice-over video never needs Audacity/
Audition: record, repair, tighten, level, deliver at target loudness —
inside the cut, undoable, scriptable.

---

## Phase 5 — Images: rival Photoshop where creators live

Not a Photoshop clone — the 95% creators actually do, done natively in the
same engine. A still is a one-frame composition; every video effect that
makes sense for a still already works on one. That unification is the
feature.

- [ ] **Layer stack for stills**: the frame graph applied to a canvas —
      image layers, text layers (titles.rs), shape layers, adjustment
      layers; blend modes and opacity per layer; reorder, group. Saved in
      the project format; exports flatten.
- [ ] **Non-destructive adjustments**: the Phase-3 color stack (curves,
      levels, HSL, WB, LUTs) on any layer or the whole canvas.
- [ ] **Selections & masks**: rect/ellipse/freehand/polygon; feather/invert/
      grow; paint a mask with a brush; masks drive any adjustment.
- [ ] Crop/rotate/straighten/perspective; canvas resize vs image resize
      done right; high-quality resampling (Lanczos already in the chain).
- [ ] **Retouch**: clone stamp, spot heal (patch-match inpainting), red-eye.
      GPU-assisted where it matters.
- [ ] **Local-model magic, same pattern as captions** (fetched once, run
      locally, never uploaded):
      - Background removal / subject cut-out (U²-Net-class segmentation).
      - Smart select ("select subject") feeding the mask system.
      - 2×/4× upscale (Real-ESRGAN-class) as an export option for stills
        *and* a quality tier for video reframes.
- [ ] Brushes: basic round brush/eraser with pressure (tablet input via
      winit), color picker, swatches. Painting is in scope; a full digital-
      painting suite is not.
- [ ] Text on images = titles, verbatim: same fractions, same fonts, same
      styles. A lower-third designed on a video drops onto a thumbnail
      unchanged.
- [ ] **Thumbnail workflow** (the creator loop): grab frame from the edit →
      image canvas with subject cut-out → title + shapes → export PNG/WebP
      at platform size. Three clicks end to end, and a CLI recipe.
- [ ] Batch: every image operation available on N files via the CLI
      (`reel convert` grows `--script` or a small op pipeline).
- [ ] RAW ingest (via embedded dcraw-class decode) — view and basic-develop.

**Exit criteria**: banners, thumbnails, screenshots-annotated, product
shots, social crops — none of them require leaving Reel; each is scriptable.

---

## Phase 6 — Capture, everywhere, properly

- [ ] Live **capture preview** (see what you're recording), pause/resume,
      and a mic track alongside system audio, mixed by the Phase-1 audio
      engine.
- [ ] Webcam as a source (V4L2/PipeWire): record it, PiP it live over a
      screen recording (the streamer layout), or use it as a multicam angle.
- [ ] Replay buffer ("keep the last 60 s") — the clip-anything feature.
- [ ] Region capture with a real region-drag UI where the portal allows.
- [ ] **Windows capture backend** (Windows.Graphics.Capture + WASAPI
      loopback) — capture parity off Linux.
- [ ] Portal audio fully via PipeWire stream (drop the pulse-monitor
      bridge).
- [ ] Direct **virtual camera** output (v4l2loopback) — present any Reel
      composition as a camera. The seam to "Reel as a streaming tool"
      without becoming OBS.

---

## Phase 7 — Platforms, ecosystem, longevity

### 7.1 Platforms
- [ ] **macOS**: the build (VideoToolbox encode already contemplated in the
      probe design; ScreenCaptureKit for capture; notarized dmg;
      self-hosted runner exists in pearl).
- [ ] Windows polish: file associations (registry), installer,
      code-signing, winget/scoop manifests.
- [ ] Linux packaging beyond Arch: Flatpak (portals we already speak),
      AppImage, AUR official, Debian/Fedora repos.
- [ ] Wayland file-drop the moment winit lands it (tracked upstream).

### 7.2 The agent platform
- [ ] CLI parity as a standing rule (every phase above lands its verbs).
- [ ] `reel serve`: a long-lived JSON-RPC/stdio session — same verbs, no
      process-per-command, plus *subscriptions* (render progress, caption
      progress) for orchestrators.
- [ ] **MCP server mode**: the `commands` table exposed as MCP tools, so
      any agent runtime drives Reel natively. The one-table design means
      this is a projection, not a second implementation.
- [ ] Machine-readable project schema (JSON Schema for `.reel`) published
      and versioned; migration guaranteed forwards.
- [ ] A cookbook of agent recipes (docs/): "clip highlights from a stream
      VOD", "caption + tighten + publish shorts from a podcast", "thumbnail
      from frame 00:12".

### 7.3 Extensibility
- [ ] Effect plugins as **WGSL fragments** with a declared parameter block —
      loaded from `~/.config/reel/effects`, hot-reloaded, keyframable like
      built-ins, shareable as single files.
- [ ] Title/motion preset format (keyframe templates as data) with an
      in-app browser; community presets are just files.
- [ ] Raw ffmpeg filter escape hatch per clip for power users (clearly
      marked "expert; preview via frame-server still honest").
- [ ] LADSPA/LV2 hosting for audio inserts (the Linux-native audio plugin
      world, for free).

### 7.4 Being a good citizen of someone's work
- [ ] Project history: periodic snapshots + named versions; "restore from
      an hour ago" without ceremony. Crash recovery restores the exact
      editor state.
- [ ] Accessibility: complete keyboard operation, AccessKit wiring for
      screen readers, UI scale, reduced-motion mode.
- [ ] Localization scaffolding (the string table exists before the second
      language does).
- [ ] Theme system (the palette is already tokens; make it user-facing).

### 7.5 Sustainability
- [ ] GitHub Sponsors live (org toggle pending) + donation link on the
      site; funding goes to keeping the promise: local, free, no strings.
- [ ] CONTRIBUTING.md + labeled starter issues; CI running the full suite
      (including Xvfb visual checks) on PRs.
- [ ] A public benchmark page: cold-open, scrub latency, export speed
      against Premiere/Resolve/Shotcut on identical footage — measured,
      reproducible, updated per release. We win by measuring in public.

---

## The order, compressed

```
1  Engine        GPU compositor · decoder pool/proxies · frame-server render · audio mixer · color
2  Time          keyframes → curves → speed ramps → animated titles
3  NLE           roll/slip/slide → media pool → multicam → color tools/scopes/key/track → transitions → delivery → captions v2
4  Audio         mixer UI → EQ/dynamics → repair → loudness → silence/filler removal → beats → recording
5  Images        layer stack → selections/masks → retouch → local models (cutout/upscale) → thumbnail loop → batch
6  Capture       live preview/mic/webcam → replay buffer → Windows backend → virtual camera
7  Platform      macOS/Windows/Flatpak → serve/MCP → plugins → history/a11y/i18n → sponsors/CI/benchmarks
```

Phases 3–7 interleave freely once 1–2 exist; nothing in them blocks anything
else. Within each phase the list is the priority order. When reality
disagrees with this document, reality wins — and this document gets edited.
