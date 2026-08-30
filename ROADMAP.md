# Reel — the refinement roadmap (1.x)

**1.0 shipped 2026-08-29.** The player, the engine (GPU compositor +
frame-server renderer), the editor (trim family, keyframes, ramps,
transitions, multi-select, tighten), the grade (LUTs, curves, power
windows, chroma key, scopes, stabilization), the sound (live PipeWire
mixer, music with ducking, mixer strip, loudness delivery), captions and
titles (local, editable, resolution-exact), delivery (presets,
publish-everywhere, chapters, stills), a 31-verb CLI whose manual is its
parser, proxies, HDR tone-mapping, multicam sync — MIT, public, tested
against real output throughout. `docs/AUDIT-1.0.md` records the 1.0 audit;
this document is what comes next.

The constitution from the first roadmap still governs: local-first
forever; the preview never lies; never lose work; measured, not guessed;
CLI parity; tests exercise the real thing; honest docs. The budgets
stand (and were re-measured at 1.0: cold open 60 ms/745 ms; 1800 clips in
64 ms; kitchen-sink export at 1.7x realtime).

Ordering inside each theme is priority. Debts from the audit come first —
refinement starts by paying what we owe.

---

## A. One truth of time

The deepest 1.x item. The editor timeline keeps gaps and uncollapsed
transition overlaps; render time collapses both. Today the scrubber can
read `10.00 / 8.00` at the end of a transitioned edit.

- [x] The timebase module (`edit_spans` / `timeline_to_edit` /
      `edit_to_timeline`): totals equal `render_duration` to the bit, gaps
      collapse, the overlap belongs to the incoming clip, round trips
      tested. (The static map is honestly two-sheeted around transitions —
      a's tail and b's head are sequential on the timeline but simultaneous
      in the edit; what is monotone is the PLAYBACK PATH.)
- [x] The scrubber and the time readout speak edit time; seeks map back
      through the inverse. `10.00 / 8.00` is gone.
- [x] Playback resumes the incoming clip PAST a transition's head (it
      already played, blended over the outgoing tail) — edit time flows
      continuously through a fade, tested as path continuity.
- [x] The transport speed control shows the USER's rate again; the active
      clip's own rate multiplies underneath and shows as a badge.
- [ ] Markers/captions/titles authored against edit time survive
      re-timing edits upstream of them (anchor to clips where possible).

## B. Preview honesty debts — shipped in v1.2.0

- [x] `render_still` composes transitions (a mid-wipe frame export shows
      the travelling edge, pixel-tested) and `still_png` burns
      captions/titles through the same filters as the full render.
- [x] The graph fallback warns about EVERYTHING it drops (masks, LUTs,
      curves, chroma key, stabilization, chapters) in one message.
- [x] `captions --source` says plainly when zero cues mapped, and why.
- [x] Speed-changed preview audio keeps its pitch: mpv's
      `audio-pitch-correction` (scaletempo2) is now pinned on, matching
      the render's atempo.
- [x] A PROXY badge in the preview corner whenever the editor is playing
      the 720p editing copy (or one is still baking).
- [x] Stabilization audition: one click renders the clip's own window
      stabilized at 720p and plays it — E returns to the edit untouched.

## C. Performance refinements — shipped in v1.2.0

- [x] PiP pool players render at inset size (`set_display_size`), not
      source size.
- [x] Double-buffered readback in the frame server: the GPU→CPU copy of
      frame N−1 overlaps compositing frame N. Measured: the 30 s 1080p
      export went 20.2 s → 16.0 s (1.47× → 1.88× realtime), output
      byte-identical.
