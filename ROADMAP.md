# Reel — road to the bar

Two targets, stated plainly: **playback better than VLC**, **editing at the
level of Premiere**. Neither is a v0.1 claim; this is the sequence that gets
there. Linux is the reference platform; Windows/macOS follow each milestone.

Scope, widened deliberately: Reel is **one door for all media** — video,
audio and images play/display in the same window, convert through the same
export seam, and the screen itself is a source (screenshots + recording open
straight into Reel). No separate viewer, converter and capture tools.

## Media unification (landed)

- [x] Audio playback through the same `Player`/transport (mpv; cover art
      shown when embedded). Audio → MP3/M4A/Opus/FLAC/WAV conversion; audio
      extraction from video.
- [x] Image viewing through the same GPU path (instant open; GPU-limit-aware
      downscale for display). PNG/JPEG/WebP conversion with resize.
- [x] Screenshots (full/region/window) and screen recording, opening directly
      in Reel. Recording is **built in**: xdg-desktop-portal ScreenCast +
      PipeWire in-process → ffmpeg encode, with the system picker choosing
      screen/window/region, best-effort system audio, and a persisted restore
      token. External tools remain as opportunistic fallbacks.
- [x] Audio visualizers (spectrum bars/spectrogram/vectorscope/waveform)
      rendered through the playback engine's lavfi graph.
- [x] SVG rasterization (resvg) into the image path, exportable to PNG/JPEG/WebP.
- [x] ffmpeg auto-download on first launch when missing (mainly Windows).
- [x] Desktop citizenship: self-installed "Open with" entry, first-run
      default-apps prompt (Video/Music/Images checkboxes, ⚙ dialog),
      Arch package with mpv+ffmpeg deps + system desktop entry, installer
      that offers the codecs on any distro.
- [ ] Waveform on the timeline, image crop/rotate, mic/camera overlay for
      recordings, per-monitor shot pick, portal audio via PipeWire stream
      (instead of the pulse monitor bridge).
- [ ] Drag-and-drop on Wayland — blocked upstream (winit has no Wayland
      file-drop); works on X11/Windows today. Revisit when winit lands it.
- [ ] Windows file associations (registry) + installer polish.

## Where v0.1 stands

Plays and scrubs video (ffmpeg-subprocess decode → wgpu texture → egui), and
renders the NLE timeline from a real Project/Track/Clip model. Honest
foundation, narrow feature set.

## Milestone 1 — playback that earns "better than VLC"

The subprocess decoder is the v0.1 crutch; the performance bar needs the frame
never leaving the GPU.

- [x] Replace subprocess decode with **libmpv** (render API) as the hot path —
      behind the existing `video::Player` API, UI untouched. libmpv is dlopen'd
      at runtime; the subprocess decoder stays as the universal fallback
      (`REEL_BACKEND=ffmpeg` forces it).
- [x] **Hardware decode** (`hwdec=auto-copy-safe`: VA-API on Linux, D3D11VA
      elsewhere) — copy-back for now; the zero-CPU-copy step is below.
- [x] **Render at display size, not source size.** mpv renders (and we copy and
      upload) only the pixels that reach the screen. Measured on a 4K60 clip in
      a 1280px window: **15.1 ms → 4.0 ms per frame**, bus traffic 632 → 70 MB/s.
- [x] **Reel's own GPU video pass** (`video_pass.rs` + `video.wgsl`): the
      picture is drawn by our wgpu pipeline instead of egui's generic image
      draw. Alpha is forced in the shader (a full CPU pass over every pixel of
      every frame, deleted), stills keep real transparency, and this is the
      seam colour management and compositing plug into.
- [ ] **Zero-copy GPU surface** — the remaining step, and now a *smaller* win
      than it looked: the copy path costs ~0.6 ms/frame at display size; the
      remaining ~3 ms is mpv's software conversion. mpv's render API offers
      OpenGL (not Vulkan), so this needs either wgpu-on-GL (a downgrade for
      everything else) or dmabuf/external-memory import into our Vulkan
      device. Worth doing for HDR/high-bit-depth output, not for frame rate.
- [ ] libplacebo-class output: HDR tone-mapping, high-bit-depth, debanding
      (rides on the interop above, or on libplacebo directly).
- [x] Frame-accurate seek (`seek absolute+exact`) and A/V sync on mpv's own clock.
- [x] Audio out + subtitle rendering (via mpv). — [ ] track selection UI,
      audio passthrough.

## Milestone 2 — an editor you'd actually cut in

- [x] Trim handles on clips (edge-drag adjusts in-point/duration); drag to
      move with edge/playhead snapping; split at playhead (S); delete;
      selection; zoomable/pannable timeline with an adaptive ruler.
- [x] Undo/redo (whole-model snapshots over the serde model).
- [x] Save/open `.reel` project documents — double-clicking a .reel opens
      straight into the editor with the source loaded at the playhead.
- [x] Editor playback **sequences the cut**: the timeline playhead is
      timeline-time (not source-time), previews the frame under it while
      scrubbing, and playback jumps clip→clip across trims and gaps.
- [x] Multi-source timelines (opening a file while editing imports it).
- [x] Per-clip effects (exposure/contrast/saturation/fades) previewed on the
      GPU pass and rendered identically — see Milestone 3.
