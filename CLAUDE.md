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

- Waveforms (`waveform.rs`) decode to 8 kHz mono s16 through an ffmpeg pipe
  and reduce to `BUCKETS_PER_SEC` peaks, cached per SOURCE path — a split or
  duplicated clip reads a different window of the same array rather than
  decoding again. The pipe read must carry an odd trailing byte into the
  next read; dropping it shifts every later sample and turns the envelope
  into noise.
- The bundled egui font has a NARROW glyph set. ✦ ● ○ ← → ⧏ ⧐ ↶ ↷ all render
  as empty boxes; ✂ ⬇ ▶ ◀ ⏭ ⏺ ☰ ⚙ ♪ 🗑 • · − + ↔ are known good. Never add a
  symbol without seeing it under Xvfb first — a missing glyph looks like a
  rendering bug, not a font gap.
- Shortcut conflicts are real and easy to miss: `M` was already mute, and
  `.`/`,` are frame-step, so markers live on Ctrl+M and Ctrl+←/→ with the
  colliding keys explicitly excluding ctrl. `shortcuts()` returns early on
  `ctx.wants_keyboard_input()`, which is what keeps typing a title from
  splitting clips — keep that guard first.

- The CLI (`cli.rs`) is driven by ONE table, `COMMANDS`: it parses the
  arguments, prints the help, and emits `reel commands --json`. Add a command
  there and the manual updates itself — and `docs/CLI.md` is checked against
  it by a test, so adding a verb means regenerating the reference.
- `main` routes on argv[1]: a real FILE always wins over a verb name (so
  `render.mp4` opens), a verb runs headless and exits, and anything else
  errors with exit 2 — never falling through to a window, which used to hang
  forever on a headless box.
- CLI output discipline: results on stdout (JSON under `--json`), progress and
  logs on stderr, so `--json 2>/dev/null | jq` is always safe. Print through
  `say()`, which treats a closed pipe as a normal end instead of panicking.

- Overlay/PiP is `TrackKind::Overlay` — deliberately NOT "a second video
  track", so `export_segments` (which flattens the cut) can never splice an
  overlay into the main sequence. Geometry is `Clip.pip` fractions, rendered
  with ffmpeg `overlay` + `enable='between(...)'`; the preview draws the same
  fractions with a thumbnail, so position/size are exact and only the moving
  picture is missing.
- The caption/title filter chain attaches to whatever label the graph
  currently ends on (read from the `-map` argument), NOT a hardcoded
  `[vcat]` — overlays rename it to `[voN]`, and hardcoding silently dropped
  them.
- SPEED: `Clip.duration` is TIMELINE length; the source window is
  `duration * speed` (`Clip::source_len()`). Everything reading a window out
  of the source — trim, waveform, thumbnails, caption mapping, split — must
  use `source_len()`, or picture and sound drift apart on any sped-up clip.
  `atempo` only accepts 0.5–100, so slow motion chains it (`atempo_chain`).
- Thumbnails (`thumbs.rs`) bake ONE tiled sheet per source via a single
  ffmpeg call (`fps=…,scale,tile=12x10`) and draw sub-rectangles of that one
  texture — a timeline full of clips costs one texture per file. `Layout` is
  kept separate from the texture so the time→cell mapping is testable.

- THE ENGINE (`src/engine/`): the compositor renders `Scene`s (placed,
  effected, blended layers) into textures; the FRAME SERVER is the DEFAULT
  timeline renderer (Reel composites every frame, pipes raw RGBA to ffmpeg
  which only encodes; audio pre-rendered to WAV by the proven filter graph;
  captions/titles burned by libass at the encode stage via the SHARED
  `burnin_filters`). The compiled-graph path (`start_timeline_graph`)
  remains the no-GPU fallback — `REEL_RENDER=graph` forces it — and cannot
  animate keyframes (it warns).
- `compose.wgsl` mirrors `effects::apply_reference` exactly, like video.wgsl;
  uniform FIELD ORDER matters, and the compositor blends premultiplied in
  linear light (a half-opacity mix meets at linear 0.5 ≈ sRGB 188 — the
  test knows this).
