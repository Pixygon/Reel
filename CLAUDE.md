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
# AFTER every pearl ship: VERIFY the release actually landed before touching
# the site — `pearl ship` sometimes prints "✓ committed (no release)" and cuts
# nothing (re-running it then ships). Check:
#   curl -sI https://pixygontech.b-cdn.net/releases/reel/vX.Y.Z/reel-linux-x86_64.tar.gz
# THEN update ~/repos/ReelSite/latest.json, commit+push, trigger the Coolify
# deploy (app uuid j3agln7m9aqo8j88nqnou5j1). Pointing latest.json at a
# version that was never uploaded serves 404s to every download button.
# The CDN caches manifest.json for 30 days with no purge, so the site's
# latest.json is the ONLY fresh "latest" pointer.
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
- Editor layout order is load-bearing: the TIMELINE panel first (full width,
  `show_animated` so it slides with the mode), then the side panel (so it can
  never resize the timeline), then the CentralPanel — which holds the
  transport as an inner bottom panel, so the scrubber matches the PREVIEW's
  width, not the window's. The side panel is drawn every frame via
  `media_panel_frame(ctx, open, …)` so it animates between modes.
- The transport row uses EXPLICIT column widths, not `ui.columns(3, …)`:
  equal thirds overflowed and the right-hand tools drew on top of the
  transport buttons. The time readout hides itself when the row gets narrow.
- The editor's scrubber is the player for the WHOLE EDIT: it ranges over
  `edit::render_duration(export_segments())` and seeks with
  `app.seek_timeline`, never the loaded source's own position.
- Side-panel content must be width-agnostic (ScrollArea::both, sliders sized
  to available width). Content with a large minimum width silently clamps the
  panel and makes a resize drag spring back — regression-tested headlessly in
  `ui_tests.rs`. Headless egui tests must read layout INSIDE `ctx.run` —
  `available_rect()` outside a pass is a debug-only panic (release hides it).
- Projects SAVE THEMSELVES (`app::poll_autosave`): ~700 ms after the last
  change, serialised on the UI thread, written on a worker via
  `edit::write_atomic` (temp + rename). There is no Save button. Mark every
  mutation with `editor.mark_changed()`; merely opening/playing media must
  NOT mark dirty, or watching a video would litter .reel files.
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
- Mixed-source renders MUST keep the per-segment normalisation filters
  (scale+pad+setsar+fps, aformat) — ffmpeg's `concat` rejects mismatched
  geometry/rate/format, and mixing sources is normal. `render_target()` owns
  the output geometry (even dimensions; encoders demand them).
- Hardware encoders are probed with a real trial encode, not just an
  `-encoders` listing (listed ≠ usable). Only families that accept software
  frames (NVENC, VideoToolbox) — VAAPI/QSV would need hwupload in every
  graph. `REEL_NO_HWENC=1` forces software; tests pin `hardware: false`.
- Timeline export renders `Project::export_segments()` (V1 clips in order,
  gaps collapsed — same flattening editor playback uses) through ONE ffmpeg
  filter_complex trim+concat graph (`export::build_timeline_args`), sharing
  the job/progress/cancel plumbing with source exports via `spawn_job`.
  Audio legs are added only when every source has an audio stream. Keep the
  export dialog honest: controls that a mode ignores must not be shown.
- Editor rules: `EditorState.playhead` is TIMELINE time; source↔timeline
  mapping goes through `Project::source_to_timeline` / clip in_points —
  never treat player.position as timeline time. Editor ops (split/delete/
  drag) snapshot via `editor.push_undo` FIRST. egui layout traps to respect:
  a painter-only canvas must `allocate_exact_size` its rect (else the
  resizable panel collapses), and chrome/columns must only ever see bounded
  Uis. Glyphs: egui's font lacks many arrows (⧏⧐↶↷ render as boxes) — test
  new icons visually under Xvfb before shipping.
- Effects have ONE definition (`effects.rs::apply_reference`) mirrored in
  `video.wgsl` (preview) and `Effects::filters()` (ffmpeg render); the parity
  test drives real ffmpeg and compares pixels. Change one, change all three,
  and keep the test green — a preview that lies is worse than no preview.
  Effects apply on sRGB-encoded values, so the shader converts linear→sRGB
  around them (the frame texture is sRGB).
- Uniform structs: WGSL field ORDER and types must match `video_pass.rs`
  exactly. A mismatch is silent — fields read each other's bytes (this once
  made exposure read 0 and blacked the whole picture).
