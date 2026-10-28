//! sab_asi — an in-process engine SDK for The Saboteur (retail GOG `Saboteur.exe`).
//!
//! The game is a 32-bit process loaded at a fixed base `0x400000` (no ASLR — verified live), so every
//! engine VA below is absolute and callable/hookable directly. Two surfaces:
//!
//!   1. **Dynamic hooks** (MinHook detours) that read engine state and append it to `sab_devlog.txt`
//!      next to the exe. Runs at full engine speed on the game's own threads — no debugger, no pause,
//!      so it cannot trigger the "resume after a full-process freeze" crash a breakpoint does.
//!   2. A **`Dev.*` Lua binding surface**: we register our own C functions into the live `lua_State`
//!      by piggy-backing on the engine's own table-publish routine, so they're callable from the
//!      game console / any script — a permanent live-inspection seam.
//!
//! Injection: build as a cdylib, rename to `sab_asi.asi`, and load it with Ultimate ASI Loader
//! (proxy `dinput8.dll`) or DXwrapper dropped in the game folder. See README.md.
//!
//! Engine facts are sourced from `docs/sab-engine-lua-seam/` and the Ghidra decomp; see `addrs`.

#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
// release builds (no `dev` feature) leave the developer console, Dev.* table and probes unreachable
#![cfg_attr(not(feature = "dev"), allow(dead_code, unused_imports, unused_variables, unused_mut))]

use core::ffi::{c_char, c_int, c_void};
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// Engine virtual addresses. Imagebase `0x400000`, no ASLR (confirmed: the module list reports base
/// `0x400000` under the debugger, and the decomp's absolute call targets resolve 1:1). If a future
/// build/patch shifts these, only this module changes.
mod addrs {
    /// `__thiscall` human body-setup (`WSFxHumanBodySetup`). `ecx` = the setup object; `+0x60` is the
    /// combined-LOD model handle (`SetupCombinedLODModel`'s write target). 15 params total
    /// (`this` + 14 stack args) — the detour must forward all of them.
    pub const BODY_SETUP: usize = 0x0051_8490;

    /// `__thiscall(manager, undefined4* pL)` — runs once per level load; publishes the engine's 26 Lua
    /// namespace tables. `L == *pL` (it feeds `*pL` to `luaL_register`/`luaL_loadstring`/`lua_pcall`).
    /// We hook it to grab `L` and append our own table right after. (docs/sab-engine-lua-seam/00 §7.1)
    pub const REGISTER_BINDINGS: usize = 0x006f_8a90;

    /// stock `luaL_openlib(lua_State* L, const char* libname, const luaL_Reg* l, int nup)` — cdecl.
    /// Creates-or-reuses the global table and installs the entries. (00 §7.1)
    pub const LUAL_OPENLIB: usize = 0x015f_d2d0;

    /// cdecl `lua_pcall(L, nargs, nresults, errfunc)` (docs/sab-engine-lua-seam/04). Entry is
    /// `push ebp; jmp short` + int3 padding; reached directly and via the thunk `0x401f90`.
    pub const LUA_PCALL: usize = 0x0040_8310;
    /// cdecl `luaL_loadstring(L, const char*)`; thunk `0x4041b0` jumps here.
    pub const LUAL_LOADSTRING: usize = 0x015f_e1a0;
    /// cdecl `lua_settop(L, idx)`: the target of the engine wrappers `0x6f6980` / `0x6f7290`.
    pub const LUA_SETTOP: usize = 0x0040_11e0;
    /// cdecl `lua_gettop(L)` = `(L->top - L->base) >> 3`.
    pub const LUA_GETTOP: usize = 0x0040_11d0;
    /// cdecl `lua_tolstring(L, idx, size_t* len)`; NULL for non-string/number values.
    pub const LUA_TOLSTRING: usize = 0x006a_c630;
    /// cdecl `lua_tonumber(L, idx)`; this build's `lua_Number` is `float` (returned in st0).
    pub const LUA_TONUMBER: usize = 0x0040_1510;

    /// `__thiscall WSGameCamera::Tick(float dt)`, returns `this + 0x70` (`ret 4`). Prologue
    /// `55 8B EC 83 EC 08` (push ebp; mov ebp,esp; sub esp,8): no relative operands, MinHook-safe.
    /// Called once per camera update from WSGameCamera vtable slot 1 (`0x678837`). None of Fusion
    /// Fix's 24 pattern sites lies within 0x100 bytes of it (nearest `0x686aa6`).
    pub const GAME_CAMERA_TICK: usize = 0x0067_1ae0;

