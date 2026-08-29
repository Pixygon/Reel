//! Reel from a terminal — and, mostly, Reel from an agent.
//!
//! Everything the editor can do to a project, this can do without a window:
//! build a cut, trim it, title it, caption it, score it, render it. A `.reel`
//! project is plain JSON, so the natural way to automate Reel is to make a
//! project and then operate on it, which is exactly what these verbs do.
//!
//! Two rules make it safe to drive blind:
//!
//! 1. **One table.** `COMMANDS` below is what parses the arguments, what
//!    prints the help, and what `reel commands --json` emits. Documentation
//!    cannot drift from behaviour, because they are the same data.
//! 2. **Every verb speaks JSON.** `--json` turns any command's result — and
//!    any failure — into one object on stdout, with a non-zero exit code on
//!    error. Nothing has to be scraped out of prose.

use crate::edit::{Music, Project, TrackKind};
use crate::export::{self, AudioMode, Codec, Fit, Quality, Resolution};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::io::Write;

/// Print a line, treating a closed stdout as a normal end rather than a
/// panic. `reel commands | head` is an obvious thing to type, and the
/// default `println!` aborts with "failed printing to stdout: Broken pipe".
fn say(s: &str) {
    let mut out = std::io::stdout();
    if writeln!(out, "{s}").is_err() {
        std::process::exit(0);
    }
}

// ── The one table ────────────────────────────────────────────────────────

pub struct Flag {
    pub name: &'static str,
    /// `Some("SECONDS")` takes a value; `None` is a switch.
    pub value: Option<&'static str>,
    pub help: &'static str,
}

pub struct Cmd {
    pub name: &'static str,
    /// Positional arguments, in order. A trailing `?` marks it optional.
    pub args: &'static [&'static str],
    pub flags: &'static [Flag],
    pub help: &'static str,
}

const F_JSON: Flag = Flag { name: "json", value: None, help: "Print the result as JSON" };

/// Render options, shared by `render` and `convert`.
const RENDER_FLAGS: &[Flag] = &[
    Flag { name: "preset", value: Some("NAME"), help: "A social preset: see `reel presets`" },
    Flag { name: "codec", value: Some("NAME"), help: "h264, h265, av1, vp9, remux, mp3, m4a, opus, flac, wav, png, jpeg, webp" },
    Flag { name: "quality", value: Some("NAME"), help: "high, balanced, small, or a CRF number" },
    Flag { name: "resolution", value: Some("HEIGHT"), help: "source, 2160, 1080, 720, 480" },
    Flag { name: "fit", value: Some("MODE"), help: "letterbox, crop or blur (how a mismatched aspect is filled)" },
    Flag { name: "audio", value: Some("MODE"), help: "copy (pass the source audio through) or encode" },
    Flag { name: "loudness", value: Some("LUFS"), help: "Deliver audio at this integrated loudness (e.g. -14); presets set it automatically" },
    Flag { name: "no-hardware", value: None, help: "Force the software encoder" },
    Flag { name: "overwrite", value: None, help: "Replace the output file if it exists" },
    Flag { name: "quiet", value: None, help: "Don't print progress" },
    F_JSON,
];