- Scene PLANNING is pure and unit-tested (`engine::render::plan`), and must
  agree with `edit::render_duration` — one overlap arithmetic. Crossfade =
  outgoing layer stays opaque, INCOMING ramps 0→1 (premultiplied over ≡
  xfade's mix). wgpu readback rows are padded to 256 bytes — strip per row.
- KEYFRAMES: clip-local time on `Clip.keys`; `Clip::animated(t)` /
  `Segment::animated(t)` are the ONLY evaluation call sites — preview, frame
  server, PiP and CLI all go through them, which is what keeps animation
  honest. `eval_keys` clamps at both ends (no extrapolation).
- Live PiP preview: `app.overlay_previews` — muted secondary Players chasing
  the main clock (nudge only when drift > 0.3 s; chase, don't fight), keyed
  by CLIP id (two clips can share one source). Frames drawn by egui's
  pipeline need alpha FORCED on upload (mpv's padding byte again); the pool
  clears outside the editor. Crossfades preview through the same pool: the
  incoming clip draws full-frame via a second `VideoDraw` at the ramp's
  opacity.
- `video_pass` prepared state is a QUEUE, not a slot: egui's
  CallbackResources is TYPE-keyed, so two VideoDraws in one frame (picture +
  crossfade) overwrote each other's bind group and both painted the same
  layer. egui prepares in paint order; the FIFO pairs them back up.
- mpv `load_file` must only fail on END_FILE with reason=ERROR (4). The OLD
  file's EOF lands in the event queue right when the next clip loads — a
  clip playing to its exact end at a cut made every switch "fail" until the
  reason was checked (struct verified against /usr/include/mpv/client.h).
- SPEED RAMPS: `Param::Speed` keyframes remap time. `speed_integral` is
  piecewise ANALYTIC (mean of a linear ramp is (a+b)/2 — and of an eased
  one too; hold holds), and is the single contract: video walks it per
  frame (`NativeReader::frame_at`, never rewinds), audio chunks per
  keyframe interval at the interval's true average tempo, and
  source↔timeline mapping inverts it by bisection. `geq` writes LIMITED
  range luma — the ramp test encodes time in RGB so it round-trips.
- The LIVE MIXER (`audio.rs`, Linux): `render_into` is a pure function over
  an immutable Plan — every mixing rule (windows, gain, fades, live duck
  attack/release) is unit-tested with no device. Output is a NATIVE
  PipeWire stream on a dedicated thread (the Rc API is !Send — build
  everything on that thread); cpal was rejected because its ALSA backend
  can't reach a PipeWire server without the pipewire-alsa shim (this
  machine has none). In the editor the mixer speaks and the main player is
  MUTED (`user_muted` keeps the user's intent separate); leaving the
  editor unmutes. Video stays the clock; the mixer chases the playhead,
  nudging only past 80 ms drift. It opens LAZILY on first entering the
  editor — an audio stream at app start would tax the cold-open budget.
- HDR: `probe_transfer` + `hdr_tonemap_chain` (libplacebo) run in the
  frame-server readers BEFORE fit. Do NOT use the zscale+tonemap recipe:
  its final transfer encode silently no-ops on float RGB — verified
  byte-by-byte at the rawvideo pipe (206 via PNG, 158 via rawvideo, same
  chain). libplacebo maps at BT.2408's 203-nit reference: 100-nit PQ white
  ≈ 185, not 255.
- PROXIES (`proxy.rs`): 1440p+ sources get a background 720p copy; the
  PREVIEW opens `proxies.preview_path(src)` — seek_timeline, clip advance,
  and the overlay/transition pool ALL go through it, and comparisons must
  be against the preview path, never the original (or every seek bounces
  back to the heavy file). Export/waveforms/thumbnails/captions keep
  originals. Keyed by path+size+mtime.
- CHROMA KEY lives in Effects (`key_color: Option<[f32;3]>` + similarity/
  softness) and in BOTH shaders — video.wgsl uses params.z=softness,
  params.w=enable; compose.wgsl uses fx.w=enable, params.w=softness (the
  free slots differ!). Both Uniforms structs grew a 5th vec4 `key` — field
  order, as ever, is load-bearing. The PiP inset draws through VideoDraw
  now, so keys/colour preview in the inset; the graph fallback cannot key.
- Roll/slip/slide: model fns on Project with invariants tested (roll/slide
  keep totals; slip keeps position). UI: Ctrl+edge=roll (tail edge rolls
  the RIGHT neighbour's head), Alt+body=slip (drag right shows earlier
  material), Ctrl+Alt=slide. Incremental application: each drag frame
  applies the delta and advances `last` by what was ACTUALLY applied.
- `Project::tighten` takes a peaks SUPPLIER closure (waveform buckets are
  normalised to each source's own peak — thresholds are relative), collects
  every hole FIRST, then cuts from the END backwards so earlier positions
  stay valid. Multi-select: `editor.multi` + `selected` as primary; group
  move applies the primary's delta to the rest.
- TRANSITIONS: `TransitionKind` on the clip; `engine::render::transition_mods`
  is the ONE geometry function (out/in opacities + incoming rect + uv) —
  wipes crop rect AND uv together (reveal, never squash); slides move a
  full-frame rect. The frame server executes it, the preview draws the same
  numbers (clip-rects/offsets), the graph fallback maps to xfade names. The
  compositor Layer grew a `uv` window (6th uniform vec4 `uvr`).
- Loudness: `ExportSettings.loudness` → loudnorm appended LAST in the wav
  pass (measures the finished mix); presets carry Some(-14.0).
- `waveform::best_lag` = zero-mean NCC over envelopes (min 40-bucket
  overlap, or slivers win by luck) — the sync engine behind `reel align`.
- LUTs: `Effects.lut` is an INDEX into `Project.luts` (Effects must stay
  Copy). `lut.rs` parses/caches .cube, uploads Rgba16Float 3D textures
  (32F isn't filterable), and `apply_reference` is the CPU lattice walk
  the GPU is tested against. Both pipelines bind the LUT at binding 3
  (identity when none); enable flag rides reframe.w. Applied on ENCODED
  values BEFORE the trims. The letterbox pad is TRANSPARENT (black@0 +
  format=rgba before the pad, use_src_alpha on base layers) so grades
  never colour the bars.
- Stabilisation: `Clip.stabilize` → vidstab two-pass; detect results cached
  in ~/.cache/reel/stab keyed by file+window; the transform splices BEFORE
  tone/fit. Preview stays raw (a full decode per analysis) — the UI says
  so. The test measures shake energy (tblend=difference + signalstats).
- MASKS (power windows): `Effects.mask` (shape/cx/cy/half-w/half-h/feather/
  invert, all fractions) → uniform vec4 pair `mask`/`mask2` in BOTH
  pipelines; the shader mixes graded↔ungraded by `mask_factor`, so the
  window gates LUT + trims but not keying. Geometry is keyframable
  (Param::MaskX/Y/W/H mutate the mask only when one exists).
- AUDIO-TRACK CLIPS RENDER: `Project::audio_clips()` (A1 + overlay audio)
  flows through Overlays into the wav pass — trimmed, tempo'd, faded,
  adelay'd to its TIMELINE position, amixed with normalize=0. The live
  mixer had made these audible in preview while the export dropped them;
  the beep-at-2s band test pins the fix.
- MIXER ROUTING: Track.gain_db/solo — `audio_clips()` composes track+clip
  dB and applies mute/solo; `video_audio_state()` gives V1's (gain,
  silenced) which export_segments folds into segment gains (-120 dB floor
  when soloed out). The LIVE plan applies the same rules — routing must
  never diverge between preview and render.
- CURVES: five points at FIXED inputs (0,¼,½,¾,1), Catmull-Rom with
  MIRRORED phantom ends (2·p0−p1) — clamping them sags identity into a
  curve, which the test catches. `bake_grade` composes LUT∘curves into one
  33³ lattice; `grade_key` (lut idx + curve bits) keys per-device texture
  caches in BOTH pipelines — never resolve a lattice by LUT index alone.
- ONE TRUTH OF TIME: `edit_spans`/`timeline_to_edit`/`edit_to_timeline` map
  timeline ↔ edit (render) time. The static map is TWO-SHEETED around
  transitions (sequential on the timeline, simultaneous in the edit) — the
  incoming clip owns the overlap on the inverse, and playback continuity
  comes from RESUMING past a transition's head (it already played under the
  fade). Scrubber + readout display edit time; playhead stays timeline
  internally. `app.user_rate` is the USER's dial; effective mpv rate =
  user_rate × clip rate — never let clip speed hijack the control again.
- Tests are capped at 8 threads (`.cargo/config.toml`): the suite runs real
  ffmpeg/GPU/mpv work, and at full parallelism the live-decode tests starve.
  `REEL_DEBUG_PLAY=1` autoplays once media lands — the Xvfb hook for
  watching the preview move without a keyboard.
- CLI media paths are canonicalized at `add`/`music set` — a project must
  find its media when opened from any directory.

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