    /// Active `WSGameCamera`: the frame function `0x43a280` uses `[0x1321b74]` when
    /// `[0x1321b78] >= 1` and passes `[cam+0xb0] + [0x121008c]` to `OdinCamera::SetFOV`.
    pub const GAME_CAMERA_PTR: usize = 0x0132_1b74;
    pub const GAME_CAMERA_COUNT: usize = 0x0132_1b78;
    /// `WSGameCamera` fields: current FOV (radians, `+0xb0`, written from the camera state FOV after
    /// the transition stack runs, `0x672789`); camera matrix `+0x70` (row 3 `+0xa0` = position,
    /// compared with the player position by `0x475bbe` through its copy at `+0x30`); RotSpeed /
    /// PitchSpeed of the blended camera state (`+0x1e8` / `+0x1ec`, read by the WSPlayer look code
    /// `0x5b4080`); live transitions in the 4-slot ring (`+0xad4`, `0x672789`).
    pub const GC_FOV: usize = 0xb0;
    pub const GC_MATRIX: usize = 0x70;
    pub const GC_ROT_SPEED: usize = 0x1e8;
    pub const GC_PITCH_SPEED: usize = 0x1ec;
    pub const GC_TRANSITIONS: usize = 0xad4;
}

// ─────────────────────────── logging sink ───────────────────────────

/// Log lines go through a channel to a writer thread: the game thread only formats the line and sends it (no file
/// I/O, no lock wait). The previous design wrote and flushed on the calling thread and, under lock contention,
/// retried with 1 ms sleeps - up to ~75 ms on the game thread (Windows sleep granularity; measured in
/// docs/bodydrag-performance-audit-2026-10-27.md).
static LOG: OnceLock<std::sync::mpsc::Sender<String>> = OnceLock::new();

fn logf(args: std::fmt::Arguments) {
    if let Some(tx) = LOG.get() {
        let _ = tx.send(args.to_string());
    }
}

/// Writer thread: buffered appends, flushed whenever the queue runs empty (lines reach the file within milliseconds).
fn log_writer(rx: std::sync::mpsc::Receiver<String>, file: std::fs::File) {
    let mut w = std::io::BufWriter::with_capacity(64 * 1024, file);
    while let Ok(line) = rx.recv() {
        let _ = writeln!(w, "{line}");
        while let Ok(more) = rx.try_recv() {
            let _ = writeln!(w, "{more}");
        }
        let _ = w.flush();
    }
}

/// A line written synchronously (DLL detach at process exit, when the writer thread no longer runs).
fn log_direct(line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path()) {
        let _ = writeln!(f, "{line}");
    }
}
macro_rules! log {
    ($($t:tt)*) => { crate::logf(format_args!($($t)*)) };
}

mod bodydrag;

/// `sab_devlog.txt` beside the host exe (`current_exe()` in an injected DLL is `Saboteur.exe`).
fn log_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("sab_devlog.txt")))
        .unwrap_or_else(|| "sab_devlog.txt".into())
}

/// Read a 32-bit word from process memory. Callers pass engine object pointers we already trust
/// (e.g. `this + 0x60`); a null base reads nothing. This is deliberately not SEH-guarded — the hook
/// sites hand us live, valid objects.
#[inline]
unsafe fn rd32(p: usize) -> u32 {
    if p == 0 {
        return 0;
    }
    core::ptr::read_unaligned(p as *const u32)
}

// ─────────────────────── dynamic hook: body setup ───────────────────────

/// `WSFxHumanBodySetup` setup, forwarding all 15 args unchanged. `undefined4` stack args are opaque
/// 4-byte slots — typing them `u32` preserves the callee-cleanup stack contract.
type BodySetupFn = unsafe extern "thiscall" fn(
    *mut c_void,
    u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, u32,
);
static BODY_ORIG: OnceLock<BodySetupFn> = OnceLock::new();

/// De-dup sets so the log stays readable: each distinct body-setup object and each distinct combined-LOD
/// model object is logged ONCE, not once per frame (the raw hook fires ~thousands/sec).
/// `Dev.DumpNextBodySetup()` clears both, forcing a fresh round of dumps (e.g. right after a disguise swap).
static SEEN_THIS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static SEEN_MODEL: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// True if `v` did not already contain `k` (and inserts it).
fn first_time(set: &Mutex<Vec<u32>>, k: u32) -> bool {
    let mut v = set.lock().unwrap_or_else(|p| p.into_inner());
    if v.contains(&k) {
        false
    } else {
        v.push(k);
        true
    }
}

/// A plausible in-process heap pointer (the observed combined-LOD objects sit at `0x0a……`–`0x2d……`).
#[inline]
fn looks_like_ptr(p: u32) -> bool {
    (0x0001_0000..0x7f00_0000).contains(&p)
}

/// Read `n` bytes at `addr` as space-separated LE dwords, for structure spelunking.
unsafe fn dump_words(addr: usize, n: usize) -> String {
    let mut s = String::with_capacity(n * 3);
    for i in 0..n / 4 {
        s.push_str(&format!("{:08x} ", rd32(addr + i * 4)));
    }
    s
}

