# Driving Reel from an agent

Reel edits video from the command line. No window, no display, no GUI
automation — every editing operation is a command that reads and writes a
plain-JSON project file.

Start here:

```bash
reel commands --json     # every command, argument and flag, machine-readable
```

That output is generated from the same table that parses the arguments, so it
is always correct for the binary you are holding. Prefer reading it at runtime
over hard-coding anything below.

## The model

A `.reel` file is a project: clips on tracks, plus titles, captions, a music
bed and markers. Editing is *make a project → change it → render it*.

```bash
reel new cut.reel --size 1920x1080 --fps 30
reel add cut.reel a.mp4 --in 2 --duration 5    # 5s of a.mp4, starting 0:02 in
reel add cut.reel b.mp4                        # whole file, after the last clip
reel inspect cut.reel --json                   # clip ids, times, everything
reel render cut.reel out.mp4 --preset tiktok
```

## Six things worth knowing

1. **`--json` on everything.** One object on stdout, nothing else. Progress
   and logs go to stderr, so `cmd --json 2>/dev/null | jq` is always safe.
2. **Errors are JSON too**, and always exit non-zero:
   `{"ok": false, "error": "no clip with id 999 — run `reel inspect` to see the ids"}`.
   Exit 1 = the command failed. Exit 2 = it wasn't a command (typo).
3. **Address clips by `id`, from `reel inspect`.** Ids are stable across edits
   elsewhere in the timeline; positions and indexes are not.
4. **Two different times.** `start` = where a clip sits on the timeline
   (`--at`, `--to`). `in_point` = where it begins inside its source file
   (`--in`). Getting these confused is the most common mistake.
5. **Fractions, not pixels.** Title `--x/--y/--size` are fractions of the
   frame, so the same numbers work at any export resolution.
6. **Unknown flags are refused.** A typo errors out instead of being ignored,
   so a render never quietly differs from what was asked for.

## Captions run locally

`reel captions cut.reel` transcribes the speech on the machine it runs on.
There is no account, no API key and nothing uploaded. The engine (~9 MB) and
model (~75 MB) are fetched automatically on first use and cached in
`~/.cache/reel`; after that it works offline.

Cues are generated against the source and mapped through the edit, so a line
spanning a cut appears in both halves and speech you trimmed away captions
nowhere. Add `--srt out.srt` to also get the file.

## A complete example

```bash
set -e
reel new promo.reel --size 1080x1920 --fps 30
reel add promo.reel shot-a.mp4 --duration 4
reel add promo.reel shot-b.mp4 --duration 3

read -r A B <<< "$(reel inspect promo.reel --json | jq -r '[.clips[].id] | @tsv')"
reel transition promo.reel --clip "$B" --seconds 0.5
reel effects   promo.reel --clip "$A" --saturation 1.2 --fade-in 0.5
reel captions  promo.reel --model base
reel title add promo.reel --text "New drop" --at 0.2 --duration 2.5 \
     --y 0.15 --size 0.11 --color ffcc00
reel music set promo.reel bed.mp3 --gain-db -14   # ducks under the speech

reel render promo.reel promo.mp4 --preset tiktok --json
```

## Gotchas

- `render` refuses to overwrite an existing file unless you pass
  `--overwrite`. This is deliberate: a rerun should not destroy the last one.
- `reel presets --json` lists the social destinations (`--preset tiktok`,
  `--preset "Instagram feed"`, …) with their exact frames.
- `--no-hardware` forces the software encoder. Hardware (NVENC /
  VideoToolbox) is used automatically when it's actually usable, which Reel
  checks with a real trial encode rather than trusting a codec listing.
- ffmpeg must be on `PATH`; Reel fetches a private build if it isn't.

Full reference: [`docs/CLI.md`](docs/CLI.md) · <https://reel.pixygon.io/cli>
