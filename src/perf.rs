//! Frame-path instrumentation. Off unless `REEL_PERF=1`; when on, it prints a
//! per-second summary of where each displayed frame's time goes — mpv's
//! software render, the alpha fixup, and the CPU→GPU upload. This is how we
//! decide (and prove) whether the zero-copy GPU path is worth its complexity.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);

struct Acc {
    frames: u64,
    render_us: f64,
    alpha_us: f64,
    upload_us: f64,
    since: Instant,
    px: u64,
    redraws: u64,
}

static ACC: Mutex<Option<Acc>> = Mutex::new(None);

pub fn init() {
    if std::env::var("REEL_PERF").as_deref() == Ok("1") {
        ENABLED.store(true, Ordering::Relaxed);
        log::info!("REEL_PERF=1 — frame-path timing on");
    }
}

pub fn on() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn note_decode(render_us: f64, alpha_us: f64) {
    if !on() {
        return;
    }
    let mut g = ACC.lock().unwrap();
    let acc = g.get_or_insert_with(|| Acc {
        frames: 0,
        render_us: 0.0,
        alpha_us: 0.0,
        upload_us: 0.0,
        since: Instant::now(),
        px: 0,
        redraws: 0,
    });
    acc.render_us += render_us;
    acc.alpha_us += alpha_us;
}

/// Count a redraw (whether or not it carried a new video frame) — separates
/// "our frame path is slow" from "the compositor is throttling us".
pub fn note_redraw() {
    if !on() {
        return;
    }
    let mut g = ACC.lock().unwrap();
    if let Some(acc) = g.as_mut() {
        acc.redraws += 1;
    }
}

pub fn note_upload(upload_us: f64, w: u32, h: u32) {
    if !on() {
        return;
    }
    let mut g = ACC.lock().unwrap();
    let Some(acc) = g.as_mut() else { return };
    acc.upload_us += upload_us;
    acc.frames += 1;
    acc.px = (w as u64) * (h as u64);
    if acc.since.elapsed().as_secs_f64() >= 1.0 && acc.frames > 0 {
        let n = acc.frames as f64;
        let (r, a, u) = (acc.render_us / n, acc.alpha_us / n, acc.upload_us / n);
        let mb_s = (acc.px as f64 * 4.0 * n) / (1024.0 * 1024.0) / acc.since.elapsed().as_secs_f64();
        log::info!(
            "[perf] {:.0} new frames/s (loop {:.0} redraws/s) · per frame: mpv-render {r:.0}µs, \
             alpha {a:.0}µs, upload {u:.0}µs (total {:.0}µs) · {mb_s:.0} MB/s over the bus",
            n / acc.since.elapsed().as_secs_f64(),
            acc.redraws as f64 / acc.since.elapsed().as_secs_f64(),
            r + a + u
        );
        *acc = Acc {
            frames: 0,
            render_us: 0.0,
            alpha_us: 0.0,
            upload_us: 0.0,
            since: Instant::now(),
            px: acc.px,
            redraws: 0,
        };
    }
}