pub static COMMANDS: &[Cmd] = &[
    Cmd {
        name: "info",
        args: &["MEDIA"],
        flags: &[F_JSON],
        help: "Duration, frame size and rate of a media file",
    },
    Cmd {
        name: "new",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "name", value: Some("TEXT"), help: "Project name" },
            Flag { name: "size", value: Some("WxH"), help: "Frame size, e.g. 1920x1080" },
            Flag { name: "fps", value: Some("N"), help: "Frame rate" },
            F_JSON,
        ],
        help: "Create an empty .reel project",
    },
    Cmd {
        name: "inspect",
        args: &["PROJECT"],
        flags: &[F_JSON],
        help: "The whole project — clips with their ids, titles, captions, music, markers",
    },
    Cmd {
        name: "add",
        args: &["PROJECT", "MEDIA"],
        flags: &[
            Flag { name: "at", value: Some("SECONDS"), help: "Timeline position (default: after the last clip)" },
            Flag { name: "in", value: Some("SECONDS"), help: "Start point inside the source (default 0)" },
            Flag { name: "duration", value: Some("SECONDS"), help: "How much of the source to use (default: all of it)" },
            Flag { name: "track", value: Some("KIND"), help: "video, overlay (picture-in-picture) or audio" },
            F_JSON,
        ],
        help: "Append a piece of media to the timeline",
    },
    Cmd {
        name: "split",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "at", value: Some("SECONDS"), help: "Where to cut" },
            F_JSON,
        ],
        help: "Cut every clip that crosses a point in the timeline",
    },
    Cmd {
        name: "trim",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id (from `reel inspect`)" },
            Flag { name: "in", value: Some("SECONDS"), help: "New start point inside the source" },
            Flag { name: "duration", value: Some("SECONDS"), help: "New length" },
            Flag { name: "start", value: Some("SECONDS"), help: "New timeline position" },
            F_JSON,
        ],
        help: "Change a clip's source window or position",
    },
    Cmd {
        name: "move",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id" },
            Flag { name: "to", value: Some("SECONDS"), help: "New timeline position" },
            F_JSON,
        ],
        help: "Move a clip along the timeline",
    },
    Cmd {
        name: "roll",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "The cut between this clip and its left neighbour moves" },
            Flag { name: "by", value: Some("SECONDS"), help: "Positive = the neighbour grows; total length never changes" },
            F_JSON,
        ],
        help: "Roll a cut: one clip grows, the other shrinks, the timeline stays put",
    },
    Cmd {
        name: "slip",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id" },
            Flag { name: "by", value: Some("SECONDS"), help: "Shift the clip's window through its source" },
            F_JSON,
        ],
        help: "Slip a clip: change WHAT plays without moving WHEN",
    },
    Cmd {
        name: "slide",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id (must touch neighbours on both sides)" },
            Flag { name: "by", value: Some("SECONDS"), help: "Move the clip; neighbours absorb the motion" },
            F_JSON,
        ],
        help: "Slide a clip between its neighbours; the combined span is unchanged",
    },
    Cmd {
        name: "remove",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id" },
            Flag { name: "ripple", value: None, help: "Close the gap left behind" },
            F_JSON,
        ],
        help: "Delete a clip",
    },
    Cmd {
        name: "gap",
        args: &["PROJECT"],
        flags: &[F_JSON],
        help: "Close every gap between clips",
    },
    Cmd {
        name: "gain",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id" },
            Flag { name: "db", value: Some("DECIBELS"), help: "Level change, e.g. -6 or 3" },
            F_JSON,
        ],
        help: "Set a clip's audio level",
    },
    Cmd {
        name: "effects",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id" },
            Flag { name: "exposure", value: Some("N"), help: "1.0 = unchanged" },
            Flag { name: "contrast", value: Some("N"), help: "1.0 = unchanged" },
            Flag { name: "saturation", value: Some("N"), help: "1.0 = unchanged" },
            Flag { name: "fade-in", value: Some("SECONDS"), help: "Fade up from black" },
            Flag { name: "fade-out", value: Some("SECONDS"), help: "Fade down to black" },
            Flag { name: "zoom", value: Some("N"), help: "1.0 = whole frame; used for reframing" },
            Flag { name: "pan-x", value: Some("N"), help: "-1..1, where the zoom sits" },
            Flag { name: "pan-y", value: Some("N"), help: "-1..1" },
            Flag { name: "key-color", value: Some("RRGGBB"), help: "Chroma key: knock this colour out (e.g. 00b140)" },
            Flag { name: "key-similarity", value: Some("0..1"), help: "How far from the key colour still counts (default 0.3)" },
            Flag { name: "key-softness", value: Some("0..1"), help: "Soft edge width beyond similarity (default 0.1)" },
            Flag { name: "key-off", value: None, help: "Stop keying" },
            Flag { name: "reset", value: None, help: "Back to no effects" },
            F_JSON,
        ],
        help: "Colour, fades and reframing for one clip",
    },
    Cmd {
        name: "keyframe",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id (from `reel inspect`)" },
            Flag { name: "param", value: Some("NAME"), help: "exposure, contrast, saturation, zoom, pan-x, pan-y, opacity, pip-x, pip-y, pip-scale" },
            Flag { name: "at", value: Some("SECONDS"), help: "TIMELINE time of the keyframe" },
            Flag { name: "value", value: Some("N"), help: "The value at that moment" },
            Flag { name: "interp", value: Some("MODE"), help: "linear (default), hold or ease" },
            Flag { name: "remove", value: None, help: "Remove the keyframe nearest --at instead" },
            Flag { name: "list", value: None, help: "Show every keyframe on the clip" },
            F_JSON,
        ],
        help: "Animate a parameter over time — evaluated per frame at render",
    },
    Cmd {
        name: "pip",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "An overlay clip's id" },
            Flag { name: "x", value: Some("0..1"), help: "Centre of the inset across the frame" },
            Flag { name: "y", value: Some("0..1"), help: "Centre of the inset down the frame" },
            Flag { name: "scale", value: Some("0..1"), help: "Inset width as a fraction of the frame" },
            F_JSON,
        ],
        help: "Place a picture-in-picture overlay in the frame",
    },
    Cmd {
        name: "speed",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id" },
            Flag { name: "rate", value: Some("N"), help: "2 = twice as fast, 0.5 = half. Audio follows." },
            Flag { name: "keep-length", value: None, help: "Keep the timeline slot; use more or less source" },
            F_JSON,
        ],
        help: "Change how fast a clip plays",
    },
    Cmd {
        name: "transition",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "Clip id — the fade runs INTO this clip" },
            Flag { name: "seconds", value: Some("SECONDS"), help: "Transition length (0 = hard cut)" },
            Flag { name: "kind", value: Some("NAME"), help: "fade, dip, wipe-left/right/up/down, slide-left/right" },
            F_JSON,
        ],
        help: "Crossfade from the previous clip into this one",
    },
    Cmd {
        name: "title",
        args: &["ACTION", "PROJECT"],
        flags: &[
            Flag { name: "text", value: Some("TEXT"), help: "The words" },
            Flag { name: "at", value: Some("SECONDS"), help: "When it appears" },
            Flag { name: "duration", value: Some("SECONDS"), help: "How long it stays" },
            Flag { name: "x", value: Some("0..1"), help: "Horizontal centre, as a fraction of the frame" },
            Flag { name: "y", value: Some("0..1"), help: "Vertical centre, as a fraction of the frame" },
            Flag { name: "size", value: Some("0..1"), help: "Text height as a fraction of the frame" },
            Flag { name: "color", value: Some("RRGGBB"), help: "Hex colour, e.g. ffcc00" },
            Flag { name: "no-bold", value: None, help: "Regular weight" },
            Flag { name: "no-outline", value: None, help: "No dark outline" },
            Flag { name: "index", value: Some("N"), help: "Which title (for remove)" },
            F_JSON,
        ],
        help: "ACTION is add, list or remove — text placed on the picture",
    },
    Cmd {
        name: "music",
        args: &["ACTION", "PROJECT", "AUDIO?"],
        flags: &[
            Flag { name: "gain-db", value: Some("DECIBELS"), help: "Level (default -12)" },
            Flag { name: "no-duck", value: None, help: "Don't pull the music down under speech" },
            Flag { name: "fade", value: Some("SECONDS"), help: "Fade in/out (default 1)" },
            F_JSON,
        ],
        help: "ACTION is set or clear — a music bed under the whole edit",
    },
    Cmd {
        name: "marker",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "at", value: Some("SECONDS"), help: "Where to put it" },
            Flag { name: "remove", value: None, help: "Take it away instead" },
            Flag { name: "list", value: None, help: "Show the markers" },
            F_JSON,
        ],
        help: "Flag a position in the timeline",
    },
    Cmd {
        name: "align",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "clip", value: Some("ID"), help: "The clip to move" },
            Flag { name: "to", value: Some("ID"), help: "The clip to sync against" },
            Flag { name: "window", value: Some("SECONDS"), help: "Largest offset to search (default 90)" },
            F_JSON,
        ],
        help: "Sync one clip to another by their AUDIO — multicam without clap sticks",
    },
    Cmd {
        name: "tighten",
        args: &["PROJECT"],
        flags: &[
            Flag { name: "threshold", value: Some("0..1"), help: "Quiet = below this fraction of the source's own peak (default 0.06)" },
            Flag { name: "min-gap", value: Some("SECONDS"), help: "Only cut silences at least this long (default 0.6)" },
            Flag { name: "pad", value: Some("SECONDS"), help: "Breathing room kept on each side of a cut (default 0.15)" },
            F_JSON,
        ],
        help: "Cut the silent air out of the edit and close up — the podcast jump-cut",
    },
    Cmd {
        name: "captions",
        args: &["TARGET"],
        flags: &[
            Flag { name: "model", value: Some("NAME"), help: "tiny, base or small (default base)" },
            Flag { name: "size", value: Some("N"), help: "Caption size (default 20)" },
            Flag { name: "srt", value: Some("FILE"), help: "Also write the captions to this .srt" },
            Flag { name: "source", value: Some("MEDIA"), help: "Transcribe this instead of the project's first clip" },
            Flag { name: "quiet", value: None, help: "Don't print progress" },
            F_JSON,
        ],
        help: "Transcribe speech locally. TARGET is a .reel project or a media file",
    },
    Cmd {
        name: "frame",
        args: &["TARGET"],
        flags: &[
            Flag { name: "at", value: Some("SECONDS"), help: "Which moment (default 0)" },
            Flag { name: "out", value: Some("FILE.png"), help: "Where to write the PNG (default beside the target)" },
            Flag { name: "overwrite", value: None, help: "Replace the output if it exists" },
            F_JSON,
        ],
        help: "Export one frame as PNG. TARGET is a .reel (rendered with effects, overlays, animation) or a media file",
    },
    Cmd {
        name: "render",
        args: &["PROJECT", "OUTPUT"],
        flags: RENDER_FLAGS,
        help: "Render the edit — captions, titles and music included",
    },
    Cmd {
        name: "convert",
        args: &["MEDIA", "OUTPUT"],
        flags: RENDER_FLAGS,
        help: "Transcode one file, no project needed",
    },
    Cmd {
        name: "presets",
        args: &[],
        flags: &[F_JSON],
        help: "The one-click destinations (YouTube, TikTok, Reels…)",
    },
    Cmd {
        name: "commands",
        args: &[],
        flags: &[F_JSON],
        help: "Every command, argument and flag — the machine-readable manual",
    },
];

// ── Parsing ──────────────────────────────────────────────────────────────

/// Is this argument a command rather than a file to open?
///
/// Checked against a real file first, so `reel render` opens a video actually
/// named "render" instead of complaining about missing arguments.
pub fn is_command(arg: &str) -> bool {
    if std::path::Path::new(arg).exists() {
        return false;
    }
    COMMANDS.iter().any(|c| c.name == arg)
}

#[derive(Debug)]
struct Parsed {
    positional: Vec<String>,
    values: HashMap<String, String>,
    switches: HashSet<String>,
}

impl Parsed {
    fn str(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }
    fn on(&self, name: &str) -> bool {
        self.switches.contains(name)
    }
    fn num<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>> {
        match self.values.get(name) {
            None => Ok(None),
            Some(v) => v
                .parse::<T>()
                .map(Some)
                .map_err(|_| anyhow!("--{name} expects a number, got {v:?}")),
        }
    }
    fn need_num<T: std::str::FromStr>(&self, name: &str) -> Result<T> {
        self.num(name)?.ok_or_else(|| anyhow!("--{name} is required"))
    }
    fn at(&self, i: usize) -> Result<&str> {
        self.positional
            .get(i)
            .map(String::as_str)
            .ok_or_else(|| anyhow!("missing argument"))
    }
}

