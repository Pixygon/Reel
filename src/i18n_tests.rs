//! The localisation guard rails. These fail the build when:
//! * a key exists in one language and not the other,
//! * code asks for a key no resource file defines,
//! * a placeholder in English is missing from (or invented in) bokmål,
//! * a UI file grows a hard-coded human-readable string literal.

use super::*;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Files that are ALL user interface: every human-readable literal in them
/// must come from the resource files.
const UI_FILES: &[&str] = &[
    "src/ui.rs",
    "src/app.rs",
    "src/tray.rs",
    "src/integration.rs",
    "src/macos.rs",
];

/// Files whose ERROR messages reach the status line / dialogs: their
/// `bail!`/`anyhow!`/`.context(` text must be localised too (ffmpeg arguments
/// and the like elsewhere in them are not UI). `cli.rs` is the scripted
/// interface and stays English (docs/CLI.md is generated from it).
const ERROR_FILES: &[&str] = &[
    "src/audio.rs",
    "src/beats.rs",
    "src/captions.rs",
    "src/capture.rs",
    "src/compound.rs",
    "src/edit/mod.rs",
    "src/edit/variants.rs",
    "src/engine/render.rs",
    "src/engine/sources.rs",
    "src/engine/compositor.rs",
    "src/export.rs",
    "src/gpu.rs",
    "src/lut.rs",
    "src/media.rs",
    "src/plates.rs",
    "src/plugins.rs",
    "src/portal.rs",
    "src/proxy.rs",
    "src/titles.rs",
    "src/track.rs",
    "src/video/decoder.rs",
    "src/video/mpv.rs",
];

/// Literals that are allowed verbatim: brand and product names, which are
/// never translated.
const BRAND: &[&str] = &["Reel", "REEL", "Reel — ", "Pixygon", "reel.pixygon.io"];

#[derive(Debug)]
struct Lit {
    line: usize,
    text: String,
    /// Source text of the statement up to the literal (since the last `;`,
    /// `{` or `}`), used to recognise logging, panics, keys and ids.
    lead: String,
}

/// A small Rust lexer: enough to find string literals while skipping
/// comments, char literals, lifetimes and raw strings correctly. Stops at
/// the first `#[cfg(test)]` (test modules sit at the end and are not UI).
/// Lines carrying `// i18n-ok` are skipped.
fn literals(src: &str) -> Vec<Lit> {
    let ok_lines: BTreeSet<usize> =
        src.lines().enumerate().filter(|(_, l)| l.contains("i18n-ok")).map(|(i, _)| i + 1).collect();
    let src = match src.find("\n#[cfg(test)]") {
        Some(i) => &src[..i],
        None => src,
    };
    let b: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut stmt) = (0usize, 1usize, 0usize);
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        match c {
            '\n' => {
                line += 1;
                i += 1;
            }
            '/' if next == Some('/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if next == Some('*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    if b[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                i += 2;
            }
            '\'' => {
                // char literal or lifetime
                if next == Some('\\') {
                    i += 2;
                    while i < b.len() && b[i] != '\'' {
                        i += 1;
                    }
                    i += 1;
                } else if b.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            ';' | '{' | '}' => {
                stmt = i + 1;
                i += 1;
            }
            'r' if (next == Some('"') || next == Some('#'))
                && (i == 0 || !(b[i - 1].is_alphanumeric() || b[i - 1] == '_')) =>
            {
                // raw string r"…" / r#"…"#
                let mut j = i + 1;
                let mut hashes = 0;
                while b.get(j) == Some(&'#') {
                    hashes += 1;
                    j += 1;
                }
                if b.get(j) != Some(&'"') {
                    i += 1;
                    continue;
                }
                let start_line = line;
                j += 1;
                let mut text = String::new();
                loop {
                    if j >= b.len() {
                        break;
                    }
                    if b[j] == '"' && (0..hashes).all(|h| b.get(j + 1 + h) == Some(&'#')) {
                        j += 1 + hashes;
                        break;
                    }
                    if b[j] == '\n' {
                        line += 1;
                    }
                    text.push(b[j]);
                    j += 1;
                }
                let lead: String = b[stmt..i].iter().collect();
                out.push(Lit { line: start_line, text, lead });
                i = j;
            }
            '"' => {
                let start_line = line;
                let byte = i > 0 && b[i - 1] == 'b';
                let mut j = i + 1;
                let mut text = String::new();
                while j < b.len() && b[j] != '"' {
                    if b[j] == '\\' {
                        // line continuation swallows the newline + indent
                        if b.get(j + 1) == Some(&'\n') {
                            line += 1;
                            j += 2;
                            while j < b.len() && b[j].is_whitespace() {
                                j += 1;
                            }
                            continue;
                        }
                        text.push(b[j]);
                        text.push(*b.get(j + 1).unwrap_or(&' '));
                        j += 2;
                        continue;
                    }
                    if b[j] == '\n' {
                        line += 1;
                    }
                    text.push(b[j]);
                    j += 1;
                }
                if !byte {
                    let lead: String = b[stmt..i].iter().collect();
                    out.push(Lit { line: start_line, text, lead });
                }
                i = j + 1;
            }
            _ => i += 1,
        }
    }
    out.retain(|l| !ok_lines.contains(&l.line));
    out
}