#[allow(clippy::too_many_arguments)]
unsafe extern "thiscall" fn body_setup_detour(
    this: *mut c_void,
    a2: u32, a3: u32, a4: u32, a5: u32, a6: u32, a7: u32, a8: u32,
    a9: u32, a10: u32, a11: u32, a12: u32, a13: u32, a14: u32, a15: u32,
) {
    let t = this as usize;
    let p60 = rd32(t + 0x60);
    if first_time(&SEEN_THIS, t as u32) {
        log!(
            "[bodysetup] this=0x{:08x} +0x60=0x{:08x} +0x78(bp)=0x{:08x} +0xa4=0x{:08x}",
            t, p60, rd32(t + 0x78), rd32(t + 0xa4)
        );
    }
    // First time we see each distinct WSFxHumanBodyPartBlueprint at +0x60, dump 0x90 bytes (so we
    // capture the combined-LOD model pointer at partBP+0x84 — per sab-mattias-port-bugs), then FOLLOW
    // that pointer and dump the combined-LOD WSModel itself so we can name it (its +0x40 name-hash).
    if looks_like_ptr(p60) && first_time(&SEEN_MODEL, p60) {
        let m = p60 as usize;
        log!(
            "[partBP] ptr=0x{:08x} vtable=0x{:08x} | {}",
            p60, rd32(m), dump_words(m, 0x90)
        );
        let lod = rd32(m + 0x84);
        if looks_like_ptr(lod) {
            log!(
                "[lodmodel] via partBP+0x84 ptr=0x{:08x} vtable=0x{:08x} | {}",
                lod, rd32(lod as usize), dump_words(lod as usize, 0x60)
            );
        } else {
            log!("[lodmodel] partBP+0x84=0x{lod:08x} (not a pointer — no combined-LOD model set)");
        }
    }
    (BODY_ORIG.get().unwrap())(
        this, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15,
    )
}

// ─────────────────── binding surface: Dev.* Lua table ───────────────────

/// stock `luaL_Reg` — `{ const char *name; lua_CFunction func; }`.
#[repr(C)]
struct LuaLReg {
    name: *const c_char,
    func: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}
type LuaLOpenlib = unsafe extern "C" fn(*mut c_void, *const c_char, *const LuaLReg, c_int);

type RegBindingsFn = unsafe extern "thiscall" fn(*mut c_void, *mut *mut c_void);
static REG_ORIG: OnceLock<RegBindingsFn> = OnceLock::new();

/// The live `lua_State*`, captured the first time the engine publishes its tables.
static LUA_STATE: AtomicUsize = AtomicUsize::new(0);

unsafe extern "thiscall" fn reg_bindings_detour(manager: *mut c_void, p_l: *mut *mut c_void) {
    // Let the engine create its own 26 tables first, then graft ours on.
    (REG_ORIG.get().unwrap())(manager, p_l);
    let l = *p_l; // L == *param_2  (docs/sab-engine-lua-seam/00 §7.1)
    LUA_STATE.store(l as usize, Ordering::SeqCst);
    register_dev(l);
    log!("[lua] Dev.* registered on L=0x{:08x}", l as usize);
}

// ---- Dev.* implementations (cdecl `lua_CFunction`: int f(lua_State*)) ----

/// `Dev.Mark()` — write a separator to the log so you can bracket an experiment.
unsafe extern "C" fn dev_mark(_l: *mut c_void) -> c_int {
    log!("──── Dev.Mark ────");
    0
}

/// `Dev.DumpNextBodySetup()` — clear the de-dup sets so every body-setup/model re-logs once more.
/// Call it, then swap a disguise, to force a fresh capture of the player's rebuilt body.
unsafe extern "C" fn dev_dump_next(_l: *mut c_void) -> c_int {
    SEEN_THIS.lock().unwrap_or_else(|p| p.into_inner()).clear();
    SEEN_MODEL.lock().unwrap_or_else(|p| p.into_inner()).clear();
    log!("[lua] Dev.DumpNextBodySetup: seen-sets cleared, will re-dump");
    0
}

/// `Dev.Log(s)` — append `s` (string or number) to the log. The console's `print` routes here.
unsafe extern "C" fn dev_log(l: *mut c_void) -> c_int {
    log!("[lua] {}", lua_str(l, 1).unwrap_or_else(|| "(non-string)".into()));
    0
}

/// `Dev.CamLog(seconds)` — record the active WSGameCamera every Tick to a new
/// `sab_camlog_<ms>.csv` for `seconds` (default 15, clamped to 1..600).
unsafe extern "C" fn dev_camlog(l: *mut c_void) -> c_int {
    let tonumber: LuaToNumberFn = core::mem::transmute(addrs::LUA_TONUMBER);
    let secs = tonumber(l, 1);
    camlog_start(if secs > 0.0 { secs } else { 15.0 });
    0
}