fn parse(cmd: &Cmd, args: &[String]) -> Result<Parsed> {
    let mut p = Parsed {
        positional: Vec::new(),
        values: HashMap::new(),
        switches: HashSet::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if let Some(name) = a.strip_prefix("--") {
            // `--flag=value` is as valid as `--flag value`.
            let (name, inline) = match name.split_once('=') {
                Some((n, v)) => (n, Some(v.to_string())),
                None => (name, None),
            };
            let Some(flag) = cmd.flags.iter().find(|f| f.name == name) else {
                let known: Vec<&str> = cmd.flags.iter().map(|f| f.name).collect();
                bail!("unknown flag --{name} for `reel {}`. Known: --{}", cmd.name, known.join(" --"));
            };
            match flag.value {
                None => {
                    if inline.is_some() {
                        bail!("--{name} is a switch and takes no value");
                    }
                    p.switches.insert(name.to_string());
                }
                Some(_) => {
                    let v = match inline {
                        Some(v) => v,
                        None => {
                            i += 1;
                            args.get(i)
                                .cloned()
                                .ok_or_else(|| anyhow!("--{name} needs a value"))?
                        }
                    };
                    p.values.insert(name.to_string(), v);
                }
            }
        } else {
            p.positional.push(a.clone());
        }
        i += 1;
    }
    let required = cmd.args.iter().filter(|a| !a.ends_with('?')).count();
    if p.positional.len() < required {
        bail!(
            "`reel {}` needs {}. Usage: reel {} {}",
            cmd.name,
            cmd.args.join(", "),
            cmd.name,
            cmd.args.join(" ")
        );
    }
    Ok(p)
}

// ── Entry point ──────────────────────────────────────────────────────────

/// Run a CLI command. Returns the process exit code.
pub fn run(argv: &[String]) -> i32 {
    let name = &argv[0];
    let Some(cmd) = COMMANDS.iter().find(|c| c.name == name) else {
        eprintln!("reel: unknown command {name:?}. Try `reel help`.");
        return 2;
    };
    let rest = &argv[1..];
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        print_command_help(cmd);
        return 0;
    }
    let json = rest.iter().any(|a| a == "--json");

    let result = parse(cmd, rest).and_then(|p| dispatch(cmd.name, &p));
    match result {
        Ok(out) => {
            if json {
                say(&serde_json::to_string_pretty(&out.data).unwrap_or_default());
            } else if !out.text.is_empty() {
                say(&out.text);
            }
            0
        }
        Err(e) => {
            if json {
                let obj = serde_json::json!({ "ok": false, "error": e.to_string() });
                say(&serde_json::to_string_pretty(&obj).unwrap_or_default());
            } else {
                eprintln!("reel: {e}");
            }
            1
        }
    }
}

/// What a command produced: a line for a human, an object for a machine.
struct Output {
    text: String,
    data: serde_json::Value,
}

impl Output {
    fn new(text: impl Into<String>, data: serde_json::Value) -> Self {
        let mut data = data;
        if let Some(o) = data.as_object_mut() {
            o.insert("ok".into(), serde_json::Value::Bool(true));
        }
        Self { text: text.into(), data }
    }
}

fn dispatch(name: &str, p: &Parsed) -> Result<Output> {
    match name {
        "info" => cmd_info(p),
        "new" => cmd_new(p),
        "inspect" => cmd_inspect(p),
        "add" => cmd_add(p),
        "split" => cmd_split(p),
        "trim" => cmd_trim(p),
        "move" => cmd_move(p),
        "roll" => cmd_nudge(p, Nudge::Roll),
        "slip" => cmd_nudge(p, Nudge::Slip),
        "slide" => cmd_nudge(p, Nudge::Slide),
        "remove" => cmd_remove(p),
        "gap" => cmd_gap(p),
        "gain" => cmd_gain(p),
        "effects" => cmd_effects(p),
        "keyframe" => cmd_keyframe(p),
        "pip" => cmd_pip(p),
        "speed" => cmd_speed(p),
        "transition" => cmd_transition(p),
        "title" => cmd_title(p),
        "music" => cmd_music(p),
        "marker" => cmd_marker(p),
        "align" => cmd_align(p),
        "tighten" => cmd_tighten(p),
        "captions" => cmd_captions(p),
        "frame" => cmd_frame(p),
        "render" => cmd_render(p),
        "convert" => cmd_convert(p),
        "presets" => cmd_presets(),
        "commands" => Ok(cmd_commands()),
        _ => bail!("unimplemented command {name}"),
    }
}

// ── Help ─────────────────────────────────────────────────────────────────

pub fn print_help() {
    say(&format!(
        "Reel {} — media player, editor and capture tool.\n\n\
         Usage:\n  reel [FILE]              open it in the player\n  \
         reel COMMAND [ARGS]      work without a window\n",
        env!("CARGO_PKG_VERSION")
    ));
    let width = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(8);
    say("Commands:");
    for c in COMMANDS {
        say(&format!("  {:width$}  {}", c.name, c.help));
    }
    say(
        "\nEvery command takes --json, which prints one object and exits\n\
         non-zero on failure. `reel commands --json` describes them all.\n\n\
         reel COMMAND --help    detail for one command\n\
         Docs: https://reel.pixygon.io/cli",
    );
}

fn print_command_help(c: &Cmd) {
    say(&format!("reel {} {}\n\n{}\n", c.name, c.args.join(" "), c.help));
    if !c.flags.is_empty() {
        say("Options:");
        let w = c
            .flags
            .iter()
            .map(|f| f.name.len() + f.value.map(|v| v.len() + 1).unwrap_or(0))
            .max()
            .unwrap_or(8);
        for f in c.flags {
            let lhs = match f.value {
                Some(v) => format!("{} {v}", f.name),
                None => f.name.to_string(),
            };
            say(&format!("  --{lhs:w$}  {}", f.help));
        }
    }
}

fn cmd_commands() -> Output {
    let cmds: Vec<serde_json::Value> = COMMANDS
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "help": c.help,
                "args": c.args,
                "flags": c.flags.iter().map(|f| serde_json::json!({
                    "name": f.name,
                    "takes_value": f.value.is_some(),
                    "value": f.value,
                    "help": f.help,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let text = {
        let mut s = String::new();
        for c in COMMANDS {
            s.push_str(&format!("reel {} {}\n    {}\n", c.name, c.args.join(" "), c.help));
        }
        s.trim_end().to_string()
    };
    Output::new(
        text,
        serde_json::json!({ "version": env!("CARGO_PKG_VERSION"), "commands": cmds }),
    )
}

// ── Project helpers ──────────────────────────────────────────────────────

fn load(path: &str) -> Result<Project> {
    Project::load(path).with_context(|| format!("could not read the project {path}"))
}

fn save(p: &Project, path: &str) -> Result<()> {
    p.save(path).with_context(|| format!("could not write the project {path}"))
}

fn clip_json(c: &crate::edit::Clip, kind: TrackKind) -> serde_json::Value {
    serde_json::json!({
        "id": c.id,
        "name": c.name,
        "source": c.source,
        "track": match kind {
            TrackKind::Video => "video",
            TrackKind::Overlay => "overlay",
            TrackKind::Audio => "audio",
        },
        "pip": if kind == TrackKind::Overlay { serde_json::to_value(c.pip).unwrap_or(serde_json::Value::Null) } else { serde_json::Value::Null },
        "start": c.start,
        "duration": c.duration,
        "in_point": c.in_point,
        "end": c.end(),
        "gain_db": c.gain_db,
        "transition_in": c.transition_in,
        "speed": c.speed,
        "source_len": c.source_len(),
        "keyframes": c.keys.iter().map(|(p, k)| (p.name(), k.len())).collect::<Vec<_>>(),
    })
}

fn find_clip_mut<'a>(p: &'a mut Project, id: u64) -> Result<&'a mut crate::edit::Clip> {
    p.tracks
        .iter_mut()
        .flat_map(|t| t.clips.iter_mut())
        .find(|c| c.id == id)
        .ok_or_else(|| anyhow!("no clip with id {id} — run `reel inspect` to see the ids"))
}

// ── Commands ─────────────────────────────────────────────────────────────

fn cmd_info(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let info = crate::video::decoder::probe(path)?;
    Ok(Output::new(
        format!(
            "{path}\n  {}×{} @ {:.3} fps\n  {:.3}s",
            info.width, info.height, info.fps, info.duration
        ),
        serde_json::json!({
            "path": path,
            "width": info.width,
            "height": info.height,
            "fps": info.fps,
            "duration": info.duration,
        }),
    ))
}

fn cmd_new(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let mut proj = Project::default();
    if let Some(n) = p.str("name") {
        proj.name = n.to_string();
    }
    if let Some(size) = p.str("size") {
        let (w, h) = size
            .split_once(['x', 'X'])
            .ok_or_else(|| anyhow!("--size wants WxH, e.g. 1920x1080"))?;
        proj.width = w.trim().parse().map_err(|_| anyhow!("bad width in --size"))?;
        proj.height = h.trim().parse().map_err(|_| anyhow!("bad height in --size"))?;
    }
    if let Some(fps) = p.num::<f64>("fps")? {
        proj.fps = fps;
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!("Created {path} — {}×{} @ {} fps", proj.width, proj.height, proj.fps),
        serde_json::json!({ "project": path, "width": proj.width, "height": proj.height, "fps": proj.fps }),
    ))
}