- The picture is drawn by Reel's own wgpu pipeline (`video_pass.rs` +
  `video.wgsl`), not `painter.image` — it forces alpha for video (mpv leaves
  a padding byte; there is deliberately NO CPU alpha pass any more) and keeps
  real alpha for stills. Uniforms are two vec4s on purpose: WGSL aligns vec3
  to 16 bytes, so an f32+vec3 tail is 48 bytes in the shader and 32 in Rust —
  the validation layer catches it, but don't reintroduce the trap.
- `Player::set_display_size` makes mpv render at on-screen size (never
  upscaling): the single biggest playback win measured (4K60: 15.1 → 4.0 ms
  per frame). Keep calling it from the viewport each frame.
- Perf work is measured, not guessed: `REEL_PERF=1` prints per-frame
  mpv-render / alpha / upload plus new-frames/s AND loop redraws/s. If both
  numbers match and are low (~20), that's the compositor throttling an
  occluded window — not Reel.
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

- Captions (`captions.rs`) run whisper.cpp as a subprocess. If the machine
  has no engine, Reel fetches the official prebuilt one into
  `~/.cache/reel/engine` (upstream sets `RUNPATH=$ORIGIN`, so the extracted
  folder is self-contained) and the ggml model into `~/.cache/reel/models`.
  Nothing is uploaded and nothing is installed system-wide — that is the
  whole point of the feature, so don't replace it with a cloud call or a
  hard package dependency.
- CAPTION GEOMETRY: libass renders an SRT through a script whose PlayResY is
  **288**, then scales it to the video height. Every number in
  `force_style` is therefore a fraction of the frame already — do NOT scale
  Fontsize or MarginV by the export resolution (that bug shipped a 4K
  caption ~3.7× too large and made the preview a lie). `captions::metrics()`
  is the single formula; the preview reads it directly and
  `the_burned_caption_matches_the_previewed_formula` renders at two
  resolutions and checks a real frame against it.
- Cues are generated in SOURCE time and mapped through
  `Project::map_source_window`, which clips them to each clip they land in —
  so a line spanning a cut appears in both halves, a duplicated clip gets
  captioned twice, and trimmed-away speech captions nowhere.
- Anything drawn ON the picture (captions, future overlays) must be painted
  after the `video_pass` paint callback and inside the same branch — that
  branch returns early for every real frame.

- Titles (`titles.rs`) generate an ASS document with an explicit
  `PlayResX/Y` set to the render frame, so `\pos()` is in real pixels and
  every stored value is a FRACTION of the frame. That is what lets a title
  placed by dragging on a 720p preview land identically in a 4K export —
  don't store pixels here.
- Concat label order is load-bearing: `concat=...:v=1:a=1[vcat][acat]` binds
  video to the FIRST label. Writing `[acat]` first (as this did for a while)
  silently swaps the streams — the file still plays, so nothing complains
  until a downstream filter treats `[acat]` as audio and ffmpeg rejects the
  whole graph. `renders_a_real_cut_from_the_fixture` now ffprobes the output
  and asserts stream 0 is video.
- The music bed mixes with `amix=...:normalize=0` — the default normalize
  divides every input by the input count, which quietly halves the dialogue.
  Ducking uses `sidechaincompress` keyed off the cut's own audio, which must
  be `asplit`'d first because a filter output can only be consumed once.
- A disabled ffmpeg filter is a PASS-THROUGH, not a mute: `volume=enable=...`
  leaves audio untouched outside the window. To gate audio, make the gain
  itself the expression (`volume=volume='between(t,a,b)':eval=frame`).

## Verifying changes

Unit tests cover both backends and the export engine against
`tests/fixture.mp4` (320×240, ~2 s) — the export test runs a real ffmpeg
encode and re-probes the output.
For a live check: `RUST_LOG=info timeout 6 ./target/release/reel
tests/fixture.mp4` — expect `playback backend: libmpv`, no panics.
`REEL_TEST_NET=1 cargo test captions::` additionally fetches the engine and
model and transcribes `tests/speech.wav`, asserting the actual words come
back — the only test that proves the caption promise end to end.
For anything visual, run under Xvfb and read the screenshot rather than
guessing: `Xvfb :97 -screen 0 1600x1000x24 &` then
`env -u WAYLAND_DISPLAY DISPLAY=:97 LIBGL_ALWAYS_SOFTWARE=1 ./target/release/reel <file>`
and `DISPLAY=:97 import -window root shot.png`. Kill test instances by PID —
never `pkill reel`, which also kills the copy the user is watching.