unsafe fn register_dev(l: *mut c_void) {
    // luaL_openlib copies these into the table during the call, so a local array is fine.
    let regs = [
        LuaLReg { name: b"Mark\0".as_ptr() as *const c_char, func: Some(dev_mark) },
        LuaLReg { name: b"DumpNextBodySetup\0".as_ptr() as *const c_char, func: Some(dev_dump_next) },
        LuaLReg { name: b"Log\0".as_ptr() as *const c_char, func: Some(dev_log) },
        LuaLReg { name: b"CamLog\0".as_ptr() as *const c_char, func: Some(dev_camlog) },
        LuaLReg { name: core::ptr::null(), func: None },
    ];
    let openlib: LuaLOpenlib = core::mem::transmute(addrs::LUAL_OPENLIB);
    openlib(l, b"Dev\0".as_ptr() as *const c_char, regs.as_ptr(), 0);
}

// ─────────────────── Lua console: queued chunks run on the Lua thread ───────────────────
//
// Lua is single-threaded, so the console never touches `L` from its own thread. Chunks are queued
// (F9 → `sab_run.lua`; any change to `sab_cmd.lua`) and drained at the top of the engine's next
// `lua_pcall`, on the `L` the engine is about to use. The chunk is loaded and called above the
// engine's pushed function + args, and the stack is restored to that height afterwards.

type LuaPcallFn = unsafe extern "C" fn(*mut c_void, c_int, c_int, c_int) -> c_int;
type LuaLoadstringFn = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type LuaSettopFn = unsafe extern "C" fn(*mut c_void, c_int);
type LuaGettopFn = unsafe extern "C" fn(*mut c_void) -> c_int;
type LuaTolstringFn = unsafe extern "C" fn(*mut c_void, c_int, *mut usize) -> *const c_char;

static PCALL_ORIG: OnceLock<LuaPcallFn> = OnceLock::new();
static PENDING: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());
static HAS_PENDING: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

const LUA_MULTRET: c_int = -1;

/// Prefixed on the chunk's first line (line numbers in errors stay correct): `print` → `Dev.Log`.
const PRELUDE: &str = "local print=function(...) local t={} for i=1,select('#',...) do \
t[i]=tostring((select(i,...))) end Dev.Log(table.concat(t,'\\t')) end;";

/// `lua_tolstring(L, idx)` copied out (the engine's pointer is into a live, collectable TString).
unsafe fn lua_str(l: *mut c_void, idx: c_int) -> Option<String> {
    let tolstring: LuaTolstringFn = core::mem::transmute(addrs::LUA_TOLSTRING);
    let mut len = 0usize;
    let p = tolstring(l, idx, &mut len);
    if p.is_null() {
        return None;
    }
    let bytes = core::slice::from_raw_parts(p as *const u8, len);
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn queue_chunk(origin: &str, code: String) {
    PENDING.lock().unwrap_or_else(|p| p.into_inner()).push((origin.to_owned(), code));
    HAS_PENDING.store(true, Ordering::SeqCst);
}

unsafe fn run_chunk(l: *mut c_void, origin: &str, code: &str) {
    let loadstring: LuaLoadstringFn = core::mem::transmute(addrs::LUAL_LOADSTRING);
    let settop: LuaSettopFn = core::mem::transmute(addrs::LUA_SETTOP);
    let gettop: LuaGettopFn = core::mem::transmute(addrs::LUA_GETTOP);
    let pcall = PCALL_ORIG.get().unwrap();

    let Ok(src) = std::ffi::CString::new(format!("{PRELUDE}{code}")) else {
        log!("[console] {origin}: chunk contains a NUL byte, skipped");
        return;
    };
    let base = gettop(l);
    log!("[console] run {origin} ({} bytes)", code.len());
    let mut st = loadstring(l, src.as_ptr());
    if st == 0 {
        st = pcall(l, 0, LUA_MULTRET, 0);
    }
    if st != 0 {
        let msg = lua_str(l, -1).unwrap_or_else(|| "(no message)".into());
        log!("[console] error {st}: {msg}");
    } else {
        for i in base + 1..=gettop(l) {
            let v = lua_str(l, i).unwrap_or_else(|| "(non-string)".into());
            log!("[console] => {v}");
        }
    }
    settop(l, base);
}

unsafe extern "C" fn pcall_detour(l: *mut c_void, nargs: c_int, nres: c_int, errf: c_int) -> c_int {
    if (HAS_PENDING.load(Ordering::Relaxed) || bodydrag::has_commands()) && !RUNNING.swap(true, Ordering::SeqCst) {
        // the console thread fills PENDING: never block the Lua (game) thread on it; retry on the next pcall
        let jobs: Vec<_> = match PENDING.try_lock() {
            Ok(mut g) => {
                HAS_PENDING.store(false, Ordering::SeqCst);
                core::mem::take(&mut *g)
            }
            Err(std::sync::TryLockError::Poisoned(p)) => {
                HAS_PENDING.store(false, Ordering::SeqCst);
                core::mem::take(&mut *p.into_inner())
            }
            Err(std::sync::TryLockError::WouldBlock) => Vec::new(),
        };
        {
            let _t = bodydrag::perf::Timer::new("Lua commands (PlayAnimation, sounds, console)", bodydrag::perf::Kind::Ours);
            for (origin, code) in &jobs {
                run_chunk(l, origin, code);
            }
            bodydrag::drain_commands(l);
        }
        RUNNING.store(false, Ordering::SeqCst);
    }
    (PCALL_ORIG.get().unwrap())(l, nargs, nres, errf)
}

#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vkey: c_int) -> i16;
    fn GetForegroundWindow() -> *mut c_void;
    fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
}

