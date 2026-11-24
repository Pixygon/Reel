//! Localisation: every word the desktop app shows comes from here.
//!
//! The strings live in `locales/<lang>/<area>.txt`, one `key = value` per line,
//! compiled into the binary. English (`en`) and Norwegian bokmål (`nb`) are the
//! two languages; English is the fallback for anything missing (and a test
//! fails the build when a key is missing from either side).
//!
//! * `tr!("key")` gives the `&'static str` for the current language.
//! * `tr!("key", name = value, …)` fills `{name}` placeholders.
//! * `trn!("key", n, …)` picks `key.one` / `key.other` by count (`{n}` is filled).
//! * `num(x, decimals)` and `clock(secs)` print numbers the locale's way
//!   (bokmål uses a decimal COMMA). Never format a user-facing number with
//!   `{:.2}` directly.
//!
//! The language follows the OS (LANGUAGE/LC_ALL/LC_MESSAGES/LANG on Linux, the
//! user's preferred languages on Windows and macOS) unless the user picked one
//! under ☰ → Language, which is remembered in `<config>/reel/language`.
//!
//! User media, caption text, titles the user typed and file names are DATA:
//! they are passed through as placeholder values and never translated.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    Nb,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Nb];

    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Nb => "nb",
        }
    }

    /// The language's own name for itself (shown in the picker untranslated,
    /// so a user stuck in the wrong language can still find theirs).
    pub fn endonym(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Nb => "Norsk bokmål",
        }
    }

    fn from_u8(v: u8) -> Lang {
        if v == 1 { Lang::Nb } else { Lang::En }
    }
}

/// The resource files, per language, in a fixed order. Adding an area means
/// adding the file to BOTH lists — the key-parity test catches a miss.
macro_rules! area_files {
    ($lang:literal: $($area:literal),* $(,)?) => {
        &[$(include_str!(concat!("../locales/", $lang, "/", $area, ".txt"))),*]
    };
}
const EN_FILES: &[&str] = area_files!("en":
    "common", "player", "panel", "mixer", "editor", "app", "capture", "engine");
const NB_FILES: &[&str] = area_files!("nb":
    "common", "player", "panel", "mixer", "editor", "app", "capture", "engine");

type Table = HashMap<&'static str, String>;

/// Parse `key = value` lines. `#` starts a comment line; `\n` in a value is a
/// newline, `\\` a backslash. Leading/trailing spaces around the value are
/// trimmed (write `\s` for a deliberate edge space).
fn parse(files: &[&'static str]) -> Result<Table, String> {
    let mut t = Table::new();
    for src in files {
        for (i, line) in src.lines().enumerate() {
            let l = line.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            let Some((k, v)) = l.split_once(" = ").or_else(|| l.strip_suffix(" =").map(|k| (k, ""))) else {
                return Err(format!("line {}: no ` = ` in {l:?}", i + 1));
            };
            let k = k.trim();
            let mut out = String::with_capacity(v.len());
            let mut chars = v.trim().chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    match chars.next() {
                        Some('n') => out.push('\n'),
                        Some('s') => out.push(' '),
                        Some('\\') => out.push('\\'),
                        Some(o) => {
                            out.push('\\');
                            out.push(o);
                        }
                        None => out.push('\\'),
                    }
                } else {
                    out.push(c);
                }
            }
            if t.insert(k, out).is_some() {
                return Err(format!("duplicate key {k}"));
            }
        }
    }
    Ok(t)
}