- [x] Grade-lattice preview cache is bounded (clears past 48 grades and
      rebuilds what's in use).
- [x] Waveforms and thumbnail sheets persist across sessions
      (`~/.cache/reel/waveforms`, `~/.cache/reel/thumbs`, keyed like
      proxies on path+size+mtime) — a reopened project is instantly
      dressed. Cache round-trip is unit-tested.
- [x] `reel bench MEDIA`: probe, first frame, scrub median, export speed
      measured on this machine — table or `--json`.

## D. Grading depth — shipped in v1.3.0 (one item open)

- [x] Levels (black/white/gamma) and white balance (temp/tint) — baked
      into the grade lattice (`Effects::grade_reference` → `bake_grade`),
      so the preview cost stays one texture sample and every pipeline
      agrees by construction. Preview verified pixel-exact against the
      formula under Xvfb.
- [x] HSL qualifiers: hue/sat/lightness window (soft edges) + push
      (hue shift, sat/light multipliers) — same lattice.
      BONUS: the graph fallback now applies the WHOLE grade via a baked
      lut3d .cube (parity-tested against grade_reference through real
      ffmpeg) instead of warning that it dropped LUTs/curves.
- [ ] Bezier handles on the keyframe curve editor; the full-width timeline
      curve lane. (Deferred — belongs with a keyframe-editor rework.)
- [x] **Auto tracking for power windows** (`track.rs`): zero-mean NCC
      template matching at 10 Hz on a 192×108 grid, patch 1.5× the window
      so the subject's edges are the feature; stops when correlation
      collapses instead of wandering. Writes MaskX/MaskY keyframes.
      `reel track` + a Track subject button. Proven end to end: a tracked
      window follows a moving square through a real render (76 vs 255).
- [x] Vectorscope and RGB parade in the scopes panel (+ scroll-to-scopes
      when opened).
- [x] Grade copy/paste between clips (Copy/Paste grade buttons; colour
      work only — fades/reframe stay per-shot) and "Paste to ALL clips"
      as the project look. CLI: `effects --like N [--like-all]`.

## E. Audio depth — shipped in v1.4.0

- [x] Pan per clip AND per track (`AudioFx.pan` + `Track.pan`, balance
      law, composed clamped). Live mixer applies channel gains; the export
      renders `pan=stereo|…` — both measured (astats per channel).
- [x] Meters on the mixer strips (decaying peaks per track bus + music)
      and momentary LUFS on the master (BS.1770 K-weighting, calibrated
      against a 997 Hz sine in a unit test). Meters silence on stop —
      a frozen bar is a lie.
- [x] EQ (low shelf 120 Hz, parametric bell, high shelf 8 kHz) and a
      compressor per clip. ONE model (`Clip.audio`): the live mixer runs
      real biquads + a feed-forward compressor (attack/release matched to
      the export's acompressor); the export renders bass/equalizer/treble/
      acompressor. Both sides measured: shelf depth, compression amount,
      below-threshold identity.
- [x] "Fix voice": 2× highpass 80 Hz (one leaves too much 50 Hz standing)
      + afftdn + adeclick at render time, honestly labelled export-only.
      Measured: steady hum guts >10 dB while bursty voice survives.
- [x] Filler-word removal: whisper `-ml 1` word-level cues →
      `filler_holes` (pure, tested) → `cut_holes` (extracted from tighten,
      now merges overlaps). `reel fillers [--words --pad --dry-run]`.
- [x] Beat detection (`beats.rs`): energy-flux onsets, adaptive threshold,
      pure detector tested on synthetic clicks AND a real rendered
      metronome. `reel beats` drops markers (music bed or any source);
      cuts and Ctrl+←/→ already snap to markers.
- [x] Voice recording straight into A1 with punch-in: PipeWire capture
      stream (same one-thread Rc architecture as the mixer), ⏺ Record
      voice rolls the timeline from the playhead and the take lands on A1
      where it started. Hand-rolled WAV writer, ffprobe-verified.

## F. Editorial workflow — shipped in v1.5.0 (two items open)

- [x] **Media pool** (`Project.pool`, `reel pool`): gather/bin/search,
      timeline sources absorbed automatically, offline files flagged in
      ember with a relink button. `Project::relink` repoints files AND
      moved directories (path-boundary-aware) everywhere at once — clips,
      pool, music, angles. `reel relink --from --to`.
- [x] **Source monitor**: ▶ on a pool item plays it in the player over
      the live edit; I/O mark the piece; Enter (or the Insert button)
      performs the three-point edit at the timeline playhead.
- [x] **Multicam cutting** (`Project.multicam` + `cut_to_angle`, pure and
      tested): `reel multicam --add FILE --align` syncs an angle by sound
      (best_lag) and the main camera auto-registers as angle 0; keys 1-9
      (and the panel's angle buttons) cut to an angle at the playhead —
      split, source swap, timeline time continuous. Verified end to end:
      audio-sync offset found exactly, cuts render the right camera.
- [ ] Compound clips (nest a sequence); adjustment layers. (Deferred —
      both need the render path to accept a sequence as a source.)
- [x] Shift+drag on empty timeline space lasso-selects clips (plain drag
      still scrubs). Track targeting for paste: not yet.
- [x] A keyboard-map overview — `?` or F1, four sections, everything
      from J-K-L to the trim modifiers.

## G. Delivery refinements — shipped in v1.6.0

- [x] Publish-everywhere grew a filename template ({name}, {platform})
      and per-platform caption burn-in checkboxes.
- [x] Markers take names (`reel marker --label`, attached by TIME so
      sorting never shuffles them); named markers name their MP4 chapters,
      and `reel chapters` emits YouTube-ready text (0:00 first, MM:SS).
- [x] `reel render --watch`: re-renders whenever the .reel changes —
      autosave plus this = a preview file that keeps itself fresh.
      Verified live (edit → automatic second render).
- [x] `--hdr-passthrough` on `reel convert`: 10-bit H.265/AV1/VP9 with
      the source's PQ/HLG tags restated (libx265 drops the generic
      -color_* flags — x265-params carries them; and ffprobe's csv prints
      fields in STRUCT order, not request order). ffprobe-verified.
      Timeline renders refuse it honestly (8-bit SDR compositor).

## H. Platform

- [ ] **macOS**: the build (VideoToolbox probing exists; ScreenCaptureKit;
      notarized dmg via the self-hosted runner). Needs Mac hardware/SDK.
- [ ] Windows capture backend (Windows.Graphics.Capture). Needs a Windows
      box to verify against.
- [ ] Flatpak and AppImage; Windows file associations + installer.
- [x] Mixer's Linux gate LIFTED for Windows (v1.6.0): WASAPI through
      cpal (the Linux objection — ALSA-only cpal on PipeWire desktops —
      doesn't apply there). Same MixState/render_into core, so meters,
      LUFS, EQ, pan all come along. Cross-compiles clean; untested on
      real Windows hardware yet.
- [ ] Wayland file-drop: UNBLOCKED upstream (winit PR #4571, a new DnD
      API, merged for 0.31) — lands here when egui/egui-winit adopt
      winit 0.31.

## I. 1.x hygiene

- [ ] Reconcile the Pixygon changelog server's version (0.40.0) with the
      shipped 1.0.0 before the next `pearl ship`.
- [ ] Sponsors button live once the org toggle is flipped; donation link
      on the site.
- [ ] CONTRIBUTING.md + labeled starter issues; CI running the full suite
      (Xvfb visual checks included) on PRs.
- [ ] Crash recovery (restore exact editor state) and named project
      snapshots — "never lose work" still has these two IOUs.
- [ ] Localization scaffolding; AccessKit wiring; UI scale setting.

---

The grand arc beyond 1.x — image editing that rivals Photoshop, the full
DAW ambition, plugins, `reel serve`/MCP — lives on in the original phased
plan (git history of this file, and the phases 5–7 sections it carried).
Nothing there is abandoned; 1.x is the season of making what shipped
*excellent* before widening again. When reality disagrees with this
document, reality wins — and this document gets edited.