// Hotkeys: only keys the game does not bind (InputTemplates.txt binds F1-F6; F6 = quickload). The body-drag action
// key comes from bodydrag::input (default F7, `action_key` in sab_bodydrag.ini), the same binding its HUD prompt shows.
const VK_F9: c_int = 0x78;
const VK_F10: c_int = 0x79;

fn game_has_focus() -> bool {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
    pid == std::process::id()
}

fn game_file(name: &str) -> std::path::PathBuf {
    log_path().with_file_name(name)
}

fn mtime(p: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// Polls F9 (game focused → queue `sab_run.lua`), F10 (15 s camera capture) and `sab_cmd.lua`
/// (queued whenever its mtime changes after attach; the version present at startup is not run).
fn console_thread() {
    let mut f7_was_down = false;
    let action = bodydrag::input::binding();
    log!("[console] body-drag action key {} (vk 0x{:02x}; {})", action.label, action.vk, action.note);
    #[cfg(feature = "dev")]
    let mut dev = DevConsole::new();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));

        let f7_down = unsafe { GetAsyncKeyState(action.vk) } as u16 & 0x8000 != 0;
        if f7_down && !f7_was_down && game_has_focus() {
            bodydrag::request_toggle();
        }
        f7_was_down = f7_down;

        #[cfg(feature = "dev")]
        dev.poll();
    }
}

/// Developer console (dev builds only): F9 runs sab_run.lua (e.g. the debug body spawn), F10 starts a camera
/// capture, and saving sab_cmd.lua runs it.
#[cfg(feature = "dev")]
struct DevConsole {
    run_file: std::path::PathBuf,
    cmd_file: std::path::PathBuf,
    last_cmd: Option<std::time::SystemTime>,
    f9_was_down: bool,
    f10_was_down: bool,
}

#[cfg(feature = "dev")]
impl DevConsole {
    fn new() -> Self {
        let cmd_file = game_file("sab_cmd.lua");
        DevConsole { run_file: game_file("sab_run.lua"), last_cmd: mtime(&cmd_file), cmd_file, f9_was_down: false, f10_was_down: false }
    }

    fn poll(&mut self) {
        let f9_down = unsafe { GetAsyncKeyState(VK_F9) } as u16 & 0x8000 != 0;
        if f9_down && !self.f9_was_down && game_has_focus() {
            match std::fs::read_to_string(&self.run_file) {
                Ok(code) => queue_chunk("sab_run.lua (F9)", code),
                Err(e) => log!("[console] F9: cannot read {}: {e}", self.run_file.display()),
            }
        }
        self.f9_was_down = f9_down;

        let f10_down = unsafe { GetAsyncKeyState(VK_F10) } as u16 & 0x8000 != 0;
        if f10_down && !self.f10_was_down && game_has_focus() {
            camlog_start(15.0);
        }
        self.f10_was_down = f10_down;

        let now = mtime(&self.cmd_file);
        if now.is_some() && now != self.last_cmd {
            self.last_cmd = now;
            // The writer may still be mid-save; give it a moment.
            std::thread::sleep(std::time::Duration::from_millis(150));
            match std::fs::read_to_string(&self.cmd_file) {
                Ok(code) if !code.trim().is_empty() => queue_chunk("sab_cmd.lua", code),
                Ok(_) => {}
                Err(e) => log!("[console] cannot read {}: {e}", self.cmd_file.display()),
            }
        }
    }
}

// ─────────────────── camera log: WSGameCamera::Tick → sab_camlog_<ms>.csv ───────────────────
//
// While a capture runs (F10 or `Dev.CamLog(s)`), every Tick of the active WSGameCamera
// (`this == [0x1321b74]`, `[0x1321b78] >= 1`) appends one row after the original Tick returns: time,
// camera pointer, dt, FOV, mouse buttons, camera position and row-2 axis, RotSpeed, PitchSpeed, live
// transitions. Reads go through `this` only (the object the engine just updated). Each capture writes
// a new file; rows are buffered and flushed when the capture ends. The hook never blocks: if the
// capture state is busy the row is dropped.

type LuaToNumberFn = unsafe extern "C" fn(*mut c_void, c_int) -> f32;
type GameCameraTickFn = unsafe extern "thiscall" fn(*mut c_void, f32) -> *mut c_void;
static TICK_ORIG: OnceLock<GameCameraTickFn> = OnceLock::new();

static START: OnceLock<std::time::Instant> = OnceLock::new();
/// Capture end, in ms since attach; 0 = not capturing.
static CAMLOG_UNTIL: AtomicU64 = AtomicU64::new(0);
static CAMLOG: Mutex<Option<std::io::BufWriter<std::fs::File>>> = Mutex::new(None);

