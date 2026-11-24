//! Render the real main window headlessly in bokmål and read back every piece
//! of text egui laid out. Any English word left over (one that appears in the
//! English resources but nowhere in the bokmål ones, or a common English UI
//! word) fails the test — brand and product names excepted.

#![cfg(test)]

use crate::app::{Mode, ReelApp};
use crate::i18n::{self, Lang};
use egui::{Context, Event, PointerButton, Pos2, RawInput, Rect, Shape, Vec2};
use std::collections::BTreeSet;

/// Names that are the same in every language.
const KEEP: &[&str] = &[
    "reel", "pixygon", "ffmpeg", "mpv", "libmpv", "whisper", "pipewire", "nvenc", "lut", "luts", "lufs",
    "hdr", "srt", "webm", "mp4", "mkv", "mov", "mp3", "flac", "opus", "png", "jpg", "svg", "gif", "ctrl",
    "shift", "alt", "enter", "backspace", "delete", "esc", "tab", "fps", "rgb", "hsl", "pip", "dbfs",
];

/// Everyday English UI words that must never show in the bokmål window, even
/// if they slipped past the resource files entirely.
const ENGLISH: &[&str] = &[
    "the", "and", "open", "file", "files", "play", "pause", "edit", "export", "clip", "clips", "timeline",
    "track", "tracks", "drop", "here", "click", "new", "add", "remove", "settings", "music", "image",
    "images", "speed", "volume", "project", "search", "effects", "done", "cancel", "apply", "later",
    "default", "record", "screen", "window", "quit", "help", "keyboard", "loop", "mute", "your", "with",
    "from", "selected", "untitled", "ready", "split", "undo", "redo", "titles", "captions", "website",
];

fn texts(shapes: &[egui::epaint::ClippedShape], out: &mut Vec<(String, Rect)>) {
    fn walk(s: &Shape, out: &mut Vec<(String, Rect)>) {
        match s {
            Shape::Text(t) => out.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
            Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    for c in shapes {
        walk(&c.shape, out);
    }
}

fn words(s: &str) -> impl Iterator<Item = String> + '_ {
    s.split(|c: char| !c.is_alphabetic())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
}

/// Words that betray English: in the English resources but in no bokmål
/// value, plus the everyday list — minus brand/product names.
fn english_words() -> BTreeSet<String> {
    let nb: BTreeSet<String> = i18n::keys(Lang::Nb).iter().flat_map(|k| words(i18n::lookup(Lang::Nb, k)).collect::<Vec<_>>()).collect();
    let mut en: BTreeSet<String> = i18n::keys(Lang::En).iter().flat_map(|k| words(i18n::lookup(Lang::En, k)).collect::<Vec<_>>()).collect();
    en.extend(ENGLISH.iter().map(|s| s.to_string()));
    en.retain(|w| !nb.contains(w) && !KEEP.contains(&w.as_str()));
    en
}

fn input(events: Vec<Event>, height: f32) -> RawInput {
    RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1600.0, height))),
        events,
        ..Default::default()
    }
}

fn frame_at(ctx: &Context, app: &mut ReelApp, events: Vec<Event>, height: f32) -> Vec<(String, Rect)> {
    let out = ctx.run(input(events, height), |ctx| crate::ui::draw(ctx, app));
    let mut t = Vec::new();
    texts(&out.shapes, &mut t);
    t
}

fn frame(ctx: &Context, app: &mut ReelApp, events: Vec<Event>) -> Vec<(String, Rect)> {
    frame_at(ctx, app, events, 1000.0)
}

fn click(at: Pos2) -> Vec<Event> {
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Default::default() };
    vec![Event::PointerMoved(at), b(true), b(false)]
}

/// A tiny still standing in for opened media (no decoder, no GPU needed):
/// with it loaded the player shows its chrome and the export dialog opens.
fn still() -> crate::media::ImageDoc {
    crate::media::ImageDoc { path: "/tmp/holiday.png".into(), width: 4, height: 4, data: vec![255; 64] }
}

/// Render `app` for a few frames, click the ☰ menu open, and collect the
/// text of both the window and the menu.
fn scene(ctx: &Context, app: &mut ReelApp, height: f32, label: &str, seen: &mut Vec<(String, String)>) {
    let still_pointer = || vec![Event::PointerMoved(Pos2::new(800.0, height - 60.0))];
    for _ in 0..30 {
        frame_at(ctx, app, still_pointer(), height);
    }
    let t = frame_at(ctx, app, still_pointer(), height);
    let menu = t.iter().find(|(s, _)| s.contains('☰')).map(|(_, r)| r.center());
    seen.extend(t.into_iter().map(|(s, _)| (label.to_string(), s)));
    let at = menu.unwrap_or_else(|| panic!("[{label}] no ☰ menu on screen"));
    frame_at(ctx, app, click(at), height);
    let opened = frame_at(ctx, app, vec![Event::PointerMoved(at)], height);
    seen.extend(opened.into_iter().map(|(s, _)| (format!("{label} ☰"), s)));
}