/// Does this literal read like words a person would see?
fn is_human(text: &str) -> bool {
    if BRAND.contains(&text) {
        return false;
    }
    // Strip `{…}` format holes and escapes before judging.
    let mut plain = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    let plain = plain.replace("\\n", " ").replace("\\t", " ");
    let words: Vec<&str> = plain
        .split(|c: char| !c.is_alphabetic() && c != '\'')
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .collect();
    if words.is_empty() {
        return false;
    }
    // URLs, paths, file globs, mime types and dotted keys are not prose.
    let t = plain.trim();
    if t.contains("://") || t.starts_with('/') || t.starts_with('.') || t.starts_with('-') {
        return false;
    }
    if !t.contains(' ') {
        // A single token: prose only when it is a Capitalised word or ends
        // like a sentence ("Ready.", "Open…"). snake_case ids, lowercase
        // keys, codecs ("libx264"), extensions are not.
        let first_cap = t.chars().next().is_some_and(|c| c.is_uppercase())
            && t.chars().nth(1).is_some_and(|c| c.is_lowercase());
        let sentence = t.ends_with('…') || t.ends_with('.') && t.len() > 3 && !t.contains('/');
        let ident = t.chars().all(|c| c.is_ascii_alphanumeric() || "_-.:/=%+,[]".contains(c));
        return (first_cap && !t.contains('_') && !t.contains('=')) || (sentence && !ident);
    }
    // Several tokens: prose when two or more are real words. Filter graphs,
    // command lines and format strings rarely have spaces between words.
    if t.contains('=') && t.contains(':') {
        return false;
    }
    words.len() >= 2 || words.iter().any(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// Statements where a literal is not UI: logging, panics, ids, keys,
/// process arguments, environment names.
fn exempt(lead: &str) -> bool {
    const SKIP: &[&str] = &[
        "log::", "eprintln!", "println!", "eprint!", "print!", "debug!", "info!(", "warn!(", "error!(",
        "trace!(", "panic!", "assert", "unreachable!", "expect(", "todo!", "timing!", "tr!(", "trn!(",
        "Id::new(", "id_salt(", "push_id(", ".arg(", ".args(", "env::var", "Command::new(", "env(", "lookup(",
        "debug_assert", "write!(", "writeln!(",
    ];
    // only the last call on the statement line matters for keys: a tr! whose
    // literal is the key
    SKIP.iter().any(|s| lead.contains(s))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn both_languages_have_exactly_the_same_keys() {
    let en: BTreeSet<_> = keys(Lang::En).into_iter().collect();
    let nb: BTreeSet<_> = keys(Lang::Nb).into_iter().collect();
    let only_en: Vec<_> = en.difference(&nb).collect();
    let only_nb: Vec<_> = nb.difference(&en).collect();
    assert!(only_en.is_empty(), "keys missing from locales/nb: {only_en:?}");
    assert!(only_nb.is_empty(), "keys missing from locales/en: {only_nb:?}");
}

fn placeholders(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut rest = s;
    while let Some(o) = rest.find('{') {
        let after = &rest[o + 1..];
        let Some(c) = after.find('}') else { break };
        out.insert(after[..c].to_string());
        rest = &after[c + 1..];
    }
    out
}

#[test]
fn placeholders_agree_between_languages() {
    let mut bad = Vec::new();
    for k in keys(Lang::En) {
        let (e, n) = (lookup(Lang::En, k), lookup(Lang::Nb, k));
        if placeholders(e) != placeholders(n) {
            bad.push(format!("{k}: en {:?} vs nb {:?}", placeholders(e), placeholders(n)));
        }
        if !e.is_empty() && n.is_empty() {
            bad.push(format!("{k}: empty in nb"));
        }
    }
    assert!(bad.is_empty(), "placeholder mismatches:\n{}", bad.join("\n"));
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Every `tr!("key")` / `trn!("key", …)` in the code must exist — in both
/// languages, by the parity test above.
#[test]
fn every_key_used_in_code_is_defined() {
    let mut files = Vec::new();
    rust_files(&root().join("src"), &mut files);
    let en: BTreeSet<_> = keys(Lang::En).into_iter().collect();
    let mut missing = Vec::new();
    for f in files {
        // The i18n module itself shows the macros in its docs and tests.
        if f.file_name().is_some_and(|n| n.to_string_lossy().starts_with("i18n")) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        for (mac, plural) in [("tr!(\"", false), ("trn!(\"", true)] {
            let mut rest = src.as_str();
            while let Some(p) = rest.find(mac) {
                let boundary = rest[..p].chars().next_back().is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                let after = &rest[p + mac.len()..];
                let end = after.find('"').unwrap();
                let key = &after[..end];
                let found = if plural {
                    en.contains(format!("{key}.one").as_str()) && en.contains(format!("{key}.other").as_str())
                } else {
                    en.contains(key)
                };
                if boundary && !found {
                    missing.push(format!("{}: {key}", f.strip_prefix(root()).unwrap().display()));
                }
                rest = &after[end..];
            }
        }
    }
    assert!(missing.is_empty(), "keys used in code but not defined in locales/:\n{}", missing.join("\n"));
}

fn offenders(rel: &str, errors_only: bool) -> Vec<String> {
    let src = read(rel);
    literals(&src)
        .into_iter()
        .filter(|l| {
            if errors_only {
                let lead = l.lead.trim_end();
                let is_err = ["bail!(", "anyhow!(", "context(", "ensure!("].iter().any(|m| lead.contains(m));
                is_err && !lead.contains("tr!(") && is_human(&l.text)
            } else {
                is_human(&l.text) && !exempt(&l.lead)
            }
        })
        .map(|l| format!("{rel}:{}: {:?}", l.line, l.text))
        .collect()
}

/// THE guard against new hard-coded UI text. A legitimately non-UI literal
/// (a technical id, a protocol string) takes a trailing `// i18n-ok` comment
/// saying so; everything a person reads goes through `tr!`.
#[test]
fn no_hard_coded_ui_strings() {
    let mut bad = Vec::new();
    for f in UI_FILES {
        bad.extend(offenders(f, false));
    }
    for f in ERROR_FILES {
        bad.extend(offenders(f, true));
    }
    assert!(
        bad.is_empty(),
        "{} hard-coded UI string(s) — move them into locales/en + locales/nb and use tr!():\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn the_lint_sees_what_it_should() {
    let src = r##"
fn f(ui: &mut Ui) {
    ui.label("Open a file");           // caught
    ui.button("Quit");                  // caught
    let id = egui::Id::new("timeline_panel");
    log::info!("Opening the thing");
    ui.label(tr!("player.open"));
    let c = '"'; ui.label("Saved."); // caught
    let x = "libx264";
    ui.label("Odd one"); // i18n-ok
    let s = r#"Raw words here"#; // caught
}
"##;
    let found: Vec<_> = literals(src).into_iter().filter(|l| is_human(&l.text) && !exempt(&l.lead)).map(|l| l.text).collect();
    assert_eq!(found, vec!["Open a file", "Quit", "Saved.", "Raw words here"]);
}

#[test]
fn locale_detection_follows_gettext_order() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    };
    let p = env_preferences(env(&[("LANG", "nb_NO.UTF-8")]));
    assert_eq!(pick(p.iter().map(String::as_str)), Lang::Nb);
    let p = env_preferences(env(&[("LANG", "nb_NO.UTF-8"), ("LC_MESSAGES", "en_US.UTF-8")]));
    assert_eq!(pick(p.iter().map(String::as_str)), Lang::En, "LC_MESSAGES beats LANG");
    let p = env_preferences(env(&[("LANG", "en_US.UTF-8"), ("LANGUAGE", "nb:en")]));
    assert_eq!(pick(p.iter().map(String::as_str)), Lang::Nb, "LANGUAGE beats LANG");
    let p = env_preferences(env(&[("LC_ALL", "C"), ("LANGUAGE", "nb")]));
    assert_eq!(pick(p.iter().map(String::as_str)), Lang::En, "C locale ignores LANGUAGE");
    let p = env_preferences(env(&[("LANG", "de_DE.UTF-8")]));
    assert_eq!(pick(p.iter().map(String::as_str)), Lang::En, "unknown language falls back to English");
    assert_eq!(pick(["nb-NO", "en-US"]), Lang::Nb, "Windows/macOS style tags");
    assert_eq!(pick(["no"]), Lang::Nb);
    assert_eq!(pick(["nn_NO"]), Lang::Nb);
    assert_eq!(pick(std::iter::empty()), Lang::En);
}

#[test]
fn numbers_follow_the_locale() {
    set_thread_lang(Some(Lang::Nb));
    assert_eq!(num(2.5, 2), "2,50");
    assert_eq!(clock(83.25), "01:23,25");
    set_thread_lang(Some(Lang::En));
    assert_eq!(num(2.5, 2), "2.50");
    assert_eq!(clock(3723.5), "1:02:03.50");
    set_thread_lang(None);
}

#[test]
fn placeholders_fill_and_plurals_pick() {
    assert_eq!(fill("Opening {name}…", &[("name", &"clip.mp4")]), "Opening clip.mp4…");
    assert_eq!(fill("{missing} stays", &[]), "{missing} stays");
    // User data passes through untouched, even when it looks like a key.
    assert_eq!(fill("{name}", &[("name", &"player.open {x}")]), "player.open {x}");
}
