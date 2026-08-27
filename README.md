# Reel

A cross-platform **video player _and_ editor**. Linux-first, native.

The bar is deliberately high: **better than VLC to play a file, at the level of
Premiere to edit one.** This repo is the running foundation that aims there —
not the finished tool, but real, honest v0.1 you can build and run today.

## What works in v0.1

- **Plays video — through libmpv when present.** Open a file (Open… dialog,
  drag-and-drop, or `reel <path>`) and it plays immediately, aspect-fit.
  With libmpv (the Milestone 1 hot path, auto-detected at runtime): hardware
  decode, correct colour conversion, **audio with real A/V sync**, subtitles,
  and **frame-exact seek + live scrubbing**. Without it, the v0.1
  ffmpeg-subprocess decoder still works everywhere (keyframe seek, video only).
- **A real player's controls.** Play/pause, frame step, jump ±5 s/±60 s,
  volume/mute, 0.25–4× speed, loop, fullscreen — all on mpv/VLC-style
  keyboard shortcuts (Space, ←/→, ,/., ↑/↓, M, L, F, [ ], E for editor).
- **Convert without editing — the HandBrake seam.** Hit **⬇ Export** in the
  player: H.264/H.265/AV1/VP9 with quality presets (or custom CRF),
  resolution downscale, audio bitrate/copy, or an instant lossless MKV remux.
  Live progress + cancel; runs on the system ffmpeg.
- **Editor timeline.** A real NLE data model (Project → Tracks → Clips), drawn
  as a multi-track timeline with a time ruler, clip blocks and a playhead.
  Opening a file drops it onto the V1 track. Trimming/drag/effects/export are
  the next steps (see ROADMAP).
- **Native Pixygon stack.** winit + wgpu + egui — the same stack as Infinite —
  themed in the Pixygon master-brand voice (void grounds, signal cyan, ember).
- **One binary, three platforms.** Builds to Linux + Windows via `pearl build`
  (macOS once the SDK is seeded), same as the rest of the estate's native apps.

## Build & run

```bash
cargo run --release -- /path/to/video.mp4     # open a file directly
cargo run --release                            # or open from the UI
cargo test                                     # decode-pipeline tests
```

Requires a GPU with Vulkan/Metal/DX12. **libmpv** (`mpv` package) is strongly
recommended — it is the real playback engine when found (dlopen'd at runtime,
never linked). A system **ffmpeg** on `PATH` is the universal fallback and is
what `cargo test`'s decode-pipeline tests exercise. `REEL_BACKEND=ffmpeg`
forces the fallback.

## Architecture

```
src/
├── main.rs         winit 0.30 app + run loop; clears the swapchain, egui draws over it
├── gpu.rs          wgpu context + VideoTexture (RGBA frame → GPU)
├── egui_backend.rs egui-wgpu integration (egui draws the whole UI incl. the video)
├── video/
│   ├── player.rs   the stable playback API (play/pause/seek/update) over two backends
│   ├── mpv.rs      libmpv backend (dlopen'd): hw decode, A/V sync, audio, exact seek
│   └── decoder.rs  fallback: ffmpeg subprocess → raw RGBA frames over a bounded channel
├── edit/           the NLE model — Project / Track / Clip (serde-serializable → a .reel doc)
├── export.rs       convert/export engine (ffmpeg encode, live progress, cancel)
├── ui.rs           player transport, editor timeline, export dialog, shortcuts (all egui)
└── theme.rs        Pixygon design tokens → egui visuals
```

### Why this stack

Every serious NLE (Resolve, Premiere, Final Cut) is native, not a web wrapper —
because beating VLC on playback needs a **zero-copy GPU video surface**, which a
web renderer can't give you. wgpu+egui gives us that surface and a fast UI in one
language, and slots straight into `pearl build`'s cross-platform pipeline.

Playback now runs on **libmpv** where available — hardware decode
(VA-API/D3D11VA, copy-back for now), mpv's colour conversion, audio out with
real A/V sync, subtitles, frame-exact seek — behind the same `Player` API the
v0.1 subprocess decoder sits under, which remains the universal fallback.
Current seam: mpv's software render target (mpv decodes and converts, Reel
uploads RGBA to wgpu). The [ROADMAP](ROADMAP.md)'s next step keeps the frame
on the GPU end-to-end (render API GL/Vulkan interop, libplacebo-class output).

## Status

Early foundation. It plays and scrubs video and shows the editor; it does not
yet trim, composite, or export. See [ROADMAP.md](ROADMAP.md) for the path to the
stated bar.