fn table(lang: Lang) -> &'static Table {
    static EN: OnceLock<Table> = OnceLock::new();
    static NB: OnceLock<Table> = OnceLock::new();
    match lang {
        Lang::En => EN.get_or_init(|| parse(EN_FILES).expect("locales/en is malformed")),
        Lang::Nb => NB.get_or_init(|| parse(NB_FILES).expect("locales/nb is malformed")),
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

#[cfg(test)]
thread_local! {
    /// Per-test language (the harness runs each test on its own thread), so a
    /// test rendering in bokmål never flips another test to it.
    static TEST_LANG: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub fn set_thread_lang(l: Option<Lang>) {
    TEST_LANG.with(|c| c.set(l));
}

pub fn lang() -> Lang {
    #[cfg(test)]
    if let Some(l) = TEST_LANG.with(|c| c.get()) {
        return l;
    }
    Lang::from_u8(CURRENT.load(Ordering::Relaxed))
}

pub fn set_lang(l: Lang) {
    CURRENT.store(l as u8, Ordering::Relaxed);
}

/// Look a key up in the current language, falling back to English, then to
/// the key itself (visible, so a missing string is noticed, never blank).
pub fn t(key: &'static str) -> &'static str {
    lookup(lang(), key)
}

pub fn lookup(l: Lang, key: &'static str) -> &'static str {
    if let Some(v) = table(l).get(key) {
        return v.as_str();
    }
    if let Some(v) = table(Lang::En).get(key) {
        return v.as_str();
    }
    key
}

/// Fill `{name}` placeholders. Unknown placeholders are left as written.
pub fn fill(template: &str, args: &[(&str, &dyn std::fmt::Display)]) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match args.iter().find(|(n, _)| *n == name) {
                    Some((_, v)) => out.push_str(&v.to_string()),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `key.one` when `n == 1`, else `key.other` — both languages here pluralise
/// the same way.
pub fn plural_key(n: i64) -> &'static str {
    if n == 1 { "one" } else { "other" }
}

pub fn tn(key: &'static str, n: i64) -> &'static str {
    // Keys are compile-time literals; build the suffixed key once per call
    // and leak nothing: look both up by the borrowed string.
    let suffixed = format!("{key}.{}", plural_key(n));
    let l = lang();
    table(l)
        .get(suffixed.as_str())
        .or_else(|| table(Lang::En).get(suffixed.as_str()))
        .map(|s| s.as_str())
        .unwrap_or(key)
}

#[macro_export]
macro_rules! tr {
    ($key:literal) => {
        $crate::i18n::t($key)
    };
    ($key:literal, $($name:ident = $val:expr),+ $(,)?) => {
        $crate::i18n::fill(
            $crate::i18n::t($key),
            &[$((stringify!($name), &$val as &dyn ::std::fmt::Display)),+],
        )
    };
}

#[macro_export]
macro_rules! trn {
    ($key:literal, $n:expr) => {{
        let n = $n as i64;
        $crate::i18n::fill($crate::i18n::tn($key, n), &[("n", &n as &dyn ::std::fmt::Display)])
    }};
    ($key:literal, $n:expr, $($name:ident = $val:expr),+ $(,)?) => {{
        let n = $n as i64;
        $crate::i18n::fill(
            $crate::i18n::tn($key, n),
            &[("n", &n as &dyn ::std::fmt::Display), $((stringify!($name), &$val as &dyn ::std::fmt::Display)),+],
        )
    }};
}

/// The locale's decimal separator.
pub fn decimal_sep() -> char {
    match lang() {
        Lang::En => '.',
        Lang::Nb => ',',
    }
}

/// A number with a fixed count of decimals, written the locale's way
/// (`2.50` / `2,50`). Thousands are not grouped: the numbers Reel shows are
/// seconds, frames and dB, where a group mark would only add noise.
pub fn num(x: impl Into<f64>, decimals: usize) -> String {
    let s = format!("{:.*}", decimals, x.into());
    localise_decimal(s)
}

/// Swap the decimal point of an already formatted number.
pub fn localise_decimal(s: String) -> String {
    match decimal_sep() {
        '.' => s,
        sep => s.replace('.', &sep.to_string()),
    }
}

/// egui slider/drag-value readout in the locale's way — pass to
/// `.custom_formatter(crate::i18n::egui_num)`.
pub fn egui_num(v: f64, decimals: std::ops::RangeInclusive<usize>) -> String {
    localise_decimal(egui::emath::format_with_decimals_in_range(v, decimals))
}

/// Parse a number the user typed into a slider: either separator works.
pub fn egui_parse(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").replace('−', "-").parse().ok()
}

/// `mm:ss.cc` (or `h:mm:ss.cc`) with the locale's decimal separator.
pub fn clock(secs: f64) -> String {
    let s = secs.max(0.0);
    let h = (s / 3600.0).floor() as u64;
    let m = ((s % 3600.0) / 60.0).floor() as u64;
    let rem = s - (h * 3600 + m * 60) as f64;
    let out = if h > 0 { format!("{h}:{m:02}:{rem:05.2}") } else { format!("{m:02}:{rem:05.2}") };
    localise_decimal(out)
}

// ---------------------------------------------------------------- detection

/// Map one locale tag (`nb_NO.UTF-8`, `nb-NO`, `no`, `en_US`, `C`) to a
/// language Reel speaks. Nynorsk and plain "Norwegian" readers get bokmål —
/// far closer to them than English.
pub fn parse_tag(tag: &str) -> Option<Lang> {
    let t = tag.trim().to_ascii_lowercase();
    let base = t.split(|c| c == '_' || c == '-' || c == '.' || c == '@').next().unwrap_or("");
    match base {
        "nb" | "no" | "nn" | "nob" | "nno" | "nor" => Some(Lang::Nb),
        "en" | "eng" => Some(Lang::En),
        _ => None,
    }
}

/// The first language Reel speaks among the user's preferences, given in
/// priority order. `C`/`POSIX` and languages Reel lacks are skipped; nothing
/// usable means English.
pub fn pick<'a>(prefs: impl IntoIterator<Item = &'a str>) -> Lang {
    for p in prefs {
        if let Some(l) = parse_tag(p) {
            return l;
        }
        // A real language Reel does not speak (de_DE, sv_SE): stop and use
        // the fallback rather than digging out a lower-priority match the
        // user never asked to see first — except for the `C` locale, which
        // means "no preference", and empty entries.
        let base = p.trim();
        if !(base.is_empty() || base == "C" || base.starts_with("C.") || base == "POSIX") {
            return Lang::En;
        }
    }
    Lang::En
}

/// The OS's language preference list, highest priority first.
pub fn os_preferences() -> Vec<String> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        let mut v: Vec<String> = sys_locale::get_locales().collect();
        if v.is_empty() {
            v.extend(sys_locale::get_locale());
        }
        v
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        env_preferences(|k| std::env::var(k).ok())
    }
}

/// gettext order: LANGUAGE (a colon list) wins, then LC_ALL, LC_MESSAGES,
/// LANG. LANGUAGE is ignored when the locale itself is `C`, as gettext does.
pub fn env_preferences(get: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|k| get(k).filter(|v| !v.is_empty()))
        .next();
    let mut out = Vec::new();
    let c_locale = matches!(locale.as_deref(), Some("C") | Some("POSIX"));
    if !c_locale {
        if let Some(list) = get("LANGUAGE") {
            out.extend(list.split(':').filter(|s| !s.is_empty()).map(str::to_string));
        }
    }
    out.extend(locale);
    out
}

fn config_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_default();
    #[cfg(target_os = "macos")]
    let base = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("Library/Application Support");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("reel")
}