fn cmd_inspect(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let proj = load(path)?;
    let clips: Vec<serde_json::Value> = proj
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(move |c| clip_json(c, t.kind)))
        .collect();
    let duration = crate::edit::render_duration(&proj.export_segments());
    let text = {
        let mut s = format!(
            "{} — {}×{} @ {} fps · {:.2}s\n",
            proj.name, proj.width, proj.height, proj.fps, duration
        );
        for t in &proj.tracks {
            for c in &t.clips {
                s.push_str(&format!(
                    "  [{}] {} {:.2}s–{:.2}s (source {:.2}s+) {}\n",
                    c.id,
                    c.name,
                    c.start,
                    c.end(),
                    c.in_point,
                    match t.kind {
                        TrackKind::Video => "V",
                        TrackKind::Overlay => "PiP",
                        TrackKind::Audio => "A",
                    }
                ));
            }
        }
        if !proj.titles.is_empty() {
            s.push_str(&format!("  {} title(s)\n", proj.titles.len()));
        }
        if !proj.captions.is_empty() {
            s.push_str(&format!("  {} caption(s)\n", proj.captions.len()));
        }
        if proj.music.is_some() {
            s.push_str("  music bed\n");
        }
        s.trim_end().to_string()
    };
    Ok(Output::new(
        text,
        serde_json::json!({
            "project": path,
            "name": proj.name,
            "width": proj.width,
            "height": proj.height,
            "fps": proj.fps,
            "duration": duration,
            "clips": clips,
            "titles": proj.titles,
            "captions": proj.captions,
            "caption_size": proj.caption_size,
            "music": proj.music,
            "markers": proj.markers,
        }),
    ))
}

fn cmd_add(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let media = p.at(1)?;
    if !std::path::Path::new(media).exists() {
        bail!("no such media file: {media}");
    }
    // Store the absolute path: a project written in one directory must still
    // find its media when opened from anywhere else.
    let media = std::fs::canonicalize(media)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| media.to_string());
    let media = media.as_str();
    let mut proj = load(path)?;
    let kind = match p.str("track").unwrap_or("video") {
        "video" | "v" => TrackKind::Video,
        "audio" | "a" => TrackKind::Audio,
        "overlay" | "pip" | "v2" => TrackKind::Overlay,
        other => bail!("--track wants video, overlay or audio, got {other:?}"),
    };

    // Default the length to the whole source, which needs a probe.
    let src_duration = crate::video::decoder::probe(media).map(|i| i.duration).unwrap_or(0.0);
    let in_point = p.num::<f64>("in")?.unwrap_or(0.0);
    let duration = match p.num::<f64>("duration")? {
        Some(d) => d,
        None => (src_duration - in_point).max(0.0),
    };
    if duration <= 0.0 {
        bail!("could not work out a duration for {media} — pass --duration");
    }
    // Default position: after whatever is already on that track.
    let at = match p.num::<f64>("at")? {
        Some(t) => t,
        None => proj
            .tracks
            .iter()
            .filter(|t| t.kind == kind)
            .flat_map(|t| t.clips.iter())
            .map(|c| c.end())
            .fold(0.0, f64::max),
    };

    let id = proj.add_clip(media, kind, at, in_point, duration);
    save(&proj, path)?;
    Ok(Output::new(
        format!("Added clip {id} at {at:.2}s ({duration:.2}s of {media})"),
        serde_json::json!({ "clip": id, "start": at, "duration": duration, "in_point": in_point }),
    ))
}

fn cmd_split(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let at: f64 = p.need_num("at")?;
    let mut proj = load(path)?;
    let n = proj.split_at(at);
    save(&proj, path)?;
    Ok(Output::new(
        format!("Split {n} clip(s) at {at:.2}s"),
        serde_json::json!({ "split": n, "at": at }),
    ))
}

fn cmd_trim(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let (in_pt, dur, start) = (
        p.num::<f64>("in")?,
        p.num::<f64>("duration")?,
        p.num::<f64>("start")?,
    );
    if in_pt.is_none() && dur.is_none() && start.is_none() {
        bail!("nothing to change — pass --in, --duration or --start");
    }
    let mut proj = load(path)?;
    {
        let c = find_clip_mut(&mut proj, id)?;
        if let Some(v) = in_pt {
            c.in_point = v.max(0.0);
        }
        if let Some(v) = dur {
            if v <= 0.0 {
                bail!("--duration must be greater than zero");
            }
            c.duration = v;
        }
        if let Some(v) = start {
            c.start = v.max(0.0);
        }
    }
    let snapshot = proj
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().map(move |c| clip_json(c, t.kind)))
        .find(|c| c["id"] == id);
    save(&proj, path)?;
    Ok(Output::new(
        format!("Trimmed clip {id}"),
        serde_json::json!({ "clip": snapshot }),
    ))
}

fn cmd_move(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let to: f64 = p.need_num("to")?;
    let mut proj = load(path)?;
    find_clip_mut(&mut proj, id)?.start = to.max(0.0);
    for t in &mut proj.tracks {
        t.clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!("Moved clip {id} to {to:.2}s"),
        serde_json::json!({ "clip": id, "start": to }),
    ))
}

enum Nudge {
    Roll,
    Slip,
    Slide,
}

fn cmd_nudge(p: &Parsed, which: Nudge) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let by: f64 = p.need_num("by")?;
    let mut proj = load(path)?;
    if proj.clip(id).is_none() {
        bail!("no clip with id {id} — run `reel inspect` to see the ids");
    }
    let (name, applied) = match which {
        Nudge::Roll => ("Rolled", proj.roll(id, by)),
        Nudge::Slip => ("Slipped", proj.slip(id, by)),
        Nudge::Slide => ("Slid", proj.slide(id, by)),
    };
    if applied.abs() < 1e-9 {
        bail!(
            "nothing moved — {}",
            match which {
                Nudge::Roll => "a roll needs a touching clip on the left, and room on both sides",
                Nudge::Slip => "the window is already at the source's start",
                Nudge::Slide => "a slide needs touching clips on BOTH sides",
            }
        );
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!("{name} clip {id} by {applied:.3}s"),
        serde_json::json!({ "clip": id, "applied": applied }),
    ))
}

fn cmd_remove(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let mut proj = load(path)?;
    let closed = if p.on("ripple") {
        let secs = proj.ripple_delete(id);
        if secs <= 0.0 {
            bail!("no clip with id {id}");
        }
        secs
    } else {
        if !proj.delete_clip(id) {
            bail!("no clip with id {id}");
        }
        0.0
    };
    save(&proj, path)?;
    Ok(Output::new(
        format!("Removed clip {id}{}", if closed > 0.0 { format!(", closed {closed:.2}s") } else { String::new() }),
        serde_json::json!({ "clip": id, "closed": closed }),
    ))
}

fn cmd_gap(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let mut proj = load(path)?;
    let closed = proj.close_all_gaps();
    save(&proj, path)?;
    Ok(Output::new(
        format!("Closed {closed:.2}s of gaps"),
        serde_json::json!({ "closed": closed }),
    ))
}

fn cmd_gain(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let db: f32 = p.need_num("db")?;
    let mut proj = load(path)?;
    find_clip_mut(&mut proj, id)?.gain_db = db;
    save(&proj, path)?;
    Ok(Output::new(
        format!("Clip {id} at {db:+.1} dB"),
        serde_json::json!({ "clip": id, "gain_db": db }),
    ))
}

