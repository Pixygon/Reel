# Reel — working notes for Claude

**https://reel.pixygon.io** — download, setup, usage.

Native cross-platform media player + editor + capture tool (video, audio,
images — one door). The bar: **better than VLC to play, Premiere-class to
edit. Linux first.** README.md and ROADMAP.md are current and honest — read
them; keep them that way when you change reality.

## Commands

```bash
cargo build --release          # build
cargo test --release           # decode-pipeline + mpv-backend tests (need ffmpeg on PATH; libmpv test self-skips if absent)
cargo run --release -- <file>  # run; opens and auto-plays the file
RUST_LOG=info … reel <file>    # logs which playback backend engaged
REEL_BACKEND=ffmpeg …          # force the subprocess fallback
pearl ship                     # ship ritual: test → draft → ship → commit (builds lin+win → CDN)
# AFTER every pearl ship: update ~/repos/ReelSite/latest.json to the new
# version, commit+push, and trigger the Coolify deploy (app uuid
# j3agln7m9aqo8j88nqnou5j1). The CDN caches manifest.json for 30 days with no
# purge, so the site's latest.json is the ONLY fresh "latest" pointer —
# installs and the download buttons read it first.
```

## Architecture rules (the non-negotiables)

- `video::Player` is the seam. The UI (app.rs/ui.rs) may only touch its public
  surface (`info/playing/position/current/ended`, `open/toggle_play/seek/
  update/take_dirty/wants_redraw/backend_name`). Backends must never leak
  upward.
- Two backends in `src/video/`: `mpv.rs` (libmpv **dlopen'd at runtime, never
  linked** — keeps the Windows cross-build and mpv-less machines working;
  hand-rolled FFI, constants verified against /usr/include/mpv) and
  `decoder.rs` (ffmpeg subprocess, the universal fallback — don't break it).
- The roadmap's end state keeps the frame on the GPU end-to-end. Current mpv
  seam is the software render target (CPU RGBA upload) — the next milestone
  step replaces that with render-API GL/Vulkan interop; don't add new code
  that depends on frames being CPU-visible.
- Event loop is `ControlFlow::Wait` + `wants_redraw()` pacing: continuous
  redraws only while playing or briefly after open/seek. Don't introduce a
  busy loop.
- `app.open()` is the single entry for ALL media (video/audio/image/captures)
  — route new sources through it, never a side door. `media.rs` owns kind
  routing (incl. SVG → resvg raster); export codec lists are kind-filtered
  via `Codec::for_kind`.
- Capture: `portal.rs` (Linux-only, cfg-gated) is the built-in recorder —
  ashpd portal session + PipeWire stream (git pipewire-rs, `*Rc` API) →
  latest-frame slot → 30 fps CFR ticker → ffmpeg stdin. `capture.rs`
  dispatches: portal first, external tools as fallback; screenshots via
  spectacle/grim/maim tiers with the portal's interactive dialog as the
  tool-free fallback. Portal start blocks on the system picker — always call
  from a worker thread. The Linux-only deps (ashpd/pipewire/tokio) MUST stay
  under `[target.'cfg(target_os = "linux")'.dependencies]` or the Windows
  cross-build breaks.
- Audio visualizers are mpv `lavfi-complex` graphs (`Visualizer` in
  player.rs); mpv renders them as the video track — no separate DSP/draw
  path. The viewport sizes off `app.tex_dims()`, never `Player::info`.
- ALL portal/rfd/ashpd work MUST run on `runtime::rt()` (the process-wide
  tokio runtime). ashpd/zbus cache their D-Bus connection against the first
  reactor — a per-call runtime works once, then every later dialog/capture
  hangs forever (the "Open… only works once" bug).
- `integration.rs` owns desktop citizenship: self-installed .desktop + icon
  (idempotent, skipped when the pacman package provides them), xdg-mime
  defaults, ~/.config/reel/settings.json. The UI flow is a one-time banner +
  ⚙ → Default apps. Arch packaging extras live in `.pixygon.json` build.arch
  (depends/desktop/icon — pearl.mjs reads them).
- There is NO player/editor tab bar and NO top bar at all: the app defaults
  to Player; ✂ Edit (or E) enters the editor, ▶ Done leaves it. Assume users
  enter Reel by double-clicking a media file, never bare.
- Player chrome is a bottom OVERLAY (`chrome()` in ui.rs): window-wide seek
  bar, ☰ REEL menu left, transport centered, tools right. It fades after
  ~2.5 s of no input during playback (cursor hides too) and stays put while
  paused or in the editor (fixed bottom panel there). Never reintroduce a
  top bar or a permanently visible transport in the player.
- Screen capture lives in the SYSTEM TRAY (`tray.rs`, ksni over the shared
  runtime; menu clicks wake winit via EventLoopProxy<UserEvent>). The ☰ menu
  shows capture entries ONLY when no tray host exists (`app.tray_available`).
- Textures handed to egui MUST be premultiplied-alpha (egui's blend mode
  assumes it). ImageDoc stays straight-alpha for exports; `sync_frame`
  premultiplies the uploaded copy. Transparent stills get the checkerboard.
- Editor rules: `EditorState.playhead` is TIMELINE time; source↔timeline
  mapping goes through `Project::source_to_timeline` / clip in_points —
  never treat player.position as timeline time. Editor ops (split/delete/
  drag) snapshot via `editor.push_undo` FIRST. egui layout traps to respect:
  a painter-only canvas must `allocate_exact_size` its rect (else the
  resizable panel collapses), and chrome/columns must only ever see bounded
  Uis. Glyphs: egui's font lacks many arrows (⧏⧐↶↷ render as boxes) — test
  new icons visually under Xvfb before shipping.
- Cold-open speed rules (measured with the `timing!` macro — keep it):
  video/audio opens go through `app.opening` (worker thread; MpvPlayer is
  deliberately `unsafe impl Send`) so the window/UI never blocks on a
  demuxer; mpv starts with `hwdec=no` and upgrades via `enable_hwdec()`
  after 1 s of playback (hwdec probing costs ~500 ms before first pixel);
  wgpu uses Backends::PRIMARY (GL probing is slow and never chosen). Don't
  reintroduce synchronous opens or eager hwdec.
- Known upstream gap: winit 0.30 has no Wayland file-drop — DnD works on
  X11/Windows only. Don't advertise drops on Wayland (empty-state hint
  already branches on WAYLAND_DISPLAY).
- GPU textures are capped at `gpu.max_texture_dim` — anything bigger must be
  downscaled before upload (see `ImageDoc::clamp_to`; an 8560×1440 ultrawide
  screenshot is the regression case).

## Verifying changes

Unit tests cover both backends and the export engine against
`tests/fixture.mp4` (320×240, ~2 s) — the export test runs a real ffmpeg
encode and re-probes the output.
For a live check: `RUST_LOG=info timeout 6 ./target/release/reel
tests/fixture.mp4` — expect `playback backend: libmpv`, no panics.