fn override_path() -> std::path::PathBuf {
    config_dir().join("language")
}

/// The user's saved choice: `None` = follow the OS.
pub fn saved_override() -> Option<Lang> {
    std::fs::read_to_string(override_path()).ok().and_then(|s| parse_tag(&s))
}

/// Remember (or with `None` forget) the user's choice and apply it now.
pub fn choose(l: Option<Lang>) {
    let p = override_path();
    match l {
        Some(l) => {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&p, l.code());
        }
        None => {
            let _ = std::fs::remove_file(&p);
        }
    }
    set_lang(l.unwrap_or_else(|| pick(os_preferences().iter().map(String::as_str))));
}

/// Called once at startup: `REEL_LANG` (tests, support) > saved choice > OS.
pub fn init() {
    let l = std::env::var("REEL_LANG")
        .ok()
        .and_then(|v| parse_tag(&v))
        .or_else(saved_override)
        .unwrap_or_else(|| pick(os_preferences().iter().map(String::as_str)));
    set_lang(l);
}

/// All keys of one language (tests).
#[cfg(test)]
pub fn keys(l: Lang) -> Vec<&'static str> {
    let mut k: Vec<_> = table(l).keys().copied().collect();
    k.sort();
    k
}

#[cfg(test)]
#[path = "i18n_tests.rs"]
mod tests;