fn cmd_effects(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let mut proj = load(path)?;
    let fx = {
        let c = find_clip_mut(&mut proj, id)?;
        if p.on("reset") {
            c.effects = crate::effects::Effects::default();
        }
        if let Some(v) = p.num::<f32>("exposure")? {
            c.effects.exposure = v;
        }
        if let Some(v) = p.num::<f32>("contrast")? {
            c.effects.contrast = v;
        }
        if let Some(v) = p.num::<f32>("saturation")? {
            c.effects.saturation = v;
        }
        if let Some(v) = p.num::<f32>("fade-in")? {
            c.effects.fade_in = v as f64;
        }
        if let Some(v) = p.num::<f32>("fade-out")? {
            c.effects.fade_out = v as f64;
        }
        if let Some(v) = p.num::<f32>("zoom")? {
            c.effects.zoom = v;
        }
        if let Some(v) = p.num::<f32>("pan-x")? {
            c.effects.pan_x = v;
        }
        if let Some(v) = p.num::<f32>("pan-y")? {
            c.effects.pan_y = v;
        }
        if let Some(hex) = p.str("key-color") {
            let rgb = parse_hex(hex)?;
            c.effects.key_color =
                Some([rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0]);
        }
        if let Some(v) = p.num::<f32>("key-similarity")? {
            c.effects.key_similarity = v.clamp(0.0, 1.0);
        }
        if let Some(v) = p.num::<f32>("key-softness")? {
            c.effects.key_softness = v.clamp(0.0, 1.0);
        }
        if p.on("key-off") {
            c.effects.key_color = None;
        }
        c.effects
    };
    save(&proj, path)?;
    Ok(Output::new(
        format!("Updated the look of clip {id}"),
        serde_json::json!({ "clip": id, "effects": fx }),
    ))
}

fn cmd_speed(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let rate: f32 = p.need_num("rate")?;
    if !(0.05..=20.0).contains(&rate) {
        bail!("--rate must be between 0.05 and 20");
    }
    let mut proj = load(path)?;
    let (duration, source_len) = {
        let c = find_clip_mut(&mut proj, id)?;
        let old_source = c.source_len();
        c.speed = rate;
        // By default the clip keeps the same footage and its timeline slot
        // grows or shrinks — which is what "make this bit faster" means.
        // --keep-length instead holds the slot and takes more source.
        if !p.on("keep-length") {
            c.duration = old_source / rate.max(0.01) as f64;
        }
        (c.duration, c.source_len())
    };
    save(&proj, path)?;
    Ok(Output::new(
        format!("Clip {id} at {rate}× — {duration:.2}s on the timeline, {source_len:.2}s of source"),
        serde_json::json!({ "clip": id, "speed": rate, "duration": duration, "source_len": source_len }),
    ))
}

fn cmd_keyframe(p: &Parsed) -> Result<Output> {
    use crate::edit::{Interp, Param};
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let mut proj = load(path)?;
    let clip_start = proj
        .clip(id)
        .map(|c| c.start)
        .ok_or_else(|| anyhow!("no clip with id {id} — run `reel inspect` to see the ids"))?;

    if p.on("list") {
        let c = proj.clip(id).unwrap();
        let mut rows = Vec::new();
        for (param, keys) in &c.keys {
            for k in keys {
                rows.push(serde_json::json!({
                    "param": param.name(),
                    "at": clip_start + k.t,
                    "clip_time": k.t,
                    "value": k.value,
                    "interp": format!("{:?}", k.interp).to_lowercase(),
                }));
            }
        }
        let text = rows
            .iter()
            .map(|r| {
                format!(
                    "  {} @ {:.2}s = {}  ({})",
                    r["param"].as_str().unwrap_or(""),
                    r["at"].as_f64().unwrap_or(0.0),
                    r["value"],
                    r["interp"].as_str().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(Output::new(text, serde_json::json!({ "clip": id, "keyframes": rows })));
    }

    let param = p
        .str("param")
        .and_then(Param::parse)
        .ok_or_else(|| {
            let names: Vec<&str> = Param::ALL.iter().map(|q| q.name()).collect();
            anyhow!("--param must be one of: {}", names.join(", "))
        })?;
    let at: f64 = p.need_num("at")?;
    let local = at - clip_start;
    if local < -1e-6 {
        bail!("--at {at:.2}s is before the clip (it starts at {clip_start:.2}s)");
    }

    if p.on("remove") {
        let c = proj.clip_mut(id).unwrap();
        if !c.clear_key(param, local) {
            bail!("no {} keyframe near {at:.2}s", param.name());
        }
        save(&proj, path)?;
        return Ok(Output::new(
            format!("Removed the {} keyframe near {at:.2}s", param.name()),
            serde_json::json!({ "clip": id, "param": param.name(), "removed_at": at }),
        ));
    }

    let value: f32 = p.need_num("value")?;
    let interp = match p.str("interp").unwrap_or("linear") {
        "linear" => Interp::Linear,
        "hold" | "step" => Interp::Hold,
        "ease" | "smooth" => Interp::Ease,
        other => bail!("--interp wants linear, hold or ease — got {other:?}"),
    };
    let c = proj.clip_mut(id).unwrap();
    c.set_key(param, local, value, interp);
    let n = c.key_track(param).map(|t| t.len()).unwrap_or(0);
    save(&proj, path)?;
    Ok(Output::new(
        format!("{} = {value} at {at:.2}s ({n} key(s) on the track)", param.name()),
        serde_json::json!({ "clip": id, "param": param.name(), "at": at, "value": value, "keys": n }),
    ))
}

fn cmd_pip(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let mut proj = load(path)?;
    let on_overlay = proj
        .tracks
        .iter()
        .any(|t| t.kind == TrackKind::Overlay && t.clips.iter().any(|c| c.id == id));
    if !on_overlay {
        bail!("clip {id} isn't on an overlay track — add it with `reel add … --track overlay`");
    }
    let pip = {
        let c = find_clip_mut(&mut proj, id)?;
        if let Some(v) = p.num::<f32>("x")? {
            c.pip.x = v;
        }
        if let Some(v) = p.num::<f32>("y")? {
            c.pip.y = v;
        }
        if let Some(v) = p.num::<f32>("scale")? {
            if !(0.02..=1.0).contains(&v) {
                bail!("--scale is a fraction of the frame, between 0.02 and 1.0");
            }
            c.pip.scale = v;
        }
        c.pip
    };
    save(&proj, path)?;
    Ok(Output::new(
        format!("Overlay {id} at ({:.2}, {:.2}), {:.0}% wide", pip.x, pip.y, pip.scale * 100.0),
        serde_json::json!({ "clip": id, "pip": pip }),
    ))
}

fn cmd_transition(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let secs: f64 = p.need_num("seconds")?;
    if secs < 0.0 {
        bail!("--seconds cannot be negative");
    }
    let mut proj = load(path)?;
    let kind = match p.str("kind") {
        Some(k) => crate::edit::TransitionKind::parse(k).ok_or_else(|| {
            let names: Vec<&str> =
                crate::edit::TransitionKind::ALL.iter().map(|k| k.name()).collect();
            anyhow!("--kind must be one of: {}", names.join(", "))
        })?,
        None => proj.clip(id).map(|c| c.transition_kind).unwrap_or_default(),
    };
    {
        let c = find_clip_mut(&mut proj, id)?;
        c.transition_in = secs;
        c.transition_kind = kind;
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!("Clip {id}: {} over {secs:.2}s", kind.label()),
        serde_json::json!({ "clip": id, "transition_in": secs, "kind": kind.name() }),
    ))
}

fn parse_hex(s: &str) -> Result<[u8; 3]> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        bail!("--color wants six hex digits, e.g. ffcc00");
    }
    let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| anyhow!("bad hex in --color"));
    Ok([byte(0)?, byte(2)?, byte(4)?])
}

