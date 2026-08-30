# Agent recipes

Reel is built to be driven by agents: every verb speaks `--json` (result
object on stdout, logs on stderr, non-zero exit on failure), `reel
commands --json` describes the whole interface, `reel serve` keeps one
process alive for many calls, and `reel mcp` exposes everything as MCP
tools. These recipes are real command sequences — each pattern here is
exercised by the test suite.

Paths should be absolute (`add` and `music set` canonicalize, but be
explicit anyway).

## Cut a highlight clip from a long recording

```bash
reel new cut.reel
reel add cut.reel vod.mp4 --in 754 --duration 42     # the moment
reel add cut.reel vod.mp4 --in 1310 --duration 18    # another one
reel transition cut.reel --clip <id2> --duration 0.5 # soften the join
reel render cut.reel highlight.mp4 --preset youtube
```

Clip ids come back in each command's JSON (`reel inspect cut.reel --json`
shows everything).

## Podcast: caption, de-um, tighten, publish shorts

```bash
reel new ep.reel
reel add ep.reel episode.mp4
reel captions ep.reel                 # local whisper; nothing uploads
reel fillers ep.reel                  # cut the ums/uhs (word-level timing)
reel tighten ep.reel                  # jump-cut the silences
reel render ep.reel ep-youtube.mp4 --preset youtube
reel render ep.reel ep-tiktok.mp4 --preset tiktok    # blur-fill 9:16
```

Captions ride the cuts: a line spanning an edit appears in both halves,
trimmed-away speech captions nowhere.

## Thumbnail from a frame

```bash
reel frame ep.reel --at 12.4 --out thumb-src.png     # rendered WITH grade/titles
reel convert thumb-src.png thumb.png --resolution 720
```

## Multicam interview

```bash
reel new iv.reel
reel add iv.reel camA.mp4                       # main angle on V1
reel multicam iv.reel --add camB.mp4 --align    # synced by sound; A auto-registers
reel multicam iv.reel --cut 42.5 --angle 1      # switch to B
reel multicam iv.reel --cut 88.0 --angle 0      # back to A
```

Or open the editor and press 1–9 while it plays.

## Music that fits and ducks

```bash
reel music set ep.reel bed.mp3 --gain-db -14    # ducks under speech on export
reel beats ep.reel                              # markers on every beat
reel chapters ep.reel                           # YouTube chapter text from markers
```

## Sync two recordings by sound

```bash
reel align ep.reel --clip <phoneAudioClip> --to <cameraClip>
```

## A still that matches the edit exactly

`reel frame` on a `.reel` renders through the same engine as the export —
grade, transitions mid-wipe, captions and titles included. On a bare media
file it's a plain frame grab.

## Keep a render fresh while editing

```bash
reel render ep.reel preview.mp4 --watch --quality small &
```

Autosave writes the project ~0.7 s after every change; the watch re-renders
on each save.

## Long-lived session (no process-per-command)

```bash
reel serve <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"info","params":{"args":["/media/vod.mp4"]}}
{"jsonrpc":"2.0","id":2,"method":"new","params":{"args":["/tmp/cut.reel"]}}
{"jsonrpc":"2.0","id":3,"method":"add","params":{"args":["/tmp/cut.reel","/media/vod.mp4"],"flags":{"in":"754","duration":"42"}}}
{"jsonrpc":"2.0","id":4,"method":"render","params":{"args":["/tmp/cut.reel","/tmp/out.mp4"],"flags":{"quiet":true}}}
EOF
```

Responses are one JSON object per line, matched by `id` (they may arrive
out of order — a render doesn't block a probe).

## MCP

Point any MCP client at:

```json
{ "command": "reel", "args": ["mcp"] }
```

Every verb appears as a tool with a schema derived from the same table
that drives the CLI. Positional arguments use their lower-cased table
names (`project`, `output`, `media`, `target`); flags keep their flag
names.

## The document itself

`reel schema` prints the versioned JSON Schema for `.reel` files —
generated from the live types. Every field is optional with a default, so
documents only ever grow and old files keep loading.