fn now_ms() -> u64 {
    START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

fn camlog_slot() -> std::sync::MutexGuard<'static, Option<std::io::BufWriter<std::fs::File>>> {
    CAMLOG.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn camlog_start(secs: f32) {
    let path = game_file(&format!("sab_camlog_{}.csv", now_ms()));
    match OpenOptions::new().create_new(true).write(true).open(&path) {
        Ok(f) => {
            let mut w = std::io::BufWriter::new(f);
            let _ = writeln!(w, "t_ms,cam,dt,fov_deg,rmb,lmb,px,py,pz,ax,ay,az,rot,pitch,ntrans");
            let mut slot = camlog_slot();
            if let Some(mut old) = slot.take() {
                let _ = old.flush();
            }
            *slot = Some(w);
            CAMLOG_UNTIL.store(now_ms() + (secs.clamp(1.0, 600.0) * 1000.0) as u64, Ordering::SeqCst);
            log!("[camlog] capturing {secs} s to {}", path.display());
        }
        Err(e) => log!("[camlog] cannot open {}: {e}", path.display()),
    }
}

const VK_LBUTTON: c_int = 0x01;
const VK_RBUTTON: c_int = 0x02;

fn key_down(vk: c_int) -> u8 {
    (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0) as u8
}

unsafe fn camlog_row(cam: usize, dt: f32, t: u64, w: &mut std::io::BufWriter<std::fs::File>) {
    let count = core::ptr::read_volatile(addrs::GAME_CAMERA_COUNT as *const i32);
    let active = core::ptr::read_volatile(addrs::GAME_CAMERA_PTR as *const u32) as usize;
    if count < 1 || active == 0 || active != cam {
        return;
    }
    let rf = |o: usize| core::ptr::read_unaligned((cam + o) as *const f32);
    let m = addrs::GC_MATRIX;
    let ntrans = core::ptr::read_unaligned((cam + addrs::GC_TRANSITIONS) as *const i32);
    let _ = writeln!(
        w,
        "{t},0x{cam:08x},{dt:.5},{:.4},{},{},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{:.4},{ntrans}",
        rf(addrs::GC_FOV).to_degrees(), key_down(VK_RBUTTON), key_down(VK_LBUTTON),
        rf(m + 0x30), rf(m + 0x34), rf(m + 0x38), rf(m + 0x20), rf(m + 0x24), rf(m + 0x28),
        rf(addrs::GC_ROT_SPEED), rf(addrs::GC_PITCH_SPEED)
    );
}

unsafe extern "thiscall" fn tick_detour(cam: *mut c_void, dt: f32) -> *mut c_void {
    let ret = match TICK_ORIG.get() {
        Some(orig) => orig(cam, dt),
        None => return core::ptr::null_mut(),
    };
    if cam as usize == rd32(addrs::GAME_CAMERA_PTR) as usize {
        bodydrag::frame(dt);
    }
    let until = CAMLOG_UNTIL.load(Ordering::Relaxed);
    if until != 0 && !cam.is_null() {
        let t = now_ms();
        let mut slot = match CAMLOG.try_lock() {
            Ok(g) => g,
            Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return ret,
        };
        if t > until {
            if let Some(mut w) = slot.take() {
                let _ = w.flush();
            }
            CAMLOG_UNTIL.store(0, Ordering::SeqCst);
            log!("[camlog] capture finished");
        } else if let Some(w) = slot.as_mut() {
            camlog_row(cam as usize, dt, t, w);
        }
    }
    ret
}

// ─────────────────────────── install / entry ───────────────────────────

use minhook_sys::{MH_CreateHook, MH_EnableHook, MH_Initialize, MH_OK};

unsafe fn create(target: usize, detour: usize) -> Result<*mut c_void, String> {
    let mut orig: *mut c_void = core::ptr::null_mut();
    let st = MH_CreateHook(target as *mut c_void, detour as *mut c_void, &mut orig);
    if st != MH_OK {
        return Err(format!("MH_CreateHook(0x{target:08x}) = {st}"));
    }
    Ok(orig)
}

/// First bytes of every engine function this plugin hooks or calls, from the retail GOG `Saboteur.exe`
/// (sha256 628cdbb6...; identical in the user's patched exe 15f5ef3f, whose patches lie elsewhere). Any other build
/// (Steam, other patches) has different code at these addresses: then nothing is hooked or called.
const EXPECTED_CODE: [(&str, usize, [u8; 8]); 20] = [
    ("Lua binding publish", 0x006f_8a90, [0x53, 0x55, 0x56, 0x57, 0x8b, 0xf1, 0x33, 0xff]),
    ("lua_pcall", 0x0040_8310, [0x55, 0xeb, 0x13, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc]),
    ("WSGameCamera tick", 0x0067_1ae0, [0x55, 0x8b, 0xec, 0x83, 0xec, 0x08, 0x89, 0x4d]),
    ("ragdoll update", 0x0059_0db0, [0x55, 0x8b, 0xec, 0x83, 0xe4, 0xf0, 0x64, 0xa1]),
    ("go-ragdoll", 0x004f_fa50, [0x81, 0xec, 0x84, 0x00, 0x00, 0x00, 0x56, 0x8b]),
    ("ragdoll body setter", 0x0058_ea90, [0x55, 0x8b, 0xec, 0x83, 0xe4, 0xf0, 0x8b, 0x81]),
    ("ragdoll state acquire flag", 0x0057_5b20, [0x8a, 0x44, 0x24, 0x04, 0x88, 0x81, 0xbd, 0x00]),
    ("rigid body activate", 0x00aa_8840, [0x51, 0x56, 0x8b, 0xf1, 0x8b, 0x8e, 0xc8, 0x00]),
    ("item in hand", 0x0049_97f0, [0x8b, 0x51, 0x04, 0x57, 0x8b, 0x7a, 0x04, 0x33]),
    ("HUD object", 0x009b_bb20, [0x8b, 0x44, 0x24, 0x04, 0x8b, 0x44, 0x81, 0x34]),
    ("HUD prompt add", 0x0077_0870, [0x56, 0x8b, 0xf1, 0x83, 0xbe, 0x88, 0x0c, 0x00]),
    ("HUD prompt update", 0x0077_09f0, [0xe9, 0xc6, 0x1a, 0xc9, 0xff, 0x90, 0x56, 0x57]),
    ("HUD prompt exists", 0x0076_fa00, [0x8b, 0x54, 0x24, 0x04, 0x83, 0xfa, 0xff, 0x74]),
    ("HUD prompt clear", 0x0077_0ee0, [0x56, 0x8b, 0x74, 0x24, 0x08, 0x83, 0xfe, 0xff]),
    ("luaL_openlib", 0x015f_d2d0, [0x55, 0xeb, 0x13, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc]),
    ("luaL_loadstring", 0x015f_e1a0, [0x55, 0xeb, 0x10, 0xcc, 0xcc, 0xcc, 0xcc, 0xcc]),
    ("lua_settop", 0x0040_11e0, [0xe9, 0x4b, 0xb3, 0x1f, 0x01, 0x90, 0xcc, 0xcc]),
    ("lua_gettop", 0x0040_11d0, [0x8b, 0x4c, 0x24, 0x04, 0x8b, 0x41, 0x08, 0x2b]),
    ("lua_tolstring", 0x006a_c630, [0x53, 0x55, 0x8b, 0x6c, 0x24, 0x10, 0x56, 0x57]),
    ("lua_tonumber", 0x0040_1510, [0x8b, 0x4c, 0x24, 0x08, 0x8b, 0x54, 0x24, 0x04]),
];

/// The names of the functions whose code differs from the supported build (empty = supported).
unsafe fn unsupported_code() -> Vec<String> {
    EXPECTED_CODE
        .iter()
        .filter(|(_, va, want)| core::slice::from_raw_parts(*va as *const u8, want.len()) != want)
        .map(|(name, va, _)| format!("{name} @0x{va:08x}"))
        .collect()
}

unsafe fn install() -> Result<(), String> {
    let bad = unsupported_code();
    if !bad.is_empty() {
        return Err(format!(
            "unsupported Saboteur.exe (this mod is built for the retail GOG executable); code differs at {} - nothing installed, the game runs unmodified",
            bad.join(", ")
        ));
    }
    if MH_Initialize() != MH_OK {
        return Err("MH_Initialize failed".into());
    }
    // research probe (combined-LOD body parts), not needed by body drag: it runs on every body setup, takes a lock,
    // scans a growing list and writes a log line per new object. Dev builds, opt-in with `sab_probe_bodysetup.on`.
    if cfg!(feature = "dev") && game_file("sab_probe_bodysetup.on").exists() {
        let orig = create(addrs::BODY_SETUP, body_setup_detour as *const () as usize)?;
        BODY_ORIG.set(core::mem::transmute::<*mut c_void, BodySetupFn>(orig)).ok();
        log!("bodysetup probe enabled (sab_probe_bodysetup.on present)");
    }

    // the Dev.* Lua table: dev builds only
    if cfg!(feature = "dev") {
        let orig = create(addrs::REGISTER_BINDINGS, reg_bindings_detour as *const () as usize)?;
        REG_ORIG.set(core::mem::transmute::<*mut c_void, RegBindingsFn>(orig)).ok();
    }

    let orig = create(addrs::LUA_PCALL, pcall_detour as *const () as usize)?;
    PCALL_ORIG.set(core::mem::transmute::<*mut c_void, LuaPcallFn>(orig)).ok();

    const TICK_PROLOGUE: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x08];
    let seen = core::slice::from_raw_parts(addrs::GAME_CAMERA_TICK as *const u8, TICK_PROLOGUE.len());
    if seen == TICK_PROLOGUE {
        let orig = create(addrs::GAME_CAMERA_TICK, tick_detour as *const () as usize)?;
        TICK_ORIG.set(core::mem::transmute::<*mut c_void, GameCameraTickFn>(orig)).ok();
    } else {
        log!("[camlog] WSGameCamera::Tick prologue at 0x671ae0 is {seen:02x?}, not the GOG bytes; camera log disabled");
    }

    // WSHumanRagdoll update (body drag): push ebp; mov ebp,esp; and esp,-16.
    const RAGDOLL_PROLOGUE: [u8; 6] = [0x55, 0x8B, 0xEC, 0x83, 0xE4, 0xF0];
    let seen = core::slice::from_raw_parts(bodydrag::RAGDOLL_UPDATE as *const u8, RAGDOLL_PROLOGUE.len());
    if seen == RAGDOLL_PROLOGUE {
        let orig = create(bodydrag::RAGDOLL_UPDATE, bodydrag::update_detour as *const () as usize)?;
        bodydrag::ORIG.set(core::mem::transmute::<*mut c_void, bodydrag::RagdollUpdateFn>(orig)).ok();
    } else {
        log!("[bodydrag] ragdoll update prologue at 0x590db0 is {seen:02x?}, not the GOG bytes; body drag disabled");
    }

    // MH_ALL_HOOKS == null.
    if MH_EnableHook(core::ptr::null_mut()) != MH_OK {
        return Err("MH_EnableHook failed".into());
    }
    Ok(())
}