fn cmd_title(p: &Parsed) -> Result<Output> {
    let action = p.at(0)?.to_string();
    let path = p.at(1)?;
    let mut proj = load(path)?;
    match action.as_str() {
        "add" => {
            let text = p.str("text").ok_or_else(|| anyhow!("--text is required"))?;
            let at = p.num::<f64>("at")?.unwrap_or(0.0);
            let dur = p.num::<f64>("duration")?.unwrap_or(3.0);
            let t = crate::titles::Title {
                text: text.to_string(),
                start: at,
                end: at + dur.max(0.05),
                x: p.num::<f32>("x")?.unwrap_or(0.5),
                y: p.num::<f32>("y")?.unwrap_or(0.5),
                size: p.num::<f32>("size")?.unwrap_or(0.09),
                color: match p.str("color") {
                    Some(c) => parse_hex(c)?,
                    None => [255, 255, 255],
                },
                bold: !p.on("no-bold"),
                outline: !p.on("no-outline"),
            };
            proj.titles.push(t.clone());
            let index = proj.titles.len() - 1;
            save(&proj, path)?;
            Ok(Output::new(
                format!("Added title {index}: {:?} at {at:.2}s", t.text),
                serde_json::json!({ "index": index, "title": t }),
            ))
        }
        "list" => {
            let text = proj
                .titles
                .iter()
                .enumerate()
                .map(|(i, t)| format!("  [{i}] {:?} {:.2}s–{:.2}s", t.text, t.start, t.end))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Output::new(text, serde_json::json!({ "titles": proj.titles })))
        }
        "remove" => {
            let i: usize = p.need_num("index")?;
            if i >= proj.titles.len() {
                bail!("no title at index {i} ({} titles)", proj.titles.len());
            }
            let gone = proj.titles.remove(i);
            save(&proj, path)?;
            Ok(Output::new(
                format!("Removed title {i}: {:?}", gone.text),
                serde_json::json!({ "removed": gone }),
            ))
        }
        other => bail!("title ACTION is add, list or remove — got {other:?}"),
    }
}

fn cmd_music(p: &Parsed) -> Result<Output> {
    let action = p.at(0)?.to_string();
    let path = p.at(1)?;
    let mut proj = load(path)?;
    match action.as_str() {
        "set" => {
            let audio = p.at(2).map_err(|_| anyhow!("`reel music set` needs an audio file"))?;
            if !std::path::Path::new(audio).exists() {
                bail!("no such audio file: {audio}");
            }
            let audio = &std::fs::canonicalize(audio)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| audio.to_string());
            let m = Music {
                source: audio.to_string(),
                start: 0.0,
                gain_db: p.num::<f32>("gain-db")?.unwrap_or(-12.0),
                duck: !p.on("no-duck"),
                fade: p.num::<f64>("fade")?.unwrap_or(1.0),
            };
            proj.music = Some(m.clone());
            save(&proj, path)?;
            Ok(Output::new(
                format!(
                    "Music bed set: {audio} at {:+.1} dB{}",
                    m.gain_db,
                    if m.duck { ", ducking under speech" } else { "" }
                ),
                serde_json::json!({ "music": m }),
            ))
        }
        "clear" => {
            proj.music = None;
            save(&proj, path)?;
            Ok(Output::new("Music bed removed", serde_json::json!({ "music": null })))
        }
        other => bail!("music ACTION is set or clear — got {other:?}"),
    }
}

fn cmd_marker(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let mut proj = load(path)?;
    if p.on("list") {
        let text = proj
            .markers
            .iter()
            .map(|m| format!("  {m:.2}s"))
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(Output::new(text, serde_json::json!({ "markers": proj.markers })));
    }
    let at: f64 = p.need_num("at")?;
    if p.on("remove") {
        let before = proj.markers.len();
        proj.markers.retain(|m| (m - at).abs() > 0.05);
        if proj.markers.len() == before {
            bail!("no marker near {at:.2}s");
        }
    } else {
        proj.markers.push(at);
        proj.markers.sort_by(|a, b| a.total_cmp(b));
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!("{} marker at {at:.2}s", if p.on("remove") { "Removed" } else { "Added" }),
        serde_json::json!({ "markers": proj.markers }),
    ))
}

fn cmd_align(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let id: u64 = p.need_num("clip")?;
    let to: u64 = p.need_num("to")?;
    let window = p.num::<f64>("window")?.unwrap_or(90.0).clamp(1.0, 600.0);
    let mut proj = load(path)?;
    let (b, _) = proj.clip_with_kind(id).ok_or_else(|| anyhow!("no clip with id {id}"))?;
    let (a, _) = proj.clip_with_kind(to).ok_or_else(|| anyhow!("no clip with id {to}"))?;

    let pa = crate::waveform::compute(&a.source)
        .ok_or_else(|| anyhow!("no audio in {} to sync against", a.source))?;
    let pb = crate::waveform::compute(&b.source)
        .ok_or_else(|| anyhow!("no audio in {} to sync", b.source))?;
    let max_lag = (window * crate::waveform::BUCKETS_PER_SEC) as usize;
    let (lag, score) = crate::waveform::best_lag(&pa.data, &pb.data, max_lag)
        .ok_or_else(|| anyhow!("not enough audio to correlate"))?;
    if score < 0.35 {
        bail!(
            "no confident match (correlation {score:.2}) — are these recordings of the same moment?"
        );
    }
    // b[i] ≈ a[i + lag]: a moment at B-source time u sits at A-source time
    // u + lag. Place B so the two land on the same timeline instant.
    let lag_secs = lag as f64 / crate::waveform::BUCKETS_PER_SEC;
    let new_start = a.start - a.in_point + b.in_point + lag_secs;
    if new_start < -1e-9 {
        bail!(
            "aligning would place the clip at {new_start:.2}s — trim its head by that much first"
        );
    }
    if let Some(c) = proj.clip_mut(id) {
        c.start = new_start.max(0.0);
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!(
            "Aligned clip {id} to {to}: moved to {new_start:.3}s (offset {lag_secs:+.3}s, correlation {score:.2})"
        ),
        serde_json::json!({ "clip": id, "start": new_start, "offset": lag_secs, "score": score }),
    ))
}

fn cmd_tighten(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let threshold = p.num::<f32>("threshold")?.unwrap_or(0.06).clamp(0.001, 0.9);
    let min_gap = p.num::<f64>("min-gap")?.unwrap_or(0.6).max(0.1);
    let pad = p.num::<f64>("pad")?.unwrap_or(0.15).max(0.0);
    let mut proj = load(path)?;
    let before = proj.duration();
    let mut cache: HashMap<String, Option<(Vec<f32>, f64)>> = HashMap::new();
    let mut supplier = |src: &str| -> Option<(Vec<f32>, f64)> {
        cache
            .entry(src.to_string())
            .or_insert_with(|| {
                crate::waveform::compute(src)
                    .map(|p| (p.data, crate::waveform::BUCKETS_PER_SEC))
            })
            .clone()
    };
    let (cuts, removed) = proj.tighten(&mut supplier, threshold, min_gap, pad);
    if cuts == 0 {
        return Ok(Output::new(
            "Nothing to tighten — no silences matched.",
            serde_json::json!({ "cuts": 0, "removed": 0.0 }),
        ));
    }
    save(&proj, path)?;
    Ok(Output::new(
        format!(
            "Tightened: {cuts} silence(s), {removed:.2}s removed ({before:.2}s → {:.2}s)",
            proj.duration()
        ),
        serde_json::json!({ "cuts": cuts, "removed": removed, "duration": proj.duration() }),
    ))
}

