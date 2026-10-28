//! Self-measurement so a play session shows whether stutter comes from this plugin: game-camera tick intervals
//! (the frame time the player sees), time spent in our own code per frame, the engine's ragdoll updates, and the
//! slowest single operation by name. One `[perf]` line every REPORT_MS, one `[perf] HITCH` line per long frame.
//! Game thread only (all timers run inside game-thread hooks).

use std::sync::Mutex;
use std::time::Instant;

/// A frame interval at least this long is a visible hitch (3 frames at 60 fps).
pub const HITCH_MS: f64 = 50.0;
const REPORT_MS: f64 = 5000.0;

struct Perf {
    window: Option<Instant>,
    last_tick: Option<Instant>,
    frames: u32,
    max_interval_ms: f64,
    hitches: u32,
    cur_mod_ns: u64,
    sum_mod_ns: u64,
    max_mod_ns: u64,
    max_engine_ns: u64,
    slowest: (u64, String),
    prev_slowest: (u64, String),
}

static PERF: Mutex<Perf> = Mutex::new(Perf {
    window: None,
    last_tick: None,
    frames: 0,
    max_interval_ms: 0.0,
    hitches: 0,
    cur_mod_ns: 0,
    sum_mod_ns: 0,
    max_mod_ns: 0,
    max_engine_ns: 0,
    slowest: (0, String::new()),
    prev_slowest: (0, String::new()),
});

fn lock() -> std::sync::MutexGuard<'static, Perf> {
    PERF.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    /// our own code, top level (summed into the plugin time per frame)
    Ours,
    /// an engine call we only wrap (max reported separately)
    Engine,
    /// a named step inside an `Ours` timer: competes for "slowest" without being counted twice
    Note,
}

pub fn add(label: &str, ns: u64, kind: Kind) {
    let mut p = lock();
    match kind {
        Kind::Engine => p.max_engine_ns = p.max_engine_ns.max(ns),
        Kind::Ours | Kind::Note => {
            if kind == Kind::Ours {
                p.cur_mod_ns += ns;
            }
            if ns > p.slowest.0 {
                p.slowest = (ns, label.to_string());
            }
        }
    }
}

/// Measures until dropped.
pub struct Timer {
    label: &'static str,
    t0: Instant,
    kind: Kind,
}

impl Timer {
    pub fn new(label: &'static str, kind: Kind) -> Self {
        Timer { label, t0: Instant::now(), kind }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        add(self.label, self.t0.elapsed().as_nanos() as u64, self.kind);
    }
}

/// At the start of each game-camera tick: close the previous frame's books, detect a hitch, report every REPORT_MS.
/// Returns log lines for the caller to write (keeps logging out of the lock).
pub fn tick() -> Vec<String> {
    let now = Instant::now();
    let mut out = Vec::new();
    let mut p = lock();
    let interval = p.last_tick.map(|t| now.duration_since(t).as_secs_f64() * 1000.0);
    p.last_tick = Some(now);
    let frame_ns = p.cur_mod_ns;
    p.cur_mod_ns = 0;
    p.sum_mod_ns += frame_ns;
    p.max_mod_ns = p.max_mod_ns.max(frame_ns);
    p.frames += 1;
    if let Some(iv) = interval {
        p.max_interval_ms = p.max_interval_ms.max(iv);
        if iv >= HITCH_MS {
            p.hitches += 1;
            out.push(format!(
                "[perf] HITCH {iv:.0} ms between frames; this plugin used {:.2} ms in the frame before{}",
                frame_ns as f64 / 1e6,
                if p.prev_slowest.0 > 0 { format!(" (slowest: {} {:.2} ms)", p.prev_slowest.1, p.prev_slowest.0 as f64 / 1e6) } else { String::new() }
            ));
        }
    }
    p.prev_slowest = std::mem::take(&mut p.slowest);
    let start = *p.window.get_or_insert(now);
    if now.duration_since(start).as_secs_f64() * 1000.0 >= REPORT_MS {
        let n = p.frames.max(1);
        out.push(format!(
            "[perf] {:.1} s: {} frames, longest frame {:.1} ms, hitches (>= {HITCH_MS:.0} ms) {} | plugin per frame avg {:.3} ms max {:.2} ms | engine ragdoll update max {:.2} ms",
            now.duration_since(start).as_secs_f64(), p.frames, p.max_interval_ms, p.hitches,
            p.sum_mod_ns as f64 / n as f64 / 1e6, p.max_mod_ns as f64 / 1e6, p.max_engine_ns as f64 / 1e6
        ));
        *p = Perf { window: Some(now), last_tick: p.last_tick, prev_slowest: std::mem::take(&mut p.prev_slowest), ..Perf::fresh() };
    }
    out
}

impl Perf {
    fn fresh() -> Perf {
        Perf {
            window: None,
            last_tick: None,
            frames: 0,
            max_interval_ms: 0.0,
            hitches: 0,
            cur_mod_ns: 0,
            sum_mod_ns: 0,
            max_mod_ns: 0,
            max_engine_ns: 0,
            slowest: (0, String::new()),
            prev_slowest: (0, String::new()),
        }
    }
}