/// Logs above this size are moved to `<name>.old.txt` at startup (one previous log kept).
const LOG_ROTATE_BYTES: u64 = 5 * 1024 * 1024;

fn init() {
    let path = log_path();
    if std::fs::metadata(&path).map_or(false, |m| m.len() > LOG_ROTATE_BYTES) {
        let old = path.with_extension("old.txt");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(&path, &old);
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(&path) {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = LOG.set(tx);
        std::thread::spawn(move || log_writer(rx, f));
    }
    log!("═══ sab_asi attached ═══ ({} build)", if cfg!(feature = "dev") { "dev" } else { "release" });
    // which build produced this session: FNV-1a 64 of the installed file (bodydrag/tools/carry_report.py maps it to a
    // versions/ folder by the same hash)
    match std::fs::read(game_file("scripts\\sab_asi.asi")) {
        Ok(bytes) => {
            let h = bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3));
            log!("build fnv64 {h:016x} ({} bytes)", bytes.len());
        }
        Err(e) => log!("build fnv64 unknown (scripts\\sab_asi.asi: {e})"),
    }
    match unsafe { install() } {
        Ok(()) => {
            log!(
                "hooks installed: lua_pcall@0x408310, gamecamera_tick@0x671ae0, ragdoll_update@0x590db0 (body drag){}",
                if cfg!(feature = "dev") { "; dev: regbindings@0x6f8a90 (Dev.*), F9/F10/sab_cmd.lua console, bodysetup probe with sab_probe_bodysetup.on" } else { "" }
            );
            std::thread::spawn(console_thread);
        }
        Err(e) => log!("INSTALL FAILED: {e}"),
    }
}