fn cmd_captions(p: &Parsed) -> Result<Output> {
    let target = p.at(0)?.to_string();
    let quiet = p.on("quiet") || p.on("json");
    let model = match p.str("model").unwrap_or("base") {
        "tiny" => crate::captions::Model::TinyEn,
        "base" => crate::captions::Model::BaseEn,
        "small" => crate::captions::Model::SmallEn,
        other => bail!("--model wants tiny, base or small — got {other:?}"),
    };

    // A project caption run transcribes a clip and maps the cues through the
    // edit; a bare media file just gets transcribed.
    let is_project = target.ends_with(".reel");
    let mut proj = if is_project { Some(load(&target)?) } else { None };
    let source = match p.str("source") {
        Some(s) => s.to_string(),
        None => match &proj {
            Some(pr) => pr
                .tracks
                .iter()
                .flat_map(|t| t.clips.iter())
                .next()
                .map(|c| c.source.clone())
                .ok_or_else(|| anyhow!("the project has no clips — add one, or pass --source"))?,
            None => target.clone(),
        },
    };

    let job = crate::captions::start(&source, model);
    let mut last = String::new();
    let cues = loop {
        let st = job.state();
        if !quiet && st.stage != last {
            eprintln!("{}", st.stage);
            last = st.stage.clone();
        }
        if st.finished {
            if let Some(e) = st.error {
                bail!("{e}");
            }
            break st.cues;
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    };

    if let Some(srt) = p.str("srt") {
        std::fs::write(srt, crate::captions::to_srt(&cues))
            .with_context(|| format!("could not write {srt}"))?;
    }

    let applied = match proj.as_mut() {
        Some(pr) => {
            let mut mapped = Vec::new();
            for cue in &cues {
                for (start, end) in pr.map_source_window(&source, cue.start, cue.end) {
                    mapped.push(crate::captions::Cue { start, end, text: cue.text.clone() });
                }
            }
            mapped.sort_by(|a, b| a.start.total_cmp(&b.start));
            let n = mapped.len();
            pr.captions = mapped;
            if let Some(sz) = p.num::<u32>("size")? {
                pr.caption_size = sz;
            }
            save(pr, &target)?;
            n
        }
        None => 0,
    };

    Ok(Output::new(
        if is_project {
            format!("{applied} caption(s) written into {target}")
        } else {
            format!("{} caption(s) transcribed", cues.len())
        },
        serde_json::json!({ "cues": cues, "applied": applied, "source": source }),
    ))
}

// ── Rendering ────────────────────────────────────────────────────────────

fn settings_from(p: &Parsed, default_codec: Codec) -> Result<export::ExportSettings> {
    let mut s = export::ExportSettings {
        codec: default_codec,
        quality: Quality::Balanced,
        resolution: Resolution::Source,
        audio: AudioMode::Encode { kbps: 160 },
        hardware: !p.on("no-hardware"),
        target: None,
        fit: Fit::Letterbox,
        loudness: None,
    };
    if let Some(name) = p.str("preset") {
        let found = export::Preset::ALL
            .iter()
            .find(|pr| pr.name.eq_ignore_ascii_case(name) || pr.name.to_lowercase().replace([' ', '/'], "") == name.to_lowercase().replace([' ', '/', '-'], ""))
            .ok_or_else(|| {
                let names: Vec<&str> = export::Preset::ALL.iter().map(|p| p.name).collect();
                anyhow!("unknown preset {name:?}. Try one of: {}", names.join(", "))
            })?;
        s.target = Some((found.w, found.h));
        s.fit = found.fit;
        s.codec = found.codec;
        s.quality = found.quality;
        s.loudness = found.loudness;
    }
    if let Some(c) = p.str("codec") {
        s.codec = match c.to_lowercase().as_str() {
            "h264" | "x264" | "avc" => Codec::H264,
            "h265" | "hevc" | "x265" => Codec::H265,
            "av1" => Codec::Av1,
            "vp9" | "webm" => Codec::Vp9,
            "remux" | "copy" => Codec::Remux,
            "mp3" => Codec::Mp3,
            "m4a" | "aac" => Codec::M4a,
            "opus" => Codec::OpusAudio,
            "flac" => Codec::Flac,
            "wav" => Codec::Wav,
            "png" => Codec::Png,
            "jpeg" | "jpg" => Codec::Jpeg,
            "webp" => Codec::WebpImage,
            other => bail!("unknown codec {other:?}"),
        };
    }
    if let Some(q) = p.str("quality") {
        s.quality = match q.to_lowercase().as_str() {
            "high" | "best" => Quality::High,
            "balanced" | "medium" => Quality::Balanced,
            "small" | "low" => Quality::Small,
            n => Quality::Custom(
                n.parse::<u8>().map_err(|_| anyhow!("--quality wants high, balanced, small or a CRF number"))?,
            ),
        };
    }
    if let Some(r) = p.str("resolution") {
        s.resolution = match r.to_lowercase().trim_end_matches('p') {
            "source" | "native" => Resolution::Source,
            "2160" | "4k" => Resolution::H2160,
            "1080" => Resolution::H1080,
            "720" => Resolution::H720,
            "480" => Resolution::H480,
            other => bail!("--resolution wants source, 2160, 1080, 720 or 480 — got {other:?}"),
        };
    }
    if let Some(f) = p.str("fit") {
        s.fit = match f.to_lowercase().as_str() {
            "letterbox" | "fit" | "bars" => Fit::Letterbox,
            "crop" | "fill" => Fit::Crop,
            "blur" | "blurred" => Fit::Blur,
            other => bail!("--fit wants letterbox, crop or blur — got {other:?}"),
        };
    }
    if let Some(l) = p.num::<f32>("loudness")? {
        s.loudness = Some(l);
    }
    if let Some(a) = p.str("audio") {
        s.audio = match a.to_lowercase().as_str() {
            "copy" | "keep" | "passthrough" => AudioMode::Copy,
            "encode" | "convert" | "aac" => AudioMode::Encode { kbps: 160 },
            other => bail!("--audio wants copy or encode — got {other:?}"),
        };
    }
    Ok(s)
}

/// Drive an export job to completion, reporting progress on stderr so stdout
/// stays clean for the result.
fn await_job(job: export::ExportJob, quiet: bool) -> Result<()> {
    let mut last = -1i32;
    loop {
        let st = job.state();
        if st.finished {
            if let Some(e) = st.error {
                bail!("{e}");
            }
            if !quiet {
                eprintln!("100%");
            }
            return Ok(());
        }
        let pct = (st.fraction * 100.0) as i32;
        if !quiet && pct != last && pct % 5 == 0 {
            eprintln!("{pct}%{}", if st.speed > 0.0 { format!("  ({:.1}× realtime)", st.speed) } else { String::new() });
            last = pct;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn prepare_output(path: &str, overwrite: bool) -> Result<()> {
    if std::path::Path::new(path).exists() {
        if !overwrite {
            bail!("{path} already exists — pass --overwrite to replace it");
        }
        std::fs::remove_file(path).with_context(|| format!("could not replace {path}"))?;
    }
    Ok(())
}

fn cmd_frame(p: &Parsed) -> Result<Output> {
    let target = p.at(0)?;
    let at = p.num::<f64>("at")?.unwrap_or(0.0);
    let out = p
        .str("out")
        .map(String::from)
        .unwrap_or_else(|| format!("{}.{at:.2}s.png", target.trim_end_matches(".reel")));
    prepare_output(&out, p.on("overwrite"))?;

    if target.ends_with(".reel") {
        let proj = load(target)?;
        let segments = proj.export_segments();
        if segments.is_empty() {
            bail!("the timeline is empty");
        }
        let settings = export::ExportSettings {
            codec: Codec::H264,
            quality: Quality::High,
            resolution: Resolution::Source,
            audio: AudioMode::Encode { kbps: 160 },
            hardware: false,
            target: None,
            fit: Fit::Letterbox,
            loudness: None,
        };
        let overlays = export::Overlays {
            captions: &proj.captions,
            caption_size: proj.caption_size,
            titles: &proj.titles,
            music: None,
            overlays: &proj.overlay_segments(),
            markers: &[],
        };
        let (rgba, w, h) = crate::engine::render::render_still(
            &segments,
            &overlays,
            (proj.width, proj.height, proj.fps),
            &settings,
            at,
        )?;
        image::save_buffer(&out, &rgba, w, h, image::ColorType::Rgba8)
            .with_context(|| format!("could not write {out}"))?;
        // Titles/captions burn through libass at encode time in a full
        // render; for a still, burn them onto the PNG via one ffmpeg pass.
        // (Simplest honest approach: the still already has picture layers;
        // text overlays follow in a later pass.)
        Ok(Output::new(
            format!("Wrote {out} — the edit at {at:.2}s ({w}×{h})"),
            serde_json::json!({ "output": out, "at": at, "width": w, "height": h }),
        ))
    } else {
        if !std::path::Path::new(target).exists() {
            bail!("no such file: {target}");
        }
        let ok = std::process::Command::new("ffmpeg")
            .args([
                "-y", "-v", "error", "-ss", &format!("{at}"), "-i", target,
                "-frames:v", "1", &out,
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            bail!("could not extract a frame at {at:.2}s");
        }
        Ok(Output::new(
            format!("Wrote {out}"),
            serde_json::json!({ "output": out, "at": at }),
        ))
    }
}

fn cmd_render(p: &Parsed) -> Result<Output> {
    let path = p.at(0)?;
    let out = p.at(1)?;
    let proj = load(path)?;
    let segments = proj.export_segments();
    if segments.is_empty() {
        bail!("the timeline is empty — add a clip first");
    }
    let settings = settings_from(p, Codec::H264)?;
    prepare_output(out, p.on("overwrite"))?;

    let job = export::start_timeline_with_captions(
        &segments,
        out,
        &settings,
        (proj.width, proj.height, proj.fps),
        export::Overlays {
            captions: &proj.captions,
            caption_size: proj.caption_size,
            titles: &proj.titles,
            music: proj.music.as_ref(),
            overlays: &proj.overlay_segments(),
            markers: &proj.markers,
        },
    )?;
    await_job(job, p.on("quiet") || p.on("json"))?;
    let duration = crate::edit::render_duration(&segments);
    Ok(Output::new(
        format!("Rendered {out} — {:.2}s from {} clip(s)", duration, segments.len()),
        serde_json::json!({
            "output": out,
            "duration": duration,
            "clips": segments.len(),
            "captions": proj.captions.len(),
            "titles": proj.titles.len(),
        }),
    ))
}

fn cmd_convert(p: &Parsed) -> Result<Output> {
    let input = p.at(0)?;
    let out = p.at(1)?;
    if !std::path::Path::new(input).exists() {
        bail!("no such file: {input}");
    }
    let duration = crate::video::decoder::probe(input).map(|i| i.duration).unwrap_or(0.0);
    let settings = settings_from(p, Codec::H264)?;
    prepare_output(out, p.on("overwrite"))?;
    let job = export::start(input, out, &settings, duration)?;
    await_job(job, p.on("quiet") || p.on("json"))?;
    Ok(Output::new(
        format!("Wrote {out}"),
        serde_json::json!({ "output": out, "input": input, "duration": duration }),
    ))
}

fn cmd_presets() -> Result<Output> {
    let text = export::Preset::ALL
        .iter()
        .map(|p| format!("  {:16} {}", p.name, p.note))
        .collect::<Vec<_>>()
        .join("\n");
    let data: Vec<serde_json::Value> = export::Preset::ALL
        .iter()
        .map(|p| {
            serde_json::json!({
                "name": p.name,
                "note": p.note,
                "width": p.w,
                "height": p.h,
            })
        })
        .collect();
    Ok(Output::new(text, serde_json::json!({ "presets": data })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_for(name: &str, args: &[&str]) -> Result<Parsed> {
        let cmd = COMMANDS.iter().find(|c| c.name == name).unwrap();
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(cmd, &owned)
    }

    #[test]
    fn flags_parse_in_both_spellings() {
        let p = parse_for("add", &["p.reel", "a.mp4", "--at", "3", "--duration=2.5"]).unwrap();
        assert_eq!(p.positional, vec!["p.reel", "a.mp4"]);
        assert_eq!(p.num::<f64>("at").unwrap(), Some(3.0));
        assert_eq!(p.num::<f64>("duration").unwrap(), Some(2.5));
    }

    #[test]
    fn bad_input_is_refused_rather_than_guessed() {
        // An unknown flag is a typo, and silently ignoring it would render
        // something other than what was asked for.
        let e = parse_for("add", &["p.reel", "a.mp4", "--start", "3"]).unwrap_err();
        assert!(e.to_string().contains("unknown flag --start"), "{e}");

        // A missing positional names what it wanted.
        let e = parse_for("add", &["p.reel"]).unwrap_err();
        assert!(e.to_string().contains("MEDIA"), "{e}");

        // A value flag with nothing after it.
        let e = parse_for("add", &["p.reel", "a.mp4", "--at"]).unwrap_err();
        assert!(e.to_string().contains("needs a value"), "{e}");

        // A switch given a value.
        let e = parse_for("remove", &["p.reel", "--ripple=yes"]).unwrap_err();
        assert!(e.to_string().contains("takes no value"), "{e}");

        // Numbers are validated, not silently zeroed.
        let p = parse_for("split", &["p.reel", "--at", "abc"]).unwrap();
        assert!(p.num::<f64>("at").is_err());
    }

    /// A file is not a command, even when it is named like one — otherwise
    /// double-clicking a video called "render.mp4" would print help.
    #[test]
    fn an_existing_file_always_wins_over_a_verb_name() {
        assert!(is_command("render"));
        let dir = std::env::temp_dir().join(format!("reel-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("render");
        std::fs::write(&f, b"x").unwrap();
        assert!(!is_command(&f.to_string_lossy()));
        let _ = std::fs::remove_file(&f);
        assert!(!is_command("definitely-not-a-command"));
    }

    /// The manual is generated from the parser's own table, so every command
    /// it advertises must actually dispatch.
    #[test]
    fn every_documented_command_is_implemented() {
        for c in COMMANDS {
            let p = Parsed {
                positional: Vec::new(),
                values: HashMap::new(),
                switches: HashSet::new(),
            };
            let e = dispatch(c.name, &p);
            if let Err(e) = e {
                assert!(
                    !e.to_string().starts_with("unimplemented"),
                    "`reel {}` is documented but not implemented",
                    c.name
                );
            }
        }
        // And no command is missing its help text or duplicated.
        let mut seen = HashSet::new();
        for c in COMMANDS {
            assert!(!c.help.is_empty(), "{} has no help", c.name);
            assert!(seen.insert(c.name), "{} is listed twice", c.name);
        }
    }

    #[test]
    fn presets_and_codecs_resolve_by_the_names_people_type() {
        let p = parse_for("render", &["p.reel", "o.mp4", "--preset", "tiktok"]).unwrap();
        let s = settings_from(&p, Codec::H264).unwrap();
        assert_eq!(s.target, Some((1080, 1920)));

        let p = parse_for("convert", &["a.mp4", "b.webm", "--codec", "vp9", "--quality", "18"]).unwrap();
        let s = settings_from(&p, Codec::H264).unwrap();
        assert_eq!(s.codec, Codec::Vp9);
        assert_eq!(s.quality, Quality::Custom(18));

        let p = parse_for("convert", &["a.mp4", "b.mp4", "--preset", "nope"]).unwrap();
        assert!(settings_from(&p, Codec::H264).is_err());
    }

    /// The whole CLI exists so a machine can drive Reel. That breaks the
    /// moment a command can block on a window, so every verb must be
    /// answerable without a display — and anything that ISN'T a verb must be
    /// refused rather than falling through to the GUI (a mistyped command
    /// used to open a window and hang forever on a headless box).
    #[test]
    fn nothing_here_can_fall_through_to_a_window() {
        for c in COMMANDS {
            assert!(is_command(c.name), "`{}` is not routed to the CLI", c.name);
        }
        for typo in ["rendr", "bogus", "--nope", "inspec"] {
            assert!(!is_command(typo), "{typo:?} must not be treated as a command");
        }
    }

    /// The docs are for agents, so a command that exists but is undocumented
    /// is worse than useless — it's a capability nobody can find. Adding a
    /// command therefore has to mean adding it to the reference.
    #[test]
    fn every_command_appears_in_the_written_docs() {
        let docs = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/CLI.md"))
            .expect("docs/CLI.md should exist");
        for c in COMMANDS {
            assert!(
                docs.contains(&format!("### `reel {}", c.name)),
                "`reel {}` is missing from docs/CLI.md — regenerate it",
                c.name
            );
        }
    }

    #[test]
    fn colours_are_read_as_hex() {
        assert_eq!(parse_hex("ffcc00").unwrap(), [255, 204, 0]);
        assert_eq!(parse_hex("#000000").unwrap(), [0, 0, 0]);
        assert!(parse_hex("fff").is_err());
        assert!(parse_hex("gggggg").is_err());
    }
}
