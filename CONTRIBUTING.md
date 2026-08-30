# Contributing to Reel

Thanks for wanting to make Reel better. The short version: **every claim
ships with a test that proves it against real output**, and **the preview
must never lie about the render**.

## Getting started

```bash
cargo build --release            # needs ffmpeg on PATH; libmpv optional
cargo test --release             # the whole suite — real ffmpeg encodes, real pixels
cargo run --release -- video.mp4 # opens and plays
```

Linux needs `pipewire` headers for the live mixer/recorder
(`libpipewire-0.3-dev` on Debian/Ubuntu). Without libmpv, playback falls
back to the ffmpeg subprocess pipeline automatically.

## The rules that keep Reel honest

- **`CLAUDE.md` is the architecture document.** It is current, blunt, and
  lists every load-bearing invariant (uniform field order, concat label
  order, the one-truth-of-time map, …). Read it before touching anything;
  update it when you change reality.
- **The preview never lies.** Any effect exists exactly once as a formula
  and is mirrored into every consumer (preview shader, compositor shader,
  ffmpeg filters), with a parity test driving real renders. If you change
  one mirror, change them all — the tests will catch you if you don't.
- **Tests measure, they don't vibe.** A feature that claims to duck music
  is tested by band-passing the music's own frequency in a rendered file.
  Follow that pattern: render, probe, assert numbers.
- **CLI verbs live in ONE table** (`src/cli.rs` `COMMANDS`) that drives the
  parser, the help, and `reel commands --json`. `docs/CLI.md` is checked
  against it by a test — add a verb, regenerate the docs.
- **Visual changes get looked at.** Run under Xvfb and read the screenshot
  (`Xvfb :97 &`, `DISPLAY=:97 … reel file.mp4`, `import -window root shot.png`).
  The bundled font is missing many glyphs — never ship a symbol you
  haven't seen rendered.

## Sending changes

1. Open an issue first for anything non-trivial — the roadmap
   (`ROADMAP.md`) says where the project is headed.
2. Keep PRs focused; include the test that proves the change.
3. `cargo test --release` must be green; CI runs the same suite plus a
   Windows cross-compile check.

Reel is MIT-licensed; contributions are accepted under the same terms.