/// Standard cdylib entry. Do no real work under the loader lock — spin up a thread.
#[no_mangle]
pub extern "system" fn DllMain(_module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    const DLL_PROCESS_ATTACH: u32 = 1;
    const DLL_PROCESS_DETACH: u32 = 0;
    if reason == DLL_PROCESS_ATTACH {
        std::thread::spawn(init);
    } else if reason == DLL_PROCESS_DETACH {
        // evidence for exit hangs: reaching this line means the game got as far as ExitProcess / unload. Only a
        // bounded log write (no thread creation, no waiting under the loader lock).
        // written directly: at process exit the log writer thread has already been terminated
        log_direct(&format!(
            "═══ sab_asi detach ({}) — {} ═══",
            if _reserved.is_null() { "FreeLibrary" } else { "process exit" }, bodydrag::exit_summary()
        ));
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_writer_keeps_order_and_flushes_when_idle() {
        let path = std::env::temp_dir().join(format!("sab_asi_logtest_{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let f = OpenOptions::new().create(true).append(true).open(&path).unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let t0 = std::time::Instant::now();
        for i in 0..1000 {
            tx.send(format!("line {i}")).unwrap();
        }
        let sent_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let writer = std::thread::spawn(move || log_writer(rx, f));
        // flushed once the queue is empty, while the sender is still alive
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let n = std::fs::read_to_string(&path).map(|s| s.lines().count()).unwrap_or(0);
            if n == 1000 || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        drop(tx);
        writer.join().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1000);
        assert!(lines.iter().enumerate().all(|(i, l)| *l == format!("line {i}")), "order kept");
        assert!(sent_ms < 50.0, "sending 1000 lines took {sent_ms:.1} ms on the caller");
        let _ = std::fs::remove_file(&path);
    }
}
