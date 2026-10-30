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

## Capture the screen and cut it, without touching a mouse

```bash
reel devices --json                                    # names to point at
reel record demo.mp4 --duration 30 --audio both        # blocks; returns the file
reel new demo.reel
reel add demo.reel demo.mp4
reel tighten demo.reel                                 # drop the dead air
reel captions demo.reel
reel render demo.reel demo-final.mp4 --preset youtube
```

`--duration` is the shape to reach for from an agent: one call, one finished
file, nothing to remember. Without it the recording runs in the background
and a later `reel record --stop` finishes it — useful when the length isn't
known up front:

```bash
reel record --area 0,0,1920x1080 --fps 60 --json      # returns immediately
reel record --status --json                           # running? how long?
reel record --stop --project demo.reel                # finish, append as a clip
```

## A screenshot of exactly the pixels you mean

```bash
reel screenshot bug.png --area 240,120,1280x720       # no picker, no person
reel screenshot menu.png --delay 3                    # time to open the menu
```

Exact rectangles are honoured natively by grim, maim and ffmpeg, and cropped
out of a full-screen grab where the desktop's own tool has no geometry flag —
so `--area` means the same thing on every machine.

## Streamer layout in one call

```bash
reel record --duration 60 --streamer
```

Records the screen and the camera together and hands back a `.reel` with the
screen on V1 and the camera as a corner PiP, ready to open or render.

## A glitch sting over an angled stamp

```bash
reel new sting.reel --size 1920x1080
reel add sting.reel color:101820 --duration 4
reel add sting.reel stamp.png --track overlay --at 0 --duration 4   # alpha is kept
reel pip sting.reel --clip 101 --x 0.5 --y 0.5 --scale 0.4
reel effects sting.reel --clip 101 --rotate-deg -12 --build tiles 24 --build-seconds 1.2
reel effects sting.reel --clip 100 --rgb-split 8 --glitch 3 --shake 12
```

Picture effects run in the frame server, so `reel render` and `reel frame` need
a GPU. The no-GPU graph fallback warns and leaves them out. A shake on an
adjustment clip moves everything beneath it; burned-in titles stay still.

## One episode template, every aspect

```bash
reel new ep.reel --size 1920x1080
reel add ep.reel intro.reel                          # nested: re-renders if it changes
reel add ep.reel capture.mp4 --slot capture
reel title add ep.reel --text "Episode 1" --y 0.85 --slot card
reel variant add ep.reel 9x16 --size 1080x1920

# Next week: same project, new content, both aspects.
reel slot set ep.reel capture --media capture-2.mp4
reel slot set ep.reel card --text "Episode 2"
reel render ep.reel ep2.mp4
reel render ep.reel ep2-9x16.mp4 --variant 9x16
```

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
