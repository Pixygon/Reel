# Reel — working notes for Claude

Native cross-platform video player + editor. The bar: **better than VLC to
play, Premiere-class to edit. Linux first.** README.md and ROADMAP.md are
current and honest — read them; keep them that way when you change reality.

## Commands

```bash
cargo build --release          # build
cargo test --release           # decode-pipeline + mpv-backend tests (need ffmpeg on PATH; libmpv test self-skips if absent)
cargo run --release -- <file>  # run; opens and auto-plays the file
RUST_LOG=info … reel <file>    # logs which playback backend engaged
REEL_BACKEND=ffmpeg …          # force the subprocess fallback
pearl ship                     # ship ritual: test → draft → ship → commit (builds lin+win → CDN)
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

## Verifying changes

Unit tests cover both backends against `tests/fixture.mp4` (320×240, ~2 s).
For a live check: `RUST_LOG=info timeout 6 ./target/release/reel
tests/fixture.mp4` — expect `playback backend: libmpv`, no panics.
