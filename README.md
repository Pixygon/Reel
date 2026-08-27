# Reel

A cross-platform **video player _and_ editor**. Linux-first, native.

The bar is deliberately high: **better than VLC to play a file, at the level of
Premiere to edit one.** This repo is the running foundation that aims there —
not the finished tool, but real, honest v0.1 you can build and run today.

## What works in v0.1

- **Plays video.** Open a file and it decodes and displays, aspect-fit, with a
  transport bar: play/pause, a live position/duration readout, and a seek
  slider that scrubs (input-seek, jumps to the nearest keyframe).
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

Requires a system **ffmpeg** on `PATH` (used for decode in v0.1) and a GPU with
Vulkan/Metal/DX12.

## Architecture

```
src/
├── main.rs         winit 0.30 app + run loop; clears the swapchain, egui draws over it
├── gpu.rs          wgpu context + VideoTexture (RGBA frame → GPU)
├── egui_backend.rs egui-wgpu integration (egui draws the whole UI incl. the video)
├── video/
│   ├── decoder.rs  ffmpeg subprocess → raw RGBA frames over a bounded channel; ffprobe metadata
│   └── player.rs   play/pause/seek, wall-clock-paced frame pull
├── edit/           the NLE model — Project / Track / Clip (serde-serializable → a .reel doc)
├── ui.rs           player transport + editor timeline (all egui)
└── theme.rs        Pixygon design tokens → egui visuals
```

### Why this stack

Every serious NLE (Resolve, Premiere, Final Cut) is native, not a web wrapper —
because beating VLC on playback needs a **zero-copy GPU video surface**, which a
web renderer can't give you. wgpu+egui gives us that surface and a fast UI in one
language, and slots straight into `pearl build`'s cross-platform pipeline.

v0.1 decodes via the **system ffmpeg as a subprocess** (codec-complete and
rock-solid; ffmpeg 9 / libav 63 is too new for the Rust bindings anyway). The
[ROADMAP](ROADMAP.md) moves the hot path onto libmpv / libav + libplacebo for
the beat-VLC performance bar — behind the same `Player` API, so nothing above it
changes.

## Status

Early foundation. It plays and scrubs video and shows the editor; it does not
yet trim, composite, or export. See [ROADMAP.md](ROADMAP.md) for the path to the
stated bar.
