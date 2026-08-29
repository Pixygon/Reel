# The 1.0 audit

Conducted 2026-08-29, against `v1.0.0` as shipped. Method: full test suite,
a kitchen-sink project exercising every feature at once (three sources,
wipe + dip transitions, LUT + curves + mask + keyframed zoom, a speed
change, PiP overlay, title, music bed, an A1 clip, markers, captions),
rendered through both engines and probed; a 0.14-era project loaded and
rendered for backwards compatibility; error paths sampled; performance
budgets re-measured; the live preview smoke-tested under Xvfb.

## Verified healthy

- **112 tests green, zero warnings.** The kitchen sink renders to exactly
  its computed 8.00 s (4+4+2 − two 1 s transitions), chapters and AAC audio
  present, at **1.7× realtime** with every feature enabled.
- **Backwards compatibility**: a v0.14-era `.reel` (no captions, titles,
  music, markers, LUTs, speed, keys, or masks in its JSON) loads and
  renders byte-for-schema clean.
- **The graph fallback** still renders the kitchen sink (8.0 s), warning
  about what it cannot animate.
- **Error paths**: JSON errors with non-zero exits; typo'd verbs exit 2;
  broken `.cube` files and malformed `--points` refused with usable
  messages.
- **Budgets**: cold open window at **60 ms**, first frame **745 ms**
  (budget: 400/1000). 1800-clip project inspected in **64 ms**. Export ≥
  realtime with the full stack enabled.

## Found and fixed in this audit (v1.0.1)

Speed-changed clips lied in the preview, four ways — every mapping between
timeline and source assumed 1×:

1. **The playhead ran at source rate** during playback of a sped clip
   (`update_editor_playback` mapped `pos − in_point` linearly): a 2× clip
   dragged the playhead through the timeline at twice its slot's rate.
2. **Seeks landed on the wrong frame** (`seek_timeline` ignored the speed
   curve); now mapped through `source_offset_at`.
3. **Head-trims shifted `in_point` by timeline deltas** instead of source
   deltas (`delta × speed`).
4. **The picture ambled at 1×**: the preview now drives mpv at the clip's
   average rate, so a 2× clip *moves* at 2× (ramps approximate with their
   mean; the render walks the true curve).

All four fixed, suite green. The renders were always correct — these were
preview-only lies, which is exactly the kind the constitution forbids.

## Found and logged (the refinement roadmap's seed)

- **Two time systems.** The editor timeline keeps gaps and does not
  collapse transition overlaps; render time does. The scrubber can read
  `00:10.00 / 00:08.00` at the end of an edit with transitions. The
  preview needs one truth of time.
- **The speed control is hijacked** in the editor: clip-rate forcing
  overwrites the transport's user-facing rate display each frame.
- **`captions --source` maps zero cues silently** when the transcribed file
  isn't on the timeline — correct behaviour, but it should say so.
- **The graph fallback warns only about keyframes**; it also silently drops
  masks, LUTs, curves and chroma keys. The warning should cover everything
  it cannot do.
- **`render_still` ignores transitions** (a still grabbed mid-fade shows no
  blend) and does not burn captions/titles.
- **The PiP preview pool decodes at full source size** for a quarter-frame
  inset — `set_display_size` is never applied to pooled players.
- **Speed-changed preview audio is pitched** (linear resample); the render
  preserves pitch via atempo. Documented, but a real stretcher belongs in
  the preview.
- **Grade lattice cache never evicts** within a session (bounded leak,
  a few hundred KB per distinct grade edited).
- **Infrastructure**: the Pixygon changelog server believes the version is
  0.40.0 while Cargo/CDN/site say 1.0.0 — reconcile before the next
  `pearl ship`. GitHub Sponsors still needs the org toggle.

Everything here is captured as work items in ROADMAP.md.