/// Every surface a first-time user meets, rendered in bokmål.
fn rendered_in_bokmal() -> Vec<(String, String)> {
    i18n::set_thread_lang(Some(Lang::Nb));
    let ctx = Context::default();
    crate::theme::apply(&ctx);
    let mut seen: Vec<(String, String)> = Vec::new();

    // 1. The player as a double-click opens it, the defaults banner up.
    let mut app = ReelApp::new();
    app.defaults_banner = true;
    for _ in 0..3 {
        frame(&ctx, &mut app, vec![]);
    }
    seen.extend(frame(&ctx, &mut app, vec![]).into_iter().map(|(s, _)| ("empty player".to_string(), s)));

    // 2. Media open: player chrome + ☰ menu, then the dialogs (default apps,
    //    keyboard reference, export) over it.
    let mut app = ReelApp::new();
    app.image = Some(still());
    scene(&ctx, &mut app, 1000.0, "player", &mut seen);
    let mut app = ReelApp::new();
    app.image = Some(still());
    app.defaults_open = true;
    app.show_keymap = true;
    app.export_open = true;
    for _ in 0..5 {
        frame_at(&ctx, &mut app, vec![], 1600.0);
    }
    seen.extend(frame_at(&ctx, &mut app, vec![], 1600.0).into_iter().map(|(s, _)| ("dialogs".to_string(), s)));

    // 3. The editor with a clip on the timeline, selected, so the inspector
    //    shows every section — on a very tall screen so its scroll area
    //    lays all of them out (mixer, captions, music, titles included).
    let mut app = ReelApp::new();
    app.mode = Mode::Editor;
    app.image = Some(still());
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixture.mp4");
    app.project.name = "fixture".into(); // user data: a project is named after its file
    app.project.append_video("fixture.mp4", fixture, 2.0);
    let id = app.project.tracks[0].clips[0].id;
    app.editor.selected = Some(id);
    scene(&ctx, &mut app, 6000.0, "editor", &mut seen);
    app.export_open = true;
    for _ in 0..5 {
        frame_at(&ctx, &mut app, vec![], 1600.0);
    }
    seen.extend(frame_at(&ctx, &mut app, vec![], 1600.0).into_iter().map(|(s, _)| ("editor export".to_string(), s)));
    i18n::set_thread_lang(None);
    seen
}

#[test]
fn the_main_window_in_bokmal_has_no_english() {
    let english = english_words();
    let seen = rendered_in_bokmal();
    if std::env::var("REEL_I18N_DUMP").is_ok() {
        for (s, t) in &seen {
            eprintln!("[{s}] {t:?}");
        }
    }
    assert!(seen.len() > 40, "the window rendered almost nothing ({} texts) — the test is blind", seen.len());
    let mut bad: Vec<String> = Vec::new();
    for (surface, text) in &seen {
        // User data in the scene (the clip's file name) is never translated.
        if text.contains("fixture") || text.contains("holiday") {
            continue;
        }
        let hits: Vec<String> = words(text).filter(|w| english.contains(w)).collect();
        if !hits.is_empty() {
            bad.push(format!("[{surface}] {text:?} — {hits:?}"));
        }
    }
    bad.sort();
    bad.dedup();
    assert!(bad.is_empty(), "English text in the bokmål window:\n{}", bad.join("\n"));
}

/// The same scenes in English must render the English strings — proves the
/// test above actually switches language rather than passing vacuously.
#[test]
fn the_language_switch_reaches_the_window() {
    let ctx = Context::default();
    let mut app = ReelApp::new();
    let render = |ctx: &Context, app: &mut ReelApp, l: Lang| {
        i18n::set_thread_lang(Some(l));
        app.defaults_banner = true;
        let t = frame(ctx, app, vec![]);
        i18n::set_thread_lang(None);
        t.into_iter().map(|(s, _)| s).collect::<Vec<_>>().join("\n")
    };
    let en = render(&ctx, &mut app, Lang::En);
    let nb = render(&ctx, &mut app, Lang::Nb);
    assert_ne!(en, nb, "the window looks the same in both languages");
    assert!(en.contains(i18n::lookup(Lang::En, "player.defaults.ask")), "{en}");
    assert!(nb.contains(i18n::lookup(Lang::Nb, "player.defaults.ask")), "{nb}");
}
