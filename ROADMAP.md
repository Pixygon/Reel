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
- [ ] **Zero-copy GPU surface**: move off mpv's software render target onto the
      render API's GL/Vulkan path (libplacebo-class output: colour management,
      HDR tone-mapping, high-bit-depth, debanding). The frame must not touch
      system memory between decoder and screen.
- [x] Frame-accurate seek (`seek absolute+exact`) and A/V sync on mpv's own clock.
- [x] Audio out + subtitle rendering (via mpv). — [ ] track selection UI,
      audio passthrough.

## Milestone 2 — an editor you'd actually cut in

- [ ] Trim handles on clips; drag to move; snapping; ripple/roll.
- [ ] Multi-track compositing on the GPU (blend, opacity, transform).
- [ ] Playhead scrub renders the **composited timeline**, not just one source clip.
- [ ] Cut/copy/paste, undo/redo (command history over the serde model).
- [ ] Save/open `.reel` project documents (the model is already serde-ready).

## Milestone 3 — finish & polish

- [x] **Convert/export of the source file** (the HandBrake seam, straight from
      the player): H.264/H.265/AV1/VP9 + lossless remux, quality presets/CRF,
      downscale, audio modes, live progress + cancel. — [ ] **Timeline export**
      (render the composited edit) and a render queue; ProRes.
- [ ] Effects/transitions (GPU shaders), a basic colour panel, audio levels/mixer.
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