- [x] Close gaps (right-click a clip: close the gap before it, or every gap).
- [x] Autosave — projects save themselves after each edit; no Save button.
- [x] Ripple delete (Shift+Delete) and ripple trim to playhead (Q/W), linked
      across tracks; J-K-L shuttle with true reverse playback.
- [x] **Local auto-captions** — one button, transcribed on this machine, with
      the engine and model fetched automatically. Cues map through the edit
      (clipped per clip), preview exactly where they burn in, and burn into
      the render. The competitive point: CapCut's captions go to a cloud,
      and the open-source route "takes scripts and setup" — this takes
      neither.
- [x] **Titles** — text placed by dragging it on the preview, stored as
      frame fractions so the preview and any export resolution agree.
- [x] **Audio that behaves** — per-clip gain, a music bed, and automatic
      ducking under speech (sidechain, no curves to draw).
- [x] **Waveforms on clips** — decoded off-thread, cached per source.
- [x] Copy/paste/duplicate (paste inserts and ripples, never overwrites) and
      markers with jump-to-next/previous.
- [x] **Thumbnails on clips** — one ffmpeg call bakes a tiled contact sheet
      per source, so a whole timeline costs one texture per file.
- [x] **A second video track (overlay / PiP)** with drag-to-place composition,
      rendered through ffmpeg `overlay` from the same frame fractions the
      preview draws. The inset previews as a still, not live video.
- [x] **Per-clip speed** (0.25×–4×), audio tempo included.
- [ ] Speed RAMPS (accelerating through a shot) and keyframed effects.
- [ ] Roll edits; live compositing of the overlay in the preview.
- [x] A second video track composited at render time (see Milestone 2 above).
- [ ] Multi-track compositing on the GPU so the overlay plays live in the
      preview — today its position and size are exact but the inset is a still.

## Milestone 3 — finish & polish

- [x] **Convert/export of the source file** (the HandBrake seam, straight from
      the player): H.264/H.265/AV1/VP9 + lossless remux, quality presets/CRF,
      downscale, audio modes, live progress + cancel.
- [x] **Timeline export** — the cut renders to a new file (ffmpeg
      trim+concat filter graph over the V1 segments, audio in lockstep,
      optional downscale). The dialog offers "Source file" vs "✂ The edit"
      and defaults to the edit when you're in the editor.
- [x] **Hardware encoding** — NVENC / VideoToolbox, probed at runtime with a
      real trial encode, quality-targeted (cq mirrors the CRF ladder), with a
      dialog toggle and automatic software fallback (VP9, no GPU, or
      `REEL_NO_HWENC=1`). VAAPI/QSV need hwupload plumbing — later.
- [x] **In/out range export** — I/O set markers, Shift+I/O clears; the
      timeline shades outside the range and playback stops at the out point;
      the dialog offers "✂ Range" and renders exactly what's enclosed.
- [x] **Multi-source timelines** — opening a file while editing imports it
      onto the timeline; the preview switches source files as the playhead
      crosses clips (one mpv instance, `loadfile`); renders normalise every
      segment (fit+letterbox, square pixels, one fps, one audio format) so
      mixed resolutions/codecs/rates concatenate correctly.
- [x] **One-click social presets** — YouTube (1080p/4K), TikTok, Reels/Shorts,
      Instagram feed (4:5), Square, Facebook, X. Each carries the frame, the
      fit (letterbox / crop / blurred sides) and the codec, and names the
      output per platform so the TikTok and YouTube cuts sit side by side.
- [x] **Per-clip effects with preview/render parity** — exposure, contrast,
      saturation, fade in/out. One formula (`effects.rs`) drives BOTH the
      preview shader and the ffmpeg render, and a test drives real ffmpeg and
      compares its pixels against that formula, so the editor cannot lie.
- [x] **Render queue** — line up every platform (＋ Queue), jobs run one at a
      time with live progress, per-job results and cancel-all.
- [x] **Crossfade transitions** — per-clip "crossfade in", rendered with
      xfade/acrossfade (clips overlap, so the export gets shorter by exactly
      the fade); the timeline draws the overlap wedge and the export dialog
      reports the true rendered duration. The preview still shows the cut —
      compositing two clips live needs a second decoder (next step).
- [x] **Reframe (zoom/pan)** — put a landscape shot inside a vertical frame
      without blurred sides. Preview UV maths and the ffmpeg scale+crop share
      one geometry, checked by a test.
- [ ] ProRes; transition types beyond crossfade; preview compositing of
      transitions (second decoder).
- [ ] Audio levels/mixer.
- [x] Native file dialogs (rfd) and drag-and-drop open. — [ ] thumbnails/waveforms.
- [ ] Proxy workflow for heavy media; background conform.

## Milestone 4 — the Pixygon seam

- [ ] Publish exports straight to Bunny CDN / a pearl's media pipeline.
- [ ] Optional PixygonAPI sign-in; project sync.
- [ ] `pearl build` release cadence (Linux + Windows now, macOS once the SDK is seeded).

## Non-negotiables

- The frame stays on the GPU. Every architecture choice defers to that.
- One binary per platform, built here → CDN. No web runtime in the hot path.
- The `Player` API stays stable as the decode backend is swapped underneath it.
