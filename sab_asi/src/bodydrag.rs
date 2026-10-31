//! Body drag v9 (Velvet-geometry underarm grip + integral sag correction, camera-steered S/W, A/D turn): F7 grabs a ragdolled body whose torso is within reach; Sean plays qol_drag_in, then qol_drag_idle,
//! and while W is held qol_drag_walk (backward root motion from its MINA summary); F7 drops the body and plays
//! qol_drag_out. The body is moved by velocity only (never teleported).
//! Notes: saboteur-camera-mod/bodydrag (FINDINGS.md, AGENT_MOVE_NOTES.md, versions/README.md).
//!
//! Engine facts used (static RE in the bodydrag notes; confirmed in game 2026-10-27 unless marked):
//! - WSHumanRagdoll update `0x590db0`: thiscall(this, f32 dt, u32, void*), `ret 0xc`; called from the human
//!   update at `0x4fa421`, which stops running for a corpse once it settles. Owner human = `[this+0x348]`; the
//!   human's ragdoll back-pointer is `[human+0x10d0]`.
//! - Human state id `[human+0x123c]` (2 Animate, 3 Ragdoll, 0xf Dying), state object `[human+0x1238]`; Actor.PlayAnimation
//!   switches the player to a new WSHumanStateAnimate whose anim request is `[state+0x18]` (static RE). Player =
//!   `[0x1240328]`.
//! - While the Animate state runs, input is ignored and Sean moves only by the clip's MINA root-motion summary (static RE;
//!   matches the measured drift of the first animated build and the zero drift of the stationary build).
//! - Human position: motion component `human+0x950`, vfunc `+0x28` returns `*const [f32; 3]` (caller 0x4fa3b1).
//! - Heading (logged): float at `[human+0x10cc]+0x494`, forward = (sin, 0, cos) (`0x490fc0`, static RE).
//! - hkaRagdollInstance `[this+0xac]`: bodies `[+8]`, count `[+0xc]`, bone->body map `[+0x20]` (count `[+0x24]`),
//!   skeleton `[+0x2c]`; NPC ragdoll bones are named RB_HIP, RB_TORSO, RB_HEAD, RB_LEFT_SHOULDER, ...
//! - hkpRigidBody: motion at `+0xe0` (motion type byte `+0xe8`, 7 = FIXED), position `+0x120`; hkpEntity::activate
//!   `0xaa8840` (thiscall); motion vtable `+0x40` setLinearVelocity (thiscall(motion, const hkVector4*), `ret 4`).
//!
//! Interaction model: every grab starts a new generation. Animation commands are queued with their generation and run
//! from the lua_pcall hook (`drain_commands`); a command whose generation is no longer current is dropped there. A queued
//! request counts as started only when Sean's state object / request pointer change to a fresh Animate state; phase
//! timing uses a game clock built from the camera tick's dt (stops while the game is paused). The hold is aborted
//! without a drop animation or cancel if the player object changes, Sean ragdolls or dies, or a looping drag request is
//! replaced by something else. The post-drop CancelAnimation is sent only if our qol_drag_out request is still active.

use core::ffi::c_void;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

pub(crate) mod input;
mod lifecycle;
pub(crate) mod perf;
mod prompt;
mod voice;

pub const RAGDOLL_UPDATE: usize = 0x0059_0db0;
const PLAYER_PTR: u32 = 0x0124_0328;
const ACTIVATE: usize = 0x00aa_8840;
const STATE_ANIMATE: u32 = 2;
const STATE_RAGDOLL: u32 = 3;
const STATE_DYING: u32 = 0xf;
// Ragdoll objects are pooled (FUN_00590940). A dead human's WSHumanStateRagdoll acquires one only once: its Update
// (0x577d80) calls FUN_004f8010 while [state+0xbd] == 0 and sets the flag on success. The engine later takes the object
// back - FUN_00590db0 frees it when the ragdoll is > 7 s old, at rest (|v|^2 < 0.001) and Sean > 5 m away; FUN_00590940
// steals the longest-idle one for a new ragdoll - and the corpse stays in state 3 as a static pose with the flag set.
// Such a frozen corpse is refused: re-acquiring a ragdoll for it (clearing the flag) gave it a pooled object whose
// bodies were still where that object was last used (9-137 m away in game), so the corpse T-posed and despawned.
const STATE_RAGDOLL_VTABLE: u32 = 0x00fa_b54c; // WSHumanStateRagdoll (RTTI .?AVWSHumanStateRagdoll@@)

const FIND_RADIUS: f32 = 2.0; // m, player to pelvis to consider a body
const REACH_H: f32 = 1.6; // m, horizontal player-to-torso distance allowed at the grab (1.3 refused 1.4 m grabs)
const REACH_V: f32 = 1.0; // m, vertical player-to-torso distance allowed at the grab
const ROPE: f32 = 1.1; // m, pelvis rope length when the ragdoll has no torso bone
// Torso target (unchanged from the stationary build): GRIP_REACH toward the body, height ramping GRIP_LOW -> GRIP_HIGH
// over GRIP_RAMP_MS of the grab animation.
const GRIP_REACH: f32 = 0.6; // 0.45 put the torso inside Sean's own body width (user: the body clips into him)
const GRIP_HIGH: f32 = 1.15;
const GRIP_LOW: f32 = 0.35;
const GRIP_RAMP_MS: f64 = 1600.0;
// Underarm grip (two shoulder bodies to Sean's hands, in Sean's frame). Geometry from Velvet's own drag pair (victim
// half of Drag_Idle / Drag_Walk, scaled to Sean like the dragger): the victim's upper-arm joints sit ~0.16 m ABOVE the
// dragger's hands, ~0.05 m closer to him, +/-0.24 m wide; the spine is 69-79 deg above horizontal leaning back on the
// dragger; head at his upper chest; pelvis 0.7-0.8 m up with the legs trailing. Sean's baked hands are ~1.05-1.14 m up,
// 0.22-0.37 m in front of his root, so the shoulder targets are 1.22 m up, 0.32 m out, +/-0.22 m.
const HAND_FWD: f32 = 0.32;
const HAND_HALF_SPAN: f32 = 0.22;
const HAND_HIGH: f32 = 1.22;
const HAND_LOW: f32 = 0.35;
const SH_GAIN: f32 = 8.0; // 1/s
const SH_MAX_SPEED: f32 = 5.0; // m/s
const GRAVITY: f32 = 9.81;
const SPINE_PITCH: f32 = 72.0; // deg above horizontal for the corpse's spine, pointing back toward Sean (Velvet 69-79)
const SH_I_GAIN: f32 = 6.0; // 1/s^2: integral of the vertical shoulder error (v8 log: steady ~0.2 m sag with P only)
const SH_I_MAX: f32 = 3.0; // m/s cap on that integral
const SPIN_GAIN: f32 = 4.0; // rad/s per unit (sine) of orientation error
const SPIN_MAX: f32 = 3.0; // rad/s
const SEAN_PELVIS_H: f32 = 1.0; // m above his feet
const CLEAR_H: f32 = 0.8; // head lower than this above Sean's feet and closer than CLEAR_R = in front of his pelvis
const CLEAR_R: f32 = 0.55;
const GIVE_UP: f32 = 6.0; // m, auto release beyond this
const GAIN: f32 = 6.0; // 1/s, velocity per metre of error
const MAX_SPEED: f32 = 7.0; // m/s
const KNOWN_MAX: usize = 96; // entries are dropped when their entity fails `human_ok`, never by age
const DIAG_MS: f64 = 250.0;

const IN_MS: f64 = 1833.0; // qol_drag_in (STOP)
const OUT_MS: f64 = 1667.0; // qol_drag_out (STOP; Velvet Drag_Out at its own speed, 51 frames at 30 fps)
// Release inside qol_drag_out (clip seconds from its activation, game clock): the grip follows RELEASE_CURVE at full
// support until RELEASE_LET_S (last frame Velvet's hands stay at the held distance from the victim's upper arms), the
// support then fades linearly to zero at RELEASE_S (Velvet's own marker, Drag_Out.txt "1.00 - ragdoll"), where the body
// is detached; the clearance push keeps running until the clip ends.
const RELEASE_LET_S: f32 = 0.80;
const RELEASE_S: f32 = 1.00;
const RELEASE_FPS: f32 = 30.0;
// Per frame of qol_drag_out: shoulder target delta (forward, up) from the held pose, and the corpse spine direction in
// Sean's frame (forward, up). Generated by bodydrag/animpack/release_curve.py from the baked clip (Sean's hand midpoint
// to frame 24) and Velvet's victim (after frame 24, and the spine).
#[rustfmt::skip]
const RELEASE_CURVE: [[f32; 4]; 51] = [
    [0.0, 0.0, -0.188, 0.982], [0.002, -0.006, -0.197, 0.98], [0.001, -0.01, -0.211, 0.977],
    [0.0, -0.015, -0.227, 0.974], [-0.002, -0.021, -0.244, 0.97], [-0.002, -0.022, -0.254, 0.967],
    [-0.001, -0.029, -0.257, 0.967], [0.003, -0.043, -0.247, 0.969], [0.006, -0.061, -0.224, 0.974],
    [0.005, -0.081, -0.193, 0.981], [0.002, -0.103, -0.153, 0.988], [-0.002, -0.125, -0.106, 0.994],
    [-0.001, -0.126, -0.054, 0.999], [0.001, -0.125, 0.00113, 1.0], [0.012, -0.086, 0.0627, 0.998],
    [0.033, -0.011, 0.129, 0.992], [0.059, 0.045, 0.196, 0.981], [0.082, 0.074, 0.261, 0.965],
    [0.103, 0.103, 0.321, 0.947], [0.124, 0.126, 0.375, 0.927], [0.146, 0.137, 0.42, 0.907],
    [0.171, 0.119, 0.454, 0.891], [0.197, 0.067, 0.479, 0.878], [0.231, -0.014, 0.501, 0.865],
    [0.256, -0.111, 0.511, 0.859], [0.284, -0.172, 0.511, 0.859], [0.313, -0.233, 0.512, 0.859],
    [0.342, -0.304, 0.519, 0.855], [0.379, -0.37, 0.528, 0.849], [0.413, -0.433, 0.531, 0.847],
    [0.445, -0.495, 0.532, 0.847], [0.478, -0.559, 0.535, 0.845], [0.516, -0.562, 0.54, 0.842],
    [0.559, -0.569, 0.552, 0.834], [0.607, -0.554, 0.573, 0.82], [0.661, -0.547, 0.604, 0.797],
    [0.694, -0.574, 0.64, 0.769], [0.731, -0.616, 0.691, 0.723], [0.761, -0.656, 0.734, 0.68],
    [0.788, -0.698, 0.776, 0.631], [0.801, -0.735, 0.811, 0.585], [0.81, -0.735, 0.811, 0.585],
    [0.818, -0.735, 0.811, 0.585], [0.827, -0.734, 0.811, 0.585], [0.836, -0.734, 0.811, 0.586],
    [0.844, -0.734, 0.81, 0.586], [0.853, -0.734, 0.81, 0.586], [0.861, -0.733, 0.81, 0.586],
    [0.87, -0.733, 0.81, 0.586], [0.878, -0.733, 0.81, 0.586], [0.887, -0.733, 0.81, 0.587],
];
const STOP_END_EARLY_MS: f64 = 250.0; // a STOP clip leaving the Animate state this close to its end = native completion
const ACTIVATE_TIMEOUT_MS: f64 = 1000.0; // queued request not seen starting: continue on the timer and log it
const OUT_CANCEL_GRACE_MS: f64 = 500.0; // after qol_drag_out's length: cancel only if our request is still active
const START_DEBOUNCE_MS: f64 = 80.0; // a move/turn key must be held this long before its clip starts (filters taps)
// clip lengths (pack v17 bake): walk and its 30 deg arcs at 1x, the 45 deg effort arcs at 1.2x, pivots at 1.25x
const WALK_LOOP_MS: f64 = 733.0;
const EFFORT_LOOP_MS: f64 = 611.0;
const PIVOT_LOOP_MS: f64 = 587.0;
const LOOP_REISSUE_EARLY_MS: f64 = 20.0;
// Steering tiers by the heading error, each with hysteresis (enter / exit, deg):
//   straight walk below STEER; 30 deg/loop arc (41 deg/s, 1.67 m/s) up to EFFORT; 45 deg/loop effort arc at 1.2x
//   (74 deg/s, 2.0 m/s) up to PIVOT; pivot in place (45 deg/loop at 1.25x = 77 deg/s) beyond PIVOT.
const STEER_ENTER: f32 = 12.0;
const STEER_EXIT: f32 = 4.0;
const EFFORT_ENTER: f32 = 25.0;
const EFFORT_EXIT: f32 = 15.0;
// The effort arc swings the body on a ~1.6 m radius, so near-reversals still start with a short pivot.
const PIVOT_ENTER: f32 = 150.0;
const PIVOT_EXIT: f32 = 110.0;
// Soft separation so the body does not pass through Sean: any of its rigid bodies within SEP_R of his vertical axis
// (between his feet and SEP_H above them) gets an outward velocity added, proportional to the overlap.
const SEP_R: f32 = 0.28; // Sean's body radius; 0.42 kept the body away from him (Velvet: victim rests against the dragger)
const SEP_H: f32 = 1.9;
const SEP_GAIN: f32 = 8.0;
const RECOVER_WINDOW_MS: f64 = 2000.0;
// Releasing W stops at once (no debounce): at 1.67 m/s every 100 ms of delay is ~17 cm of extra travel.

pub type RagdollUpdateFn = unsafe extern "thiscall" fn(*mut c_void, f32, u32, *mut c_void) -> u32;
pub static ORIG: std::sync::OnceLock<RagdollUpdateFn> = std::sync::OnceLock::new();

/// Game clock in ms (sum of camera-tick dt), as f64 bits.
static CLOCK: AtomicU64 = AtomicU64::new(0);
/// Current interaction generation; commands carrying another generation are dropped.
static GEN: AtomicU32 = AtomicU32::new(0);
/// GetTickCount at the F7 press (0 = none); set by the console thread, consumed by `frame` on the game thread.
static TOGGLE_MS: AtomicU32 = AtomicU32::new(0);
/// Frame counter (camera ticks) and the frame of the last pull, so a body is pulled at most once per frame.
static FRAME: AtomicU32 = AtomicU32::new(0);
static PULLED_FRAME: AtomicU32 = AtomicU32::new(u32::MAX);

static COMMANDS: Mutex<Vec<(u32, String, String)>> = Mutex::new(Vec::new());
static HAS_COMMANDS: AtomicBool = AtomicBool::new(false);

fn clock() -> f64 {
    f64::from_bits(CLOCK.load(Ordering::Relaxed))
}

/// A ragdoll seen in the ragdoll update, with the identity fields re-checked before any later use.
#[derive(Clone, Copy)]
struct Body {
    owner: u32,
    vt: u32, // the owner's vtable at registration (identity check against a freed / reused address)
    rd: u32,
    body: u32,
    pos: [f32; 3],
    ms: u32,
}
static KNOWN: Mutex<Vec<Body>> = Mutex::new(Vec::new());

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    In,
    Idle,
    Walk,  // drag backwards (qol_drag_walk, MINA travel -1.222 m per 0.733 s)
    TurnL, // pivot: idle pose with root yaw +45 deg per 0.587 s (MINA unk0[4], signed)
    TurnR, // pivot: root yaw -45 deg per 0.587 s
    WalkL, // steering: backward walk + yaw +30 deg per loop (arc)
    WalkR, // steering: backward walk + yaw -30 deg per loop
    HardL, // effort arc: backward walk at 1.2x (1.222 m per 0.611 s) + yaw +45 deg per loop
    HardR, // effort arc: yaw -45 deg per loop
    Out,
}

impl Phase {
    fn clip(self) -> &'static str {
        match self {
            Phase::In => "qol_drag_in",
            Phase::Idle => "qol_drag_idle",
            Phase::Walk => "qol_drag_walk",
            Phase::TurnL => "qol_drag_turn_l",
            Phase::TurnR => "qol_drag_turn_r",
            Phase::WalkL => "qol_drag_walk_l",
            Phase::WalkR => "qol_drag_walk_r",
            Phase::HardL => "qol_drag_walk_hl",
            Phase::HardR => "qol_drag_walk_hr",
            Phase::Out => "qol_drag_out",
        }
    }
    /// Length of one play-through of a looping hold clip.
    fn loop_ms(self) -> f64 {
        match self {
            Phase::TurnL | Phase::TurnR => PIVOT_LOOP_MS,
            Phase::HardL | Phase::HardR => EFFORT_LOOP_MS,
            _ => WALK_LOOP_MS,
        }
    }
    fn is_hold(self) -> bool {
        !matches!(self, Phase::In | Phase::Out)
    }
    /// Clips whose root yaw the engine applies only over their first play-through (v6 log: turning stopped after
    /// ~30 deg while the key stayed down); these are re-requested at each loop end.
    fn rotates(self) -> bool {
        matches!(self, Phase::TurnL | Phase::TurnR | Phase::WalkL | Phase::WalkR | Phase::HardL | Phase::HardR)
    }
    fn is_walk(self) -> bool {
        matches!(self, Phase::Walk | Phase::WalkL | Phase::WalkR | Phase::HardL | Phase::HardR)
    }
    fn stop_ms(self) -> Option<f64> {
        match self {
            Phase::In => Some(IN_MS),
            Phase::Out => Some(OUT_MS),
            _ => None,
        }
    }
}

/// Sean's animation state: (state id, state object, its anim request `[state+0x18]`).
#[derive(Clone, Copy, PartialEq, Debug)]
struct AnimState(u32, u32, u32);

unsafe fn anim_state(p: u32) -> AnimState {
    let st = r32(p + 0x123c).unwrap_or(u32::MAX);
    let obj = r32(p + 0x1238).unwrap_or(0);
    let req = if st == STATE_ANIMATE { r32(obj + 0x18).unwrap_or(0) } else { 0 };
    AnimState(st, obj, req)
}

/// Our latest animation request: what was queued and when, Sean's state before it, and (once seen) the Animate state
/// it started.
#[derive(Clone, Copy)]
struct Request {
    phase: Phase,
    queued: f64,
    before: AnimState,
    active: Option<(AnimState, f64)>,
}

/// Movement latency probe for one W press or release.
#[derive(Clone, Copy)]
struct Probe {
    start: bool,
    detected: f64,
    detected_pos: [f32; 3],
}

struct Interaction {
    gen: u32,
    player: u32,
    hold: Option<(Body, Grip)>,
    frame_dt: f64,
    sh_i: [f32; 2], // vertical integral term per shoulder (right, left)
    grab_clock: f64,
    req: Request,
    pending: Option<(Phase, f64)>,
    probe: Option<Probe>,
    trail: VecDeque<(f64, [f32; 3])>,
    last_diag: f64,
    cancel_queued: bool,
    voice: voice::Voice,
    last_recover: f64,
    /// The body after its detachment at RELEASE_S, kept clear of Sean until qol_drag_out ends.
    clear: Option<Body>,
    /// Posture verdicts of the settled hold samples (good, total) and the first reason the underarm grip was not used.
    posture: (u32, u32),
    downgrade: Option<String>,
    /// This frame's carry diagnostic will be logged (the line is built only then).
    want_diag: bool,
}
static INTER: Mutex<Option<Interaction>> = Mutex::new(None);

/// Posture acceptance for a settled underarm sample (grab ramp complete, not releasing): shoulders near their targets
/// and the corpse oriented as intended. A small target distance alone does not mean the pose is right (a reversed
/// corpse has both shoulders at the midpoint).
const POSTURE_MISS_M: f32 = 0.20;
const POSTURE_LINE_DEG: f32 = 30.0;
const POSTURE_SPINE_DEG: f32 = 35.0;

fn posture_ok(miss_r: f32, miss_l: f32, line_err: f32, spine_err: f32) -> bool {
    miss_r <= POSTURE_MISS_M && miss_l <= POSTURE_MISS_M && line_err <= POSTURE_LINE_DEG && spine_err.is_finite() && spine_err <= POSTURE_SPINE_DEG
}

#[repr(C, align(16))]
struct V4([f32; 4]);

#[link(name = "kernel32")]
extern "system" {
    fn VirtualQuery(addr: *const c_void, info: *mut MemInfo, len: usize) -> usize;
    fn GetTickCount() -> u32;
}
#[link(name = "user32")]
extern "system" {
    fn GetAsyncKeyState(vkey: i32) -> i16;
}
#[repr(C)]
struct MemInfo {
    base: usize,
    alloc_base: usize,
    alloc_protect: u32,
    region_size: usize,
    state: u32,
    protect: u32,
    kind: u32,
}

/// VirtualQuery answers (region base, end, state, protect) for the current frame. A 32-bit game runs VirtualQuery
/// through WOW64 (a kernel call per query); the carry and ragdoll hooks read thousands of fields per frame, so one
/// query per region per frame replaces one per read. The cache is emptied whenever FRAME changes.
struct RegionCache {
    frame: u32,
    regions: Vec<(usize, usize, u32, u32)>,
}
static REGIONS: Mutex<RegionCache> = Mutex::new(RegionCache { frame: u32::MAX, regions: Vec::new() });
const REGION_CACHE_MAX: usize = 256;

fn region(p: u32) -> Option<(usize, usize, u32, u32)> {
    let frame = FRAME.load(Ordering::Relaxed);
    let mut c = REGIONS.lock().unwrap_or_else(|e| e.into_inner());
    if c.frame != frame {
        c.frame = frame;
        c.regions.clear();
    }
    let a = p as usize;
    if let Some(r) = c.regions.iter().find(|r| r.0 <= a && a < r.1) {
        return Some(*r);
    }
    let mut mi: MemInfo = unsafe { core::mem::zeroed() };
    if unsafe { VirtualQuery(p as *const c_void, &mut mi, core::mem::size_of::<MemInfo>()) } == 0 {
        return None;
    }
    let r = (mi.base, mi.base + mi.region_size, mi.state, mi.protect);
    if c.regions.len() >= REGION_CACHE_MAX {
        c.regions.clear();
    }
    c.regions.push(r);
    Some(r)
}

/// True if `[p, p+len)` is committed, readable memory (no guard / no-access page).
fn readable(p: u32, len: usize) -> bool {
    if p < 0x10000 {
        return false;
    }
    let Some((_, end, state, protect)) = region(p) else { return false };
    const MEM_COMMIT: u32 = 0x1000;
    const READ_MASK: u32 = 0x02 | 0x04 | 0x08 | 0x20 | 0x40 | 0x80;
    state == MEM_COMMIT && protect & READ_MASK != 0 && protect & 0x100 == 0 && (p as usize + len) <= end
}

unsafe fn r32(p: u32) -> Option<u32> {
    readable(p, 4).then(|| core::ptr::read_unaligned(p as *const u32))
}

unsafe fn rf32(p: u32) -> Option<f32> {
    r32(p).map(f32::from_bits).filter(|v| v.is_finite())
}

unsafe fn rvec(p: u32) -> Option<[f32; 3]> {
    if !readable(p, 12) {
        return None;
    }
    let v = core::ptr::read_unaligned(p as *const [f32; 3]);
    v.iter().all(|x| x.is_finite() && x.abs() < 1.0e5).then_some(v)
}

fn in_text(f: u32) -> bool {
    (0x0040_1000..0x0100_0000).contains(&f)
}

/// Root position of a human through its motion component (`human+0x950`, vfunc `+0x28`).
unsafe fn human_pos(h: u32) -> Option<[f32; 3]> {
    let comp = h.wrapping_add(0x950);
    let f = r32(r32(comp)?.wrapping_add(0x28))?;
    if !in_text(f) {
        return None;
    }
    let get: unsafe extern "thiscall" fn(u32) -> u32 = core::mem::transmute(f as usize);
    rvec(get(comp))
}

/// Heading of a human in radians (`[human+0x10cc]+0x494`).
unsafe fn heading(h: u32) -> Option<f32> {
    rf32(r32(h + 0x10cc)? + 0x494).filter(|y| y.abs() < 100.0)
}



/// Pelvis rigid body of a ragdoll object.
unsafe fn pelvis(rd: u32) -> Option<u32> {
    let ri = r32(rd + 0xac)?;
    let bodies = r32(ri + 8)?;
    let count = r32(ri + 0xc)?;
    let idx = r32(r32(ri + 0x20)?)?;
    if count == 0 || count > 64 || idx >= count {
        return None;
    }
    let body = r32(bodies + idx * 4)?;
    readable(body, 0x130).then_some(body)
}

unsafe fn cstr(p: u32) -> Option<String> {
    if !readable(p, 48) {
        return None;
    }
    let bytes = core::slice::from_raw_parts(p as *const u8, 48);
    let end = bytes.iter().position(|&c| c == 0)?;
    let s = &bytes[..end];
    (end >= 3 && s.iter().all(|c| c.is_ascii_graphic() || *c == b' ')).then(|| String::from_utf8_lossy(s).into_owned())
}

/// Bone names of the ragdoll skeleton: hkaRagdollInstance `+0x2c` -> hkaSkeleton, whose bone array is found by
/// scanning its first fields for an hkArray of pointers to hkaBone {char* name, ...}.
unsafe fn ragdoll_bone_names(rd: u32) -> Vec<String> {
    match r32(rd + 0xac).and_then(|ri| r32(ri + 0x2c)) {
        Some(skel) => skeleton_bone_names(skel),
        None => Vec::new(),
    }
}

/// Bone names of an hkaSkeleton (the hkArray of hkaBone* found by scanning its first fields, as above).
unsafe fn skeleton_bone_names(skel: u32) -> Vec<String> {
    for off in (0x08..0x40).step_by(4) {
        let (Some(arr), Some(n)) = (r32(skel + off), r32(skel + off + 4)) else { continue };
        if !(2..=64).contains(&n) || !readable(arr, 4 * n as usize) {
            continue;
        }
        let names: Vec<String> = (0..n).filter_map(|i| r32(arr + 4 * i).and_then(|b| r32(b)).and_then(|p| cstr(p))).collect();
        if names.len() == n as usize {
            return names;
        }
    }
    Vec::new()
}

/// Ragdoll-skeleton index of the bone to hold: RB_TORSO (the NPC ragdolls' upper-body bone), else a chest/spine name.
unsafe fn find_grip_bone(rd: u32) -> (Option<u32>, Vec<String>) {
    let names = ragdoll_bone_names(rd);
    let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    let pick = lower.iter().position(|n| n.contains("torso"))
        .or_else(|| lower.iter().position(|n| n.contains("chest")))
        .or_else(|| lower.iter().rposition(|n| n.contains("spine")));
    (pick.map(|i| i as u32), names)
}

/// Rigid body for a ragdoll-skeleton bone through the bone -> body map (`[ri+0x20]`, i32 entries, -1 = none).
unsafe fn bone_body(rd: u32, bone: u32) -> Option<u32> {
    let ri = r32(rd + 0xac)?;
    let map = r32(ri + 0x20)?;
    if bone >= r32(ri + 0x24)? {
        return None;
    }
    let idx = r32(map + 4 * bone)? as i32;
    let count = r32(ri + 0xc)? as i32;
    if idx < 0 || idx >= count {
        return None;
    }
    let body = r32(r32(ri + 8)? + 4 * idx as u32)?;
    readable(body, 0x130).then_some(body)
}

unsafe fn r8(p: u32) -> Option<u8> {
    readable(p, 1).then(|| core::ptr::read(p as *const u8))
}

/// The remembered entity is still a human of the same class in a ragdoll / dying state: its memory readable through
/// the state fields, the same vtable as at registration (a freed or reused address fails this or the state test).
/// Returns its state id. Nothing else of the entity is touched before this passes.
unsafe fn human_ok(b: &Body) -> Result<u32, String> {
    if !readable(b.owner, 0x1240) {
        return Err("entity memory gone".into());
    }
    if r32(b.owner) != Some(b.vt) {
        return Err("entity replaced (vtable changed)".into());
    }
    match r32(b.owner + 0x123c) {
        Some(s) if s == STATE_RAGDOLL || s == STATE_DYING => Ok(s),
        Some(s) => Err(format!("entity no longer a corpse (state {s})")),
        None => Err("state unreadable".into()),
    }
}

/// For a corpse frozen by the ragdoll pool: its WSHumanStateRagdoll object, when the human is in state 3 with that
/// state object, the acquire flag `[state+0xbd]` set and no ragdoll of its own (link cleared or object owned by
/// another human). The caller has checked `human_ok`.
unsafe fn frozen_state(b: &Body) -> Option<u32> {
    if r32(b.owner + 0x123c) != Some(STATE_RAGDOLL) {
        return None;
    }
    let st = r32(b.owner + 0x1238).filter(|&s| readable(s, 0x120))?;
    if r32(st) != Some(STATE_RAGDOLL_VTABLE) || r8(st + 0xbd) != Some(1) {
        return None;
    }
    let rd = r32(b.owner + 0x10d0)?;
    (rd == 0 || r32(rd + 0x348) != Some(b.owner)).then_some(st)
}

/// Re-checks a remembered body: entity identity (`human_ok`), owner <-> ragdoll back-pointers and a ragdoll/dying
/// state. The pelvis body and its motion vtable are re-resolved, because the pooled ragdoll object can change.
/// Returns the refreshed body with its current pelvis position, or the reason it no longer qualifies.
unsafe fn still_valid(b: &Body) -> Result<Body, String> {
    still_valid_q(b, false)
}

/// `still_valid` without its log line (for the per-tick hover query, which must not write to the log).
unsafe fn still_valid_q(b: &Body, quiet: bool) -> Result<Body, String> {
    human_ok(b)?;
    let mut rd = b.rd;
    if r32(rd + 0x348) != Some(b.owner) || r32(b.owner + 0x10d0) != Some(rd) {
        // Ragdoll objects are pooled (STATE_RAGDOLL_VTABLE): a corpse can hold a different object than at registration.
        // Follow the owner's current ragdoll, but only with the mutual link (owner -> ragdoll -> owner).
        match r32(b.owner + 0x10d0) {
            Some(nrd) if nrd != 0 && nrd != rd && r32(nrd + 0x348) == Some(b.owner) => {
                if !quiet {
                    log!("[bodydrag] 0x{:08x}: ragdoll object replaced 0x{rd:08x} -> 0x{nrd:08x}; following it", b.owner);
                }
                rd = nrd;
            }
            _ => return Err("owner/ragdoll link broken".into()),
        }
    }
    let b = &Body { rd, ..*b };
    let state = r32(b.owner + 0x123c).unwrap_or(u32::MAX);
    if state != STATE_RAGDOLL && state != STATE_DYING {
        return Err(format!("state {state}"));
    }
    let body = pelvis(b.rd).ok_or("no pelvis body")?;
    r32(body + 0xe0).filter(|&v| v > 0x0040_0000).ok_or("no motion vtable")?;
    let pos = rvec(body + 0x120).ok_or("pelvis position unreadable")?;
    Ok(Body { body, pos, ..*b })
}

fn dist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn hdist(a: [f32; 3], b: [f32; 3]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

unsafe fn player() -> Option<u32> {
    r32(PLAYER_PTR).filter(|&p| p != 0)
}

fn key(vk: i32) -> bool {
    unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 }
}

/// Movement keys held right now, for the log (W A S D).
fn keys() -> String {
    [(0x57, 'W'), (0x41, 'A'), (0x53, 'S'), (0x44, 'D')].iter().map(|&(vk, c)| if key(vk) { c } else { '-' }).collect()
}

/// Horizontal look direction of the game camera as a heading (radians, same convention as the human heading: forward =
/// (sin, 0, cos)). Taken from the camera position relative to Sean ([0x1321b74] active WSGameCamera, position at
/// +0xa0 = matrix +0x70 row 3), because the third-person camera orbits behind him; this avoids any axis convention.
unsafe fn camera_look(pp: [f32; 3]) -> Option<f32> {
    if r32(0x0132_1b78).map_or(true, |n| n < 1) {
        return None;
    }
    let cam = r32(0x0132_1b74)?;
    let c = rvec(cam + 0xa0)?;
    let (dx, dz) = (pp[0] - c[0], pp[2] - c[2]);
    ((dx * dx + dz * dz).sqrt() > 0.3).then(|| dx.atan2(dz))
}

fn wrap_pi(a: f32) -> f32 {
    let pi = core::f32::consts::PI;
    (a + pi).rem_euclid(2.0 * pi) - pi
}

/// Travel direction (heading) for the movement keys relative to the camera look, Velvet Assassin's drag scheme
/// (Player_Animation_Controller::HandleThirdPersonAnimations: TargetHeading = CamHeading + key direction): W along the
/// look, S toward the camera, A to its left (+90 deg: heading + 90 deg = -right), D to its right; diagonals between.
/// None when no key or the keys cancel out.
fn travel_heading(look: f32, w: bool, a: bool, s: bool, d: bool) -> Option<f32> {
    let side = d as i32 as f32 - a as i32 as f32; // +1 = right
    let fwd = w as i32 as f32 - s as i32 as f32;
    (side != 0.0 || fwd != 0.0).then(|| wrap_pi(look + (-side).atan2(fwd)))
}

/// Clip that turns Sean (heading `h`) toward `want` while dragging, by the size of the error: pivot in place beyond
/// PIVOT, effort arc beyond EFFORT, normal arc beyond STEER, else the straight backward walk. Each boundary uses its
/// exit value while `cur` is at or above that tier (hysteresis), so the clip does not flicker at a boundary.
fn steer_clip(want: f32, h: f32, cur: Phase) -> Phase {
    let err = wrap_pi(want - h); // > 0: turn toward +yaw (the _l clips)
    let tier = |p: Phase| match p {
        Phase::TurnL | Phase::TurnR => 3,
        Phase::HardL | Phase::HardR => 2,
        Phase::WalkL | Phase::WalkR => 1,
        _ => 0,
    };
    let t = tier(cur);
    let pivot = if t >= 3 { PIVOT_EXIT } else { PIVOT_ENTER };
    let effort = if t >= 2 { EFFORT_EXIT } else { EFFORT_ENTER };
    let arc = if t >= 1 { STEER_EXIT } else { STEER_ENTER };
    let left = err > 0.0;
    match err.abs() {
        e if e > pivot.to_radians() => if left { Phase::TurnL } else { Phase::TurnR },
        e if e > effort.to_radians() => if left { Phase::HardL } else { Phase::HardR },
        e if e > arc.to_radians() => if left { Phase::WalkL } else { Phase::WalkR },
        _ => Phase::Walk,
    }
}

/// Hold clip for the keys (game focused), Velvet Assassin's drag steering: any of W A S D picks a travel direction
/// relative to the camera (`travel_heading`) and Sean, always in the one backward drag walk (Velvet's DRAGGING_BODY
/// has a single Forward clip), turns to face away from it so he backs into it with the body trailing.
/// These are the game's own movement keys; the game ignores them while Sean plays a drag sequence (Animate state).
unsafe fn hold_mode(p: u32, pp: Option<[f32; 3]>, cur: Phase) -> Phase {
    if !crate::game_has_focus() {
        return Phase::Idle;
    }
    let (w, a, s, d) = (key(0x57), key(0x41), key(0x53), key(0x44));
    if !(w || a || s || d) {
        return Phase::Idle;
    }
    let (Some(pp), Some(h)) = (pp, heading(p)) else { return Phase::Walk };
    let Some(look) = camera_look(pp) else { return Phase::Walk };
    match travel_heading(look, w, a, s, d) {
        Some(t) => steer_clip(t +core::f32::consts::PI, h, cur),
        None => Phase::Idle,
    }
}

unsafe fn fmt_heading(p: u32) -> String {
    heading(p).map(|y| format!("{:.1} deg", y.to_degrees())).unwrap_or_else(|| "?".into())
}

// ───────────────────────── commands ─────────────────────────

fn queue(gen: u32, label: &str, call: &str) {
    log!("[bodydrag] clock={:.0} gen {gen}: queued {label}", clock());
    COMMANDS.lock().unwrap_or_else(|e| e.into_inner()).push((
        gen,
        label.to_owned(),
        format!("local h = Util.GetHandleByName(\"Saboteur\") {call}"),
    ));
    HAS_COMMANDS.store(true, Ordering::SeqCst);
}

pub fn has_commands() -> bool {
    HAS_COMMANDS.load(Ordering::Relaxed)
}

/// Run queued bodydrag commands from the lua_pcall hook; commands of an ended interaction are dropped.
pub unsafe fn drain_commands(l: *mut c_void) {
    let jobs: Vec<_> = core::mem::take(&mut *COMMANDS.lock().unwrap_or_else(|e| e.into_inner()));
    HAS_COMMANDS.store(false, Ordering::SeqCst);
    let current = GEN.load(Ordering::SeqCst);
    for (gen, label, code) in jobs {
        if gen != current {
            log!("[bodydrag] clock={:.0} gen {gen}: dropped stale {label} (current gen {current})", clock());
            continue;
        }
        log!("[bodydrag] clock={:.0} gen {gen}: executing {label}", clock());
        crate::run_chunk(l, "bodydrag", &code);
    }
}

// ───────────────────────── grab ─────────────────────────

/// Outcome of target selection for one F7.
enum Pick {
    Grab(Body, Grip),
    Restoring,
    Refused,
}

#[derive(Clone)]
enum Kind {
    Live,          // own ragdoll, pelvis body readable
    Frozen,        // static pose after the pool took its ragdoll
    NoRagdoll(String),
}

/// A held-able ragdoll at the grab: pelvis not FIXED (motion type 7 is never converted) and torso within reach.
/// Err((label, detail)).
/// The physics / reach part of the grab check, without side effects (shared by F7 and the HUD prompt):
/// (grip bone index, bone names, horizontal torso distance, motion type).
unsafe fn live_reach(b: &Body, pp: [f32; 3]) -> Result<(Option<u32>, Vec<String>, f32, Option<u32>), (&'static str, String)> {
    let motion = r32(b.body + 0xe8).map(|t| t & 0xff);
    if motion == Some(7) {
        return Err(("PHYSICS_FIXED", "pelvis rigid body motion type 7 (FIXED); not converted".into()));
    }
    let (grip_bone, names) = find_grip_bone(b.rd);
    let torso = grip_bone.and_then(|i| bone_body(b.rd, i)).and_then(|gb| rvec(gb + 0x120)).unwrap_or(b.pos);
    let (h, v) = (hdist(torso, pp), (torso[1] - pp[1]).abs());
    if h > REACH_H || v > REACH_V {
        return Err(("OUT_OF_RANGE", format!("torso {h:.2} m across, {v:.2} m up; limit {REACH_H}/{REACH_V}")));
    }
    Ok((grip_bone, names, h, motion))
}

unsafe fn grip_check(b: &Body, pp: [f32; 3]) -> Result<Grip, (&'static str, String)> {
    let (grip_bone, names, h, motion) = live_reach(b, pp)?;
    let count = r32(b.rd + 0xac).and_then(|ri| r32(ri + 0xc)).unwrap_or(0);
    log!(
        "[bodydrag] 0x{:08x}: ragdoll 0x{:08x}, {count} rigid bodies, pelvis motion type {}, torso {h:.2} m away, grip bone {}",
        b.owner, b.rd, motion.map_or("?".into(), |m| m.to_string()),
        grip_bone.map(|i| format!("#{i} {}", names[i as usize])).unwrap_or_else(|| "none (pelvis on the rope)".into())
    );
    let g = grip_bones(&names);
    log!(
        "[bodydrag] 0x{:08x}: ragdoll type {}: {}",
        b.owner, r32(b.rd + 0x36c).map_or("?".into(), |t| t.to_string()), grip_txt(&g, &names)
    );
    Ok(g)
}

/// Target selection for one F7 over the registry, nearest first; every request ends in one logged RESULT.
/// Entries whose entity fails `human_ok` are dropped here (freed / reused / revived), never by age.
/// `only`: the corpse a restore just woke; nothing else is considered and no further restore is started.
unsafe fn find_body(p: u32, only: Option<u32>) -> Pick {
    let _t = perf::Timer::new("F7 target search", perf::Kind::Note);
    let Some(pp) = human_pos(p) else {
        log!("[bodydrag] clock={:.0} F7 RESULT: NO_CANDIDATE (player position unreadable)", clock());
        return Pick::Refused;
    };
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let mut pruned = Vec::new();
    known.retain(|b| match human_ok(b) {
        Ok(_) => true,
        Err(why) => {
            pruned.push(format!("0x{:08x}: {why}", b.owner));
            false
        }
    });
    let mut cands: Vec<(Body, Kind, f32)> = Vec::new();
    for b in known.iter_mut() {
        match still_valid(b) {
            Ok(fresh) => {
                *b = fresh;
                cands.push((fresh, Kind::Live, dist(fresh.pos, pp)));
            }
            Err(why) => {
                // human_ok passed: the entity is a corpse; its root position stands in for the missing ragdoll
                let at = human_pos(b.owner);
                let kind = frozen_state(b).map_or(Kind::NoRagdoll(why), |_| Kind::Frozen);
                cands.push((Body { pos: at.unwrap_or(b.pos), ..*b }, kind, at.map_or(f32::INFINITY, |a| dist(a, pp))));
            }
        }
    }
    drop(known);
    cands.sort_by(|a, b| a.2.total_cmp(&b.2));
    // the body the HUD prompt names goes first, so F7 picks what the prompt showed (stable sort keeps the rest)
    if only.is_none() {
        if let Some(sel) = hover_selected() {
            cands.sort_by_key(|c| c.0.owner != sel);
        }
    }
    let count = |f: fn(&Kind) -> bool| cands.iter().filter(|c| f(&c.1)).count();
    let kind_txt = |k: &Kind| match k {
        Kind::Live => "live ragdoll".to_string(),
        Kind::Frozen => "frozen (ragdoll returned to the pool)".to_string(),
        Kind::NoRagdoll(why) => format!("no ragdoll ({why})"),
    };
    log!(
        "[bodydrag] clock={:.0} F7 REQUEST: player ({:.2},{:.2},{:.2}) heading {}; known {} (live {}, frozen {}, other {}), pruned {}{}; nearest {}",
        clock(), pp[0], pp[1], pp[2], heading(p).map(|y| format!("{:.1} deg", y.to_degrees())).unwrap_or_else(|| "?".into()),
        cands.len(), count(|k| matches!(k, Kind::Live)), count(|k| matches!(k, Kind::Frozen)),
        count(|k| matches!(k, Kind::NoRagdoll(_))), pruned.len(),
        if pruned.is_empty() { String::new() } else { format!(" [{}]", pruned.join(", ")) },
        cands.first().map_or("none".into(), |c| format!("0x{:08x} {:.2} m, {}", c.0.owner, c.2, kind_txt(&c.1)))
    );
    let mut last = ("NO_CANDIDATE", format!("nothing within {FIND_RADIUS} m"));
    for (b, kind, d) in cands.into_iter().take_while(|c| c.2 <= FIND_RADIUS).filter(|c| only.map_or(true, |o| c.0.owner == o)) {
        match kind {
            Kind::Live => match grip_check(&b, pp) {
                Ok(grip) => {
                    log!("[bodydrag] clock={:.0} F7 RESULT: GRAB_SUCCESS 0x{:08x} ({d:.2} m)", clock(), b.owner);
                    return Pick::Grab(b, grip);
                }
                Err((label, why)) => {
                    log!("[bodydrag]   0x{:08x} {d:.2} m: {label} ({why})", b.owner);
                    last = (label, format!("0x{:08x}: {why}", b.owner));
                }
            },
            Kind::Frozen => {
                let started = match (frozen_state(&b), only) {
                    (_, Some(_)) => Err("still frozen after its restore".into()),
                    (Some(_), None) => lifecycle::start(b.owner, b.vt, true),
                    (None, None) => Err("ragdoll state changed".into()),
                };
                match started {
                    Ok(txt) => {
                        log!("[bodydrag] clock={:.0} F7 PENDING: RESTORING 0x{:08x} ({d:.2} m): ragdoll recycled by the pool; {txt}", clock(), b.owner);
                        return Pick::Restoring;
                    }
                    Err(why) => {
                        log!("[bodydrag]   0x{:08x} {d:.2} m: RESTORE_REFUSED ({why})", b.owner);
                        last = ("RESTORE_REFUSED", format!("0x{:08x}: {why}", b.owner));
                    }
                }
            }
            Kind::NoRagdoll(why) => {
                log!("[bodydrag]   0x{:08x} {d:.2} m: NO_RAGDOLL ({why})", b.owner);
                last = ("NO_RAGDOLL", format!("0x{:08x}: {why}", b.owner));
            }
        }
    }
    log!("[bodydrag] clock={:.0} F7 RESULT: {} ({})", clock(), last.0, last.1);
    Pick::Refused
}

// ───────────────────────── pull ─────────────────────────

/// Velocity toward `target` for the body at `pos`, gain-limited and capped upward.
fn steer(pos: [f32; 3], target: [f32; 3], up_cap: f32) -> [f32; 3] {
    let mut v = [(target[0] - pos[0]) * GAIN, (target[1] - pos[1]) * GAIN, (target[2] - pos[2]) * GAIN];
    let s = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if s > MAX_SPEED {
        v.iter_mut().for_each(|c| *c *= MAX_SPEED / s);
    }
    v[1] = v[1].min(up_cap);
    v
}

/// Push every rigid body of the ragdoll that overlaps Sean's column (radius SEP_R, height 0..SEP_H above his feet)
/// outward, by adding to its current linear velocity (motion +0xd0). Returns how many bodies were pushed.
unsafe fn separate(rd: u32, pp: [f32; 3]) -> u32 {
    let (Some(ri), mut pushed) = (r32(rd + 0xac), 0) else { return 0 };
    let (Some(bodies), Some(count)) = (r32(ri + 8), r32(ri + 0xc)) else { return 0 };
    for i in 0..count.min(32) {
        let Some(body) = r32(bodies + 4 * i).filter(|&b| readable(b, 0x130)) else { continue };
        let Some(p) = rvec(body + 0x120) else { continue };
        let (dx, dz) = (p[0] - pp[0], p[2] - pp[2]);
        let r = (dx * dx + dz * dz).sqrt();
        if r >= SEP_R || p[1] < pp[1] - 0.1 || p[1] > pp[1] + SEP_H {
            continue;
        }
        let (nx, nz) = if r > 0.01 { (dx / r, dz / r) } else { (1.0, 0.0) };
        let push = (SEP_R - r) * SEP_GAIN;
        let v = rvec(body + 0xe0 + 0xd0).unwrap_or([0.0; 3]);
        if set_velocity(body, [v[0] + nx * push, v[1], v[2] + nz * push]) {
            pushed += 1;
        }
    }
    pushed
}

unsafe fn set_velocity(body: u32, v: [f32; 3]) -> bool {
    let Some(setv) = r32(body + 0xe0).and_then(|m| r32(m + 0x40)).filter(|&f| in_text(f)) else { return false };
    if r32(body + 0xe8).map(|t| t & 0xff) == Some(7) {
        return false;
    }
    let activate: unsafe extern "thiscall" fn(u32) = core::mem::transmute(ACTIVATE);
    let set_lin_vel: unsafe extern "thiscall" fn(u32, *const V4) = core::mem::transmute(setv as usize);
    activate(body);
    set_lin_vel(body + 0xe0, &V4([v[0], v[1], v[2], 0.0]));
    true
}

/// Ragdoll-skeleton bones used by the underarm grip, found by name at the grab (NPC ragdolls: RB_TORSO, RB_HEAD,
/// RB_LEFT_SHOULDER, RB_RIGHT_SHOULDER; LEFT/RIGHT are the corpse's own sides). The low-detail pool type has no
/// shoulder bodies (sab_devlog session at line 33988: all 15 type-1 grabs fell back to the single torso pull); its
/// upper-arm bodies, whose origin is the shoulder joint, stand in for them (`arms`).
#[derive(Clone, Copy, Default)]
struct Grip {
    torso: Option<u32>,
    head: Option<u32>,
    lsh: Option<u32>,
    rsh: Option<u32>,
    arms: bool,
}

fn grip_bones(names: &[String]) -> Grip {
    let find = |key: &str| names.iter().position(|n| n.to_ascii_lowercase().contains(key)).map(|i| i as u32);
    let (lsh, rsh) = (find("left_shoulder"), find("right_shoulder"));
    let arms = lsh.is_none() || rsh.is_none();
    Grip {
        torso: find("torso"),
        head: find("head"),
        lsh: lsh.or_else(|| find("left_upper_arm")),
        rsh: rsh.or_else(|| find("right_upper_arm")),
        arms,
    }
}

/// Which bones carry the body, for the grab log.
fn grip_txt(g: &Grip, names: &[String]) -> String {
    let n = |i: Option<u32>| i.and_then(|i| names.get(i as usize)).map_or("-".to_string(), |s| s.clone());
    match (g.lsh, g.rsh) {
        (Some(_), Some(_)) => format!(
            "underarm grip on {} / {}{}",
            n(g.lsh), n(g.rsh), if g.arms { " (no shoulder bodies in this ragdoll: upper arms used)" } else { "" }
        ),
        _ => format!("TORSO PULL only: no left/right shoulder or upper-arm bodies in [{}]", names.join(" ")),
    }
}

/// The body's current linear velocity (motion +0xd0, where setLinearVelocity 0xaad860 stores it), zero if unreadable.
unsafe fn lin_velocity(body: u32) -> [f32; 3] {
    rvec(body + 0xe0 + 0xd0).unwrap_or([0.0; 3])
}

/// The body's current angular velocity (motion +0xe0, where setAngularVelocity 0xaad890 stores it), zero if unreadable.
unsafe fn ang_velocity(body: u32) -> [f32; 3] {
    rvec(body + 0xe0 + 0xe0).unwrap_or([0.0; 3])
}

unsafe fn set_spin(body: u32, w: [f32; 3]) -> bool {
    let Some(f) = r32(body + 0xe0).and_then(|m| r32(m + 0x44)).filter(|&f| in_text(f)) else { return false };
    if r32(body + 0xe8).map(|t| t & 0xff) == Some(7) {
        return false;
    }
    let set_ang_vel: unsafe extern "thiscall" fn(u32, *const V4) = core::mem::transmute(f as usize);
    set_ang_vel(body + 0xe0, &V4([w[0], w[1], w[2], 0.0]));
    true
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let n = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt().max(1e-6);
    [a[0] / n, a[1] / n, a[2] / n]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn angle_deg(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (a, b) = (norm(a), norm(b));
    (a[0] * b[0] + a[1] * b[1] + a[2] * b[2]).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Grip pose at `t` clip seconds into qol_drag_out (None = holding): shoulder target delta (forward, up) from the held
/// pose, spine elevation from Sean's forward axis (radians; the held SPINE_PITCH back toward Sean is 180 - SPINE_PITCH
/// deg, shifted by Velvet's change since frame 0 so frame 0 continues the hold), and the support factor (1 = full).
fn release_pose(t: Option<f32>) -> (f32, f32, f32, f32) {
    let hold = core::f32::consts::PI - SPINE_PITCH.to_radians();
    let Some(t) = t else { return (0.0, 0.0, hold, 1.0) };
    let x = (t.max(0.0) * RELEASE_FPS).min((RELEASE_CURVE.len() - 1) as f32);
    let (i, a) = (x as usize, x.fract());
    let (r0, r1) = (RELEASE_CURVE[i], RELEASE_CURVE[(i + 1).min(RELEASE_CURVE.len() - 1)]);
    let lerp = |k: usize| r0[k] + (r1[k] - r0[k]) * a;
    let elev = |r: [f32; 4]| r[3].atan2(r[2]);
    let (e0, e1) = (elev(r0), elev(r1));
    let spine = hold + (e0 + (e1 - e0) * a) - elev(RELEASE_CURVE[0]);
    let support = (1.0 - (t - RELEASE_LET_S) / (RELEASE_S - RELEASE_LET_S)).clamp(0.0, 1.0);
    (lerp(0), lerp(1), spine, support)
}

/// Underarm grip, one frame (velocity attachment with the verified Havok setters only; no new constraints):
/// - each shoulder body is steered to a point under Sean's matching hand, in Sean's frame (heading): the corpse's right
///   shoulder to Sean's right hand, so the corpse faces away from Sean with its back to him (fixes the roll);
///   v = Sean's velocity + SH_GAIN * error + a gravity feed-forward, ramped in over the grab;
/// - the torso gets an angular velocity that turns its spine toward Sean's chest (SPINE_PITCH above horizontal) and its
///   shoulder line square to Sean;
/// - the head is not pulled (it follows through the ragdoll's neck joint) except a small lift when it enters the zone in
///   front of Sean's pelvis; hips and legs stay free.
/// During qol_drag_out (`release` = clip seconds) the targets and spine follow `release_pose` and every term is scaled
/// by its support factor, so the weight passes to gravity and the ground instead of being cut at once.
/// Returns the diagnostic text, or None when the shoulder bodies are unavailable (caller falls back to the torso pull).
/// Share of the per-shoulder lateral offset in the hand targets from the cosine of the shoulder-line error: 0 while
/// the corpse's shoulder line is 90 deg or more off Sean's right (both shoulders go to the midpoint, so a reversed
/// corpse is not dragged through itself), 1 within 30 deg.
fn lateral_weight(cos_err: f32) -> f32 {
    (cos_err / LATERAL_FULL_COS).clamp(0.0, 1.0)
}
const LATERAL_FULL_COS: f32 = 0.866; // cos 30 deg

/// Roll correction vector for the shoulder line. Within 90 deg: cross(line, right) (sine-scaled, as before). Beyond
/// 90 deg, where that cross product shrinks to zero at 180 deg: a unit roll about the corpse's own spine in the
/// shorter direction (positive when exactly reversed).
fn roll_term(axis_now: [f32; 3], right: [f32; 3], spine_now: [f32; 3]) -> [f32; 3] {
    let c = cross(axis_now, right);
    let cos_err = axis_now[0] * right[0] + axis_now[1] * right[1] + axis_now[2] * right[2];
    if cos_err >= 0.0 {
        return c;
    }
    let along = c[0] * spine_now[0] + c[1] * spine_now[1] + c[2] * spine_now[2];
    let s = if along < 0.0 { -1.0 } else { 1.0 };
    [spine_now[0] * s, spine_now[1] * s, spine_now[2] * s]
}

/// The underarm grip. Err(reason) when it cannot run this frame; the caller then uses the torso pull and logs why.
unsafe fn underarm(it: &mut Interaction, b: &Body, g: &Grip, p: u32, pp: [f32; 3], now: f64, release: Option<f32>) -> Result<String, String> {
    let (Some(li), Some(ri)) = (g.lsh, g.rsh) else { return Err("no shoulder or upper-arm bones in this ragdoll".into()) };
    let (Some(ls), Some(rs)) = (bone_body(b.rd, li), bone_body(b.rd, ri)) else { return Err("shoulder bodies not mapped".into()) };
    let (Some(lp), Some(rp)) = (rvec(ls + 0x120), rvec(rs + 0x120)) else { return Err("shoulder positions unreadable".into()) };
    let Some(yaw) = heading(p) else { return Err("Sean's heading unreadable".into()) };
    let f = [yaw.sin(), 0.0, yaw.cos()];
    let right = [-yaw.cos(), 0.0, yaw.sin()]; // Sean's right (his left is +X at yaw 0)
    let (d_fwd, d_up, spine_elev, support) = release_pose(release);
    let ramp = ((now - it.grab_clock) / GRIP_RAMP_MS).clamp(0.15, 1.0) as f32;
    let h = HAND_LOW + (HAND_HIGH - HAND_LOW) * ramp + d_up;
    let fwd = HAND_FWD + d_fwd;
    let axis_now = norm(sub(rp, lp));
    let lat = lateral_weight(axis_now[0] * right[0] + axis_now[1] * right[1] + axis_now[2] * right[2]);
    let at = |side: f32| [
        pp[0] + f[0] * fwd + right[0] * HAND_HALF_SPAN * side * lat,
        pp[1] + h,
        pp[2] + f[2] * fwd + right[2] * HAND_HALF_SPAN * side * lat,
    ];
    let (tr, tl) = (at(1.0), at(-1.0));
    let sv = trail_velocity(&it.trail);
    let dt = (it.frame_dt.max(1.0) / 1000.0) as f32;
    // integral of the vertical error (only once the grab ramp is complete, so the pickup cannot wind it up; frozen
    // during the release)
    if ramp >= 1.0 && release.is_none() {
        for (i, (pos, tgt)) in [(rp, tr), (lp, tl)].iter().enumerate() {
            it.sh_i[i] = (it.sh_i[i] + (tgt[1] - pos[1]) * SH_I_GAIN * dt).clamp(-SH_I_MAX, SH_I_MAX);
        }
    }
    // Velocities are written, not added, so a fading support blends the controller with the body's own simulated
    // velocity (support 0 = untouched by us, gravity and contacts only).
    let pull_to = |body: u32, pos: [f32; 3], tgt: [f32; 3], bias: f32| -> [f32; 3] {
        let mut v = [0.0f32; 3];
        for k in 0..3 {
            v[k] = sv[k] + (tgt[k] - pos[k]) * SH_GAIN * ramp;
        }
        v[1] += GRAVITY * dt * ramp + bias;
        let s = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        if s > SH_MAX_SPEED {
            v.iter_mut().for_each(|c| *c *= SH_MAX_SPEED / s);
        }
        let own = lin_velocity(body);
        [0, 1, 2].map(|k| own[k] + (v[k] - own[k]) * support)
    };
    let (vr, vl) = (pull_to(rs, rp, tr, it.sh_i[0]), pull_to(ls, lp, tl, it.sh_i[1]));
    match (set_velocity(rs, vr), set_velocity(ls, vl)) {
        (true, true) => {}
        (r, l) => return Err(format!("shoulder velocity not written (right {r}, left {l}: FIXED or no motion)")),
    }

    // torso orientation: spine (shoulder midpoint minus torso centre... toward the head) should point back toward Sean
    // and up; shoulder line (left -> right) should match Sean's right.
    let mid = [(lp[0] + rp[0]) / 2.0, (lp[1] + rp[1]) / 2.0, (lp[2] + rp[2]) / 2.0];
    let spine_want = norm([f[0] * spine_elev.cos(), spine_elev.sin(), f[2] * spine_elev.cos()]);
    let mut spine_err = f32::NAN;
    let mut spin_mag = 0.0f32;
    if let Some(tb) = g.torso.and_then(|i| bone_body(b.rd, i)) {
        if let Some(tp) = rvec(tb + 0x120) {
            let spine_now = norm(sub(mid, tp));
            spine_err = angle_deg(spine_now, spine_want);
            // pitch: same treatment as the roll - cross(spine_now, spine_want) vanishes when the spine points the
            // opposite way, so beyond 90 deg the corpse pitches about its own shoulder line
            let e1 = roll_term(spine_now, spine_want, axis_now);
            let e2 = roll_term(axis_now, right, spine_now);
            let mut w = [(e1[0] + e2[0]) * SPIN_GAIN * ramp, (e1[1] + e2[1]) * SPIN_GAIN * ramp, (e1[2] + e2[2]) * SPIN_GAIN * ramp];
            spin_mag = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt();
            if spin_mag > SPIN_MAX {
                w.iter_mut().for_each(|c| *c *= SPIN_MAX / spin_mag);
            }
            let own = ang_velocity(tb);
            set_spin(tb, [0, 1, 2].map(|k| own[k] + (w[k] - own[k]) * support));
        }
    }

    // head clearance: lift it out of the zone in front of Sean's pelvis (below CLEAR_H, closer than CLEAR_R)
    let mut head_txt = String::from("head ?");
    if let Some(hb) = g.head.and_then(|i| bone_body(b.rd, i)) {
        if let Some(hp) = rvec(hb + 0x120) {
            let rel = sub(hp, pp);
            let fwd = rel[0] * f[0] + rel[2] * f[2];
            let pelvis = [pp[0], pp[1] + SEAN_PELVIS_H, pp[2]];
            let clear = dist(hp, pelvis);
            let mut lifted = false;
            if support >= 1.0 && rel[1] < CLEAR_H && hdist(hp, pp) < CLEAR_R {
                let v = rvec(hb + 0xe0 + 0xd0).unwrap_or([0.0; 3]);
                lifted = set_velocity(hb, [v[0] + f[0] * 0.6, v[1].max(0.0) + (CLEAR_H - rel[1]) * 4.0, v[2] + f[2] * 0.6]);
            }
            if it.want_diag {
                head_txt = format!("head fwd {fwd:.2} up {:.2} to-pelvis {clear:.2}{}", rel[1], if lifted { " LIFT" } else { "" });
            }
        }
    }
    let axis_err = angle_deg(axis_now, right);
    let (miss_r, miss_l) = (dist(rp, tr), dist(lp, tl));
    let verdict = if ramp >= 1.0 && release.is_none() {
        let ok = posture_ok(miss_r, miss_l, axis_err, spine_err);
        it.posture.1 += 1;
        it.posture.0 += ok as u32;
        if ok { " | posture OK" } else { " | posture BAD" }
    } else {
        ""
    };
    if !it.want_diag {
        return Ok(String::new()); // the diagnostic line is written only every DIAG_MS: do not format it every frame
    }
    let pel = b.pos; // pelvis body (still_valid's position)
    let geo = format!(
        "shoulders up {:.2} fwd {:.2}, pelvis up {:.2} fwd {:.2}, shoulder-pelvis {:.2} m, I {:+.2}/{:+.2}",
        mid[1] - pp[1], (mid[0] - pp[0]) * f[0] + (mid[2] - pp[2]) * f[2],
        pel[1] - pp[1], (pel[0] - pp[0]) * f[0] + (pel[2] - pp[2]) * f[2], dist(mid, pel), it.sh_i[0], it.sh_i[1]
    );
    let rel_txt = release.map_or(String::new(), |t| {
        format!("RELEASE t {t:.3} s support {support:.2} target fwd {fwd:.2} up {h:.2} spine elev {:.0} deg | ", spine_elev.to_degrees())
    });
    Ok(format!(
        "{rel_txt}underarm{}: R-shoulder to target {miss_r:.2} L-shoulder to target {miss_l:.2} lateral {lat:.2} | {head_txt} | {geo} | spine err {spine_err:.0} deg, shoulder-line err {axis_err:.0} deg, spin cmd {spin_mag:.1} rad/s (before the {SPIN_MAX} cap), shoulder v cmd {:.1}/{:.1} m/s, ramp {ramp:.2}{verdict}",
        if g.arms { " (upper arms)" } else { "" },
        (vr[0] * vr[0] + vr[1] * vr[1] + vr[2] * vr[2]).sqrt(), (vl[0] * vl[0] + vl[1] * vl[1] + vl[2] * vl[2]).sqrt()
    ))
}

/// One pull step for the held body: the underarm grip when the shoulder bodies exist, else the torso pull (or the
/// pelvis rope). Returns Err(reason) if the hold must end.
unsafe fn pull(it: &mut Interaction, b: &Body, g: &Grip, p: u32, pp: [f32; 3], release: Option<f32>) -> Result<(String, Body), String> {
    let b = still_valid(b).map_err(|why| format!("body no longer valid ({why})"))?;
    let d = dist(b.pos, pp);
    if d > GIVE_UP {
        return Err(format!("body {d:.2} m away"));
    }
    match underarm(it, &b, g, p, pp, clock(), release) {
        Ok(txt) => {
            let pushed = separate(b.rd, pp);
            PULLED_FRAME.store(FRAME.load(Ordering::Relaxed), Ordering::Relaxed);
            if !it.want_diag {
                return Ok((String::new(), b));
            }
            return Ok((format!("{txt}{} body d {d:.2}", if pushed > 0 { format!(" (+{pushed} pushed out of Sean)") } else { String::new() }), b));
        }
        Err(why) => {
            if it.downgrade.is_none() {
                log!("[bodydrag] clock={:.0} gen {}: UNDERARM GRIP NOT USED ({why}); torso pull instead", clock(), it.gen);
                it.downgrade = Some(why);
            }
        }
    }
    let grip_bone = g.torso;
    let (dx, dz) = (b.pos[0] - pp[0], b.pos[2] - pp[2]);
    let n = (dx * dx + dz * dz).sqrt().max(0.01);
    let (ux, uz) = (dx / n, dz / n);
    let grip = grip_bone.and_then(|i| bone_body(b.rd, i)).and_then(|gb| rvec(gb + 0x120).map(|gp| (gb, gp)));
    let (what, (body, at), target, cap) = match grip {
        Some((gb, gp)) => {
            let ramp = ((clock() - it.grab_clock) / GRIP_RAMP_MS).clamp(0.0, 1.0) as f32;
            let y = pp[1] + GRIP_LOW + (GRIP_HIGH - GRIP_LOW) * ramp + release_pose(release).1;
            ("torso", (gb, gp), [pp[0] + ux * GRIP_REACH, y, pp[2] + uz * GRIP_REACH], 3.0)
        }
        None => ("pelvis", (b.body, b.pos), [pp[0] + ux * ROPE, pp[1] + 0.15, pp[2] + uz * ROPE], 1.0),
    };
    let (support, own, v) = (release_pose(release).3, lin_velocity(body), steer(at, target, cap));
    // Sean's own velocity fed forward (as the underarm grip does): with error feedback alone, following him at the
    // 1.67 m/s walk needs a standing lag of 1.67 / GAIN = 0.28 m (the v19 log: torso miss 0.35-0.38 m while walking)
    let sv = trail_velocity(&it.trail);
    let v = [v[0] + sv[0], v[1], v[2] + sv[2]];
    let v = [0, 1, 2].map(|k| own[k] + (v[k] - own[k]) * support);
    if !set_velocity(body, v) {
        return Err(format!("{what} body cannot be moved (FIXED or no motion)"));
    }
    let pushed = separate(b.rd, pp);
    PULLED_FRAME.store(FRAME.load(Ordering::Relaxed), Ordering::Relaxed);
    if !it.want_diag {
        return Ok((String::new(), b));
    }
    let what = if pushed > 0 { format!("{what} (+{pushed} pushed out of Sean)") } else { what.to_string() };
    Ok((
        format!(
            "{what} rel ({:.2},{:.2},{:.2}) miss {:.2} set ({:.2},{:.2},{:.2}) body d {d:.2}",
            at[0] - pp[0], at[1] - pp[1], at[2] - pp[2], dist(at, target), v[0], v[1], v[2]
        ),
        b,
    ))
}

// ───────────────────────── interaction ─────────────────────────

fn start_request(it: &mut Interaction, phase: Phase, before: AnimState) {
    queue(it.gen, phase.clip(), &format!("Actor.PlayAnimation(h, \"{}\")", phase.clip()));
    it.req = Request { phase, queued: clock(), before, active: None };
}

/// End the interaction without touching Sean's animation (used when ownership is lost).
fn abort(it: &Interaction, why: &str) {
    let g = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    log!("[bodydrag] clock={:.0} gen {}: ABORTED ({why}); no drop animation, no cancel; gen -> {g}", clock(), it.gen);
    log!("[bodydrag] gen {}: {}", it.gen, carry_summary(it));
}

/// End the interaction normally (drop animation finished).
fn finish(it: &Interaction, why: &str) {
    let g = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    log!("[bodydrag] clock={:.0} gen {}: finished ({why}); gen -> {g}", clock(), it.gen);
    log!("[bodydrag] gen {}: {}", it.gen, carry_summary(it));
}

/// One line per carry: which controller held the body and how often the settled posture passed.
fn carry_summary(it: &Interaction) -> String {
    let (ok, n) = it.posture;
    let ctrl = match &it.downgrade {
        Some(why) => format!("TORSO PULL ({why})"),
        None => "underarm grip".to_string(),
    };
    if n == 0 {
        format!("CARRY SUMMARY: {ctrl}; no settled underarm samples")
    } else {
        format!("CARRY SUMMARY: {ctrl}; posture OK in {ok}/{n} settled samples ({:.0} %)", 100.0 * ok as f32 / n as f32)
    }
}

/// Speed over the last ~100 ms of the position trail (m/s).
fn trail_speed(trail: &VecDeque<(f64, [f32; 3])>) -> Option<f32> {
    let &(t1, p1) = trail.back()?;
    let &(t0, p0) = trail.iter().rev().find(|(t, _)| t1 - *t >= 100.0)?;
    Some(hdist(p0, p1) / ((t1 - t0) as f32 / 1000.0))
}

/// Sean's velocity over the last ~100 ms of the position trail (m/s), zero if unknown.
fn trail_velocity(trail: &VecDeque<(f64, [f32; 3])>) -> [f32; 3] {
    let Some(&(t1, p1)) = trail.back() else { return [0.0; 3] };
    let Some(&(t0, p0)) = trail.iter().rev().find(|(t, _)| t1 - *t >= 100.0) else { return [0.0; 3] };
    let s = ((t1 - t0) as f32 / 1000.0).max(1e-3);
    [(p1[0] - p0[0]) / s, (p1[1] - p0[1]) / s, (p1[2] - p0[2]) / s]
}

/// One frame of the interaction. Returns false when it has ended.
unsafe fn step(it: &mut Interaction, toggle: bool) -> bool {
    let now = clock();
    // ownership: same player object, alive, not ragdolled
    let Some(p) = player() else {
        abort(it, "no player object");
        return false;
    };
    if p != it.player {
        abort(it, &format!("player object changed 0x{:08x} -> 0x{p:08x}", it.player));
        return false;
    }
    let st = anim_state(p);
    if st.0 == STATE_RAGDOLL || st.0 == STATE_DYING {
        abort(it, &format!("Sean entered state {}", st.0));
        return false;
    }
    let pp = human_pos(p);
    if let Some(&(t_prev, _)) = it.trail.back() {
        it.frame_dt = (now - t_prev).clamp(1.0, 100.0);
    }
    if let Some(pp) = pp {
        it.trail.push_back((now, pp));
        while it.trail.len() > 90 {
            it.trail.pop_front();
        }
    }

    // request tracking: started / native completion / replaced by something else
    let req = it.req;
    match req.active {
        None => {
            if st.0 == STATE_ANIMATE && st != req.before {
                log!(
                    "[bodydrag] clock={now:.0} gen {}: {} ACTIVE (state 0x{:08x} req 0x{:08x}) {:.0} ms after queue",
                    it.gen, req.phase.clip(), st.1, st.2, now - req.queued
                );
                it.req.active = Some((st, now));
                if let Some(pr) = it.probe {
                    log!("[bodydrag]   latency {}: detected->active {:.0} ms", if pr.start { "start" } else { "stop" }, now - pr.detected);
                }
            } else if now - req.queued > ACTIVATE_TIMEOUT_MS {
                log!("[bodydrag] clock={now:.0} gen {}: {} not seen starting after {ACTIVATE_TIMEOUT_MS} ms (state {:?}); continuing on the timer", it.gen, req.phase.clip(), st);
                it.req.active = Some((st, now));
            }
        }
        Some((ours, since)) => {
            if st != ours && st.0 == STATE_ANIMATE {
                // Still animating but a different state object / request: whether the engine re-creates these while
                // a clip runs is unverified, so follow it and log it instead of treating it as a takeover.
                log!("[bodydrag] clock={now:.0} gen {}: Animate state/request changed without our command ({:?} -> {:?}); following", it.gen, ours, st);
                it.req.active = Some((st, since));
            } else if st != ours {
                match req.phase.stop_ms() {
                    Some(len) if now - since >= len - STOP_END_EARLY_MS => {
                        log!("[bodydrag] clock={now:.0} gen {}: {} completed natively after {:.0} ms (state now {:?})", it.gen, req.phase.clip(), now - since, st);
                        if req.phase == Phase::Out {
                            finish(it, "qol_drag_out completed");
                            return false;
                        }
                        it.req.active = Some((ours, since - len)); // let the timer below advance at once
                    }
                    None if st.0 == 0 && now - it.last_recover > RECOVER_WINDOW_MS => {
                        // a looping drag clip dropped to the normal state (cause not identified in the v4 log):
                        // take it back once; a second drop within RECOVER_WINDOW_MS aborts
                        log!("[bodydrag] clock={now:.0} gen {}: {} dropped to state 0 - re-requesting it once", it.gen, req.phase.clip());
                        it.last_recover = now;
                        start_request(it, req.phase, st);
                    }
                    _ => {
                        abort(it, &format!("{} left the Animate state early: {:?} (ours {:?})", req.phase.clip(), st, ours));
                        return false;
                    }
                }
            }
        }
    }

    // F7: controlled release - qol_drag_out once (no root motion; movement keys are ignored in the Animate state and by
    // the phase logic below), the grip stays on and follows the release curve until RELEASE_S. Repeated F7 is ignored.
    if toggle && it.req.phase != Phase::Out {
        log!("[bodydrag] clock={now:.0} gen {}: F7 release requested - qol_drag_out, grip held until {RELEASE_S} s", it.gen);
        it.probe = None;
        it.pending = None;
        start_request(it, Phase::Out, st);
        return true;
    }

    // phase transitions
    if let Some((ours, since)) = it.req.active {
        match it.req.phase {
            Phase::In if now - since >= IN_MS => start_request(it, Phase::Idle, st),
            Phase::Out if it.cancel_queued => {
                // keep the generation current until our cancel has run, then end
                if !has_commands() {
                    finish(it, "drop animation cancelled after its length");
                    return false;
                }
            }
            Phase::Out if now - since >= OUT_MS + OUT_CANCEL_GRACE_MS => {
                if anim_state(p) == ours {
                    log!("[bodydrag] clock={now:.0} gen {}: qol_drag_out still active after {:.0} ms - cancelling our request", it.gen, now - since);
                    queue(it.gen, "CancelAnimation", "Actor.CancelAnimation(h)");
                    it.cancel_queued = true;
                } else {
                    finish(it, "drop animation no longer ours, nothing to cancel");
                    return false;
                }
            }
            cur if cur.is_hold() => {
                // the clip follows the keys: a move/turn starts after START_DEBOUNCE_MS, returning to idle is immediate
                let want = hold_mode(p, pp, cur);
                if want == cur {
                    it.pending = None;
                    if cur.rotates() && now - since >= cur.loop_ms() - LOOP_REISSUE_EARLY_MS {
                        start_request(it, cur, st); // keep turning: the root yaw only applies over a first play-through
                    }
                } else if want == Phase::Idle {
                    log!("[bodydrag] clock={now:.0} gen {}: keys released -> idle (heading {})", it.gen, fmt_heading(p));
                    it.probe = pp.map(|p0| Probe { start: false, detected: now, detected_pos: p0 });
                    it.pending = None;
                    start_request(it, Phase::Idle, st);
                } else {
                    let since_k = match it.pending {
                        Some((m, t0)) if m == want => t0,
                        _ => {
                            it.pending = Some((want, now));
                            now
                        }
                    };
                    if now - since_k >= START_DEBOUNCE_MS {
                        log!("[bodydrag] clock={now:.0} gen {}: {:?} -> {:?} (heading {})", it.gen, cur, want, fmt_heading(p));
                        if cur == Phase::Idle && want.is_walk() {
                            it.probe = pp.map(|p0| Probe { start: true, detected: since_k, detected_pos: p0 });
                        }
                        it.pending = None;
                        start_request(it, want, st);
                    }
                }
            }
            _ => {}
        }
    }

    // Sean's voice and face, on the clip time of the playing drag clip (commands carry this interaction's generation,
    // so an aborted drag drops any that are still queued)
    let stage = match it.req.phase {
        Phase::In => voice::Stage::Lifting,
        Phase::Out => voice::Stage::Lowering,
        p if p.is_walk() => voice::Stage::Walking,
        _ => voice::Stage::Holding,
    };
    let clip_s = it.req.active.map(|(_, s)| (now - s) / 1000.0);
    for (label, call) in it.voice.tick(stage, clip_s, now) {
        if call.is_empty() {
            log!("[bodydrag] clock={now:.0} gen {}: {label}", it.gen);
        } else {
            queue(it.gen, &label, &call);
        }
    }

    // movement probe: when did Sean actually start / stop, and how far did he go after the key event?
    if let (Some(pr), Some(pp), Some(v)) = (it.probe, pp, trail_speed(&it.trail)) {
        let moved = hdist(pr.detected_pos, pp);
        if pr.start && v > 0.5 {
            log!("[bodydrag] clock={now:.0} gen {}: START measured: moving ({v:.2} m/s) {:.0} ms after the key, {moved:.2} m", it.gen, now - pr.detected);
            it.probe = None;
        } else if !pr.start && v < 0.05 {
            log!("[bodydrag] clock={now:.0} gen {}: STOP measured: stopped {:.0} ms after the key release, travelled {moved:.2} m", it.gen, now - pr.detected);
            it.probe = None;
        } else if now - pr.detected > 3000.0 {
            log!("[bodydrag] clock={now:.0} gen {}: {} not observed within 3 s (speed {v:.2} m/s, moved {moved:.2} m)", it.gen, if pr.start { "start" } else { "stop" });
            it.probe = None;
        }
    }

    // release clock: clip seconds since qol_drag_out became active (0 while it is queued)
    let release = (it.req.phase == Phase::Out).then(|| it.req.active.map_or(0.0, |(_, s)| ((now - s) / 1000.0) as f32));

    // detachment at RELEASE_S: the support is already zero; stop writing velocities, keep the body clear of Sean
    if let (Some(t), Some((b, grip))) = (release, it.hold) {
        if t >= RELEASE_S {
            it.hold = None;
            if let Ok(b) = still_valid(&b) {
                let vel = |i: Option<u32>| i.and_then(|i| bone_body(b.rd, i)).map(|x| lin_velocity(x));
                let fmt = |v: Option<[f32; 3]>| v.map_or("?".into(), |v| format!("{:.2}", (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()));
                let up = pp.map_or(f32::NAN, |pp| b.pos[1] - pp[1]);
                log!(
                    "[bodydrag] clock={now:.0} gen {}: DETACHED at clip t {t:.3} s - pelvis up {up:.2} m, |v| pelvis {} torso {} R/L shoulder {}/{} m/s",
                    it.gen, fmt(Some(lin_velocity(b.body))), fmt(vel(grip.torso)), fmt(vel(grip.rsh)), fmt(vel(grip.lsh))
                );
                it.clear = Some(b);
            }
        }
    }
    if let (Some(b), Some(pp), Some(t)) = (it.clear, pp, release) {
        match still_valid(&b) {
            Ok(b) => {
                let pushed = separate(b.rd, pp);
                if now - it.last_diag >= DIAG_MS / 2.0 {
                    it.last_diag = now;
                    log!("[bodydrag] clock={now:.0} gen {}: settling t {t:.3} s pelvis up {:.2} |v| {:.2} m/s{}", it.gen,
                        b.pos[1] - pp[1], dist(lin_velocity(b.body), [0.0; 3]),
                        if pushed > 0 { format!(" (+{pushed} pushed out of Sean)") } else { String::new() });
                }
                it.clear = Some(b);
            }
            Err(_) => it.clear = None,
        }
    }

    // pull the held body (unless its ragdoll update already did this frame)
    if let (Some((b, grip)), Some(pp)) = (it.hold, pp) {
        if PULLED_FRAME.load(Ordering::Relaxed) != FRAME.load(Ordering::Relaxed) {
            it.want_diag = now - it.last_diag >= if release.is_some() { DIAG_MS / 2.0 } else { DIAG_MS };
            match pull(it, &b, &grip, p, pp, release) {
                Ok((diag, fresh)) => {
                    // a replaced ragdoll object (pool) can be another type with other bone indices: resolve again
                    let grip = if fresh.rd != b.rd {
                        let names = ragdoll_bone_names(fresh.rd);
                        let g = grip_bones(&names);
                        log!("[bodydrag] clock={now:.0} gen {}: ragdoll replaced during the hold; {}", it.gen, grip_txt(&g, &names));
                        g
                    } else {
                        grip
                    };
                    it.hold = Some((fresh, grip));
                    if now - it.last_diag >= if release.is_some() { DIAG_MS / 2.0 } else { DIAG_MS } {
                        it.last_diag = now;
                        log!(
                            "[bodydrag] clock={now:.0} gen {} {:?}{}: player ({:.3},{:.3},{:.3}) heading {} camera look {} state {} keys {} speed {} | {diag}",
                            it.gen, it.req.phase, if it.req.active.is_some() { "" } else { " (queued)" }, pp[0], pp[1], pp[2],
                            heading(p).map(|y| format!("{:.1}", y.to_degrees())).unwrap_or_else(|| "?".into()),
                            camera_look(pp).map(|y| format!("{:.1}", y.to_degrees())).unwrap_or_else(|| "?".into()),
                            st.0, keys(), trail_speed(&it.trail).map(|v| format!("{v:.2}")).unwrap_or_else(|| "?".into())
                        );
                    }
                }
                Err(why) => {
                    it.hold = None;
                    if it.req.phase == Phase::Out {
                        log!("[bodydrag] clock={now:.0} gen {}: grip lost during the release ({why})", it.gen);
                    } else {
                        log!("[bodydrag] clock={now:.0} gen {}: released ({why}) - qol_drag_out", it.gen);
                        start_request(it, Phase::Out, st);
                    }
                }
            }
        }
    }
    true
}

// ───────────────────────── hooks ─────────────────────────

/// Ragdoll update hook: remember the body; pull it here while the engine still updates it.
unsafe fn after_update(rd: u32) {
    let now = GetTickCount();
    let Some(owner) = r32(rd + 0x348).filter(|&o| o != 0) else { return };
    if Some(owner) == player()
        || r32(owner + 0x10d0) != Some(rd)
        || !matches!(r32(owner + 0x123c), Some(STATE_RAGDOLL) | Some(STATE_DYING))
    {
        return;
    }
    let Some(body) = pelvis(rd) else { return };
    if r32(body + 0xe0).filter(|&v| v > 0x0040_0000).is_none() {
        return;
    }
    let (Some(pos), Some(vt)) = (rvec(body + 0x120), r32(owner)) else { return };
    LAST_RAGDOLL_MS.store(now.max(1), Ordering::Relaxed);
    let mut known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let entry = Body { owner, vt, rd, body, pos, ms: now };
    match known.iter_mut().find(|b| b.owner == owner) {
        Some(b) => *b = entry,
        None => {
            if known.len() >= KNOWN_MAX {
                // make room from entities that are gone first; only a registry full of valid corpses drops the
                // least recently updated one
                known.retain(|b| human_ok(b).is_ok());
                if known.len() >= KNOWN_MAX {
                    known.sort_by_key(|b| b.ms);
                    known.remove(0);
                }
            }
            known.push(entry);
        }
    }
}

pub unsafe extern "thiscall" fn update_detour(rd: *mut c_void, dt: f32, a2: u32, a3: *mut c_void) -> u32 {
    let Some(orig) = ORIG.get() else { return 0 };
    let armed = {
        let _t = perf::Timer::new("ragdoll hook (before)", perf::Kind::Ours);
        lifecycle::before_update(rd as u32)
    };
    let ret = {
        let _t = perf::Timer::new("engine ragdoll update", perf::Kind::Engine);
        orig(rd, dt, a2, a3)
    };
    let _t = perf::Timer::new("ragdoll hook (after)", perf::Kind::Ours);
    lifecycle::after_update(rd as u32, armed);
    after_update(rd as u32);
    ret
}

/// Once per frame from the game-camera tick (game thread): advance the game clock by dt, then F7 and the
/// interaction.
pub unsafe fn frame(dt: f32) {
    for line in perf::tick() {
        log!("{line}");
    }
    let _t = perf::Timer::new("frame tick (F7, carry, wake, retention, prompt)", perf::Kind::Ours);
    if FRAME.fetch_add(1, Ordering::Relaxed) == 0 {
        log!("[bodydrag] first game-camera tick (dt {dt:.4}); action key {} ({})", input::binding().label, input::binding().note);
    }
    prompt_tick();
    if dt.is_finite() && dt > 0.0 && dt < 1.0 {
        CLOCK.store((clock() + dt as f64 * 1000.0).to_bits(), Ordering::Relaxed);
    }
    let real = GetTickCount();
    let t = TOGGLE_MS.swap(0, Ordering::SeqCst);
    let toggle = t != 0 && real.wrapping_sub(t) < 1000;
    if t != 0 && !toggle {
        log!("[bodydrag] clock={:.0} F7 RESULT: REQUEST_EXPIRED (press {} ms old when the game thread ran)", clock(), real.wrapping_sub(t));
    }
    LAST_FRAME_MS.store(real, Ordering::Relaxed);
    let mut inter = INTER.lock().unwrap_or_else(|e| e.into_inner());
    match inter.as_mut() {
        Some(it) => {
            if toggle && it.req.phase == Phase::Out {
                log!("[bodydrag] clock={:.0} F7 RESULT: INTERACTION_BUSY (release in progress, gen {})", clock(), it.gen);
            }
            if !step(it, toggle) {
                *inter = None;
                lifecycle::set_dragged(0);
            }
        }
        None => {
            let Some(p) = player() else {
                if toggle {
                    log!("[bodydrag] clock={:.0} F7 RESULT: NO_CANDIDATE (no player object)", clock());
                }
                return;
            };
            let holster = *HOLSTER.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((owner, since)) = holster {
                if toggle {
                    log!("[bodydrag] clock={:.0} F7 RESULT: INTERACTION_BUSY (holstering before the pickup)", clock());
                }
                let in_hand = weapon_in_hand(p);
                if in_hand == 0 || clock() - since > HOLSTER_TIMEOUT_MS {
                    *HOLSTER.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    log!(
                        "[bodydrag] clock={:.0} {} after {:.0} ms; picking up 0x{owner:08x}",
                        clock(), if in_hand == 0 { "weapon holstered" } else { "holster not confirmed (item still in hand)" }, clock() - since
                    );
                    match find_body(p, Some(owner)) {
                        Pick::Grab(b, grip) => *inter = Some(begin(p, b, grip)),
                        _ => lifecycle::set_dragged(0),
                    }
                }
                return;
            }
            match lifecycle::poll() {
                lifecycle::Poll::Busy => {
                    if toggle {
                        log!("[bodydrag] clock={:.0} F7 RESULT: INTERACTION_BUSY (ragdoll restore in progress)", clock());
                    }
                    return;
                }
                lifecycle::Poll::Done(owner) => {
                    log!("[bodydrag] clock={:.0} restore 0x{owner:08x} complete; grabbing it", clock());
                    if let Pick::Grab(b, grip) = find_body(p, Some(owner)) {
                        *inter = start_grab(p, b, grip);
                    }
                    return;
                }
                lifecycle::Poll::Failed(owner, why) => {
                    log!("[bodydrag] clock={:.0} F7 RESULT: RESTORE_FAILED 0x{owner:08x} ({why}); the corpse keeps its static pose", clock());
                }
                lifecycle::Poll::Idle => {}
            }
            if toggle {
                match find_body(p, None) {
                    Pick::Grab(b, grip) => *inter = start_grab(p, b, grip),
                    Pick::Restoring | Pick::Refused => {}
                }
            }
        }
    }
    let busy = inter.is_some();
    drop(inter);
    retain_nearby(busy);
}

/// Pickup waiting for Sean to stow his weapon: (corpse, game clock when the holster was requested).
static HOLSTER: Mutex<Option<(u32, f64)>> = Mutex::new(None);
const HOLSTER_TIMEOUT_MS: f64 = 1500.0;
const ITEM_IN_HAND: usize = 0x0049_97f0; // fastcall(human+0x1144): the item handle's object, 0 when empty-handed
const HAND_SLOT: u32 = 0x1144;

/// The object in Sean's hand (FUN_004997f0 on human+0x1144, the slot the stow FSM FUN_004f29b0 checks), 0 if none.
unsafe fn weapon_in_hand(p: u32) -> u32 {
    let slot = p + HAND_SLOT;
    match r32(slot + 4) {
        Some(h) if readable(h, 8) => {
            let get: unsafe extern "fastcall" fn(u32) -> u32 = core::mem::transmute(ITEM_IN_HAND);
            get(slot)
        }
        _ => 0,
    }
}

/// Begin the pickup, first stowing a weapon in Sean's hand with the game's own holster (Inventory.HolsterWeapons ->
/// the stow FSM); the drag starts once the hand is empty or after HOLSTER_TIMEOUT_MS. The corpse's ragdoll is
/// protected from recycling meanwhile.
unsafe fn start_grab(p: u32, b: Body, grip: Grip) -> Option<Interaction> {
    let item = weapon_in_hand(p);
    if item == 0 {
        return Some(begin(p, b, grip));
    }
    lifecycle::set_dragged(b.owner);
    queue(GEN.load(Ordering::SeqCst), "holster", "Inventory.HolsterWeapons(h)");
    *HOLSTER.lock().unwrap_or_else(|e| e.into_inner()) = Some((b.owner, clock()));
    log!("[bodydrag] clock={:.0} weapon in hand (0x{item:08x}): holstering before picking up 0x{:08x}", clock(), b.owner);
    None
}

static RETAIN_AT: AtomicU64 = AtomicU64::new(0);
const RETAIN_EVERY_MS: f64 = 500.0;
/// Real time of the last ragdoll registered by the update hook (only happens with a live world and a corpse).
static LAST_RAGDOLL_MS: AtomicU32 = AtomicU32::new(0);
const RETAIN_LIVE_MS: u32 = 2000;
const RETAIN_WARMUP_FRAMES: u32 = 300;
static RETAIN_STARTED: AtomicBool = AtomicBool::new(false);

// ───────────────────────── HUD prompt ─────────────────────────

static HOVER: Mutex<Option<u32>> = Mutex::new(None);
static PROMPT: Mutex<prompt::Prompt> = Mutex::new(prompt::Prompt::new());
static PROMPT_AT: AtomicU64 = AtomicU64::new(0);
const PROMPT_EVERY_MS: f64 = 100.0;
/// The selected body keeps the prompt unless another one is this much nearer (no flicker in a pile).
const HOVER_KEEP_M: f32 = 0.3;

/// The body the HUD prompt currently names (F7 takes it first).
fn hover_selected() -> Option<u32> {
    *HOVER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Side-effect-free choice of the body F7 would pick up: the same reach checks as the grab (pelvis within
/// FIND_RADIUS, torso reach, not FIXED; a recycled corpse only with a captured pose to restore from), in front of
/// Sean, nearest first with hysteresis. Never wakes, restores or logs.
unsafe fn hover_pick(p: u32, pp: [f32; 3]) -> Option<u32> {
    let fwd = heading(p).map(|y| [y.sin(), y.cos()]);
    let in_front = |a: [f32; 3]| {
        let (dx, dz) = (a[0] - pp[0], a[2] - pp[2]);
        let n = (dx * dx + dz * dz).sqrt();
        n < 0.6 || fwd.map_or(true, |f| (dx * f[0] + dz * f[1]) / n >= 0.0)
    };
    let cur = hover_selected();
    let known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let mut best: Option<(f32, u32)> = None;
    for b in known.iter() {
        if human_ok(b).is_err() {
            continue;
        }
        let at = match still_valid_q(b, true) {
            Ok(f) if dist(f.pos, pp) <= FIND_RADIUS && live_reach(&f, pp).is_ok() => f.pos,
            Ok(_) => continue,
            Err(_) if frozen_state(b).is_some() && lifecycle::static_entry(b.owner).is_ok() => match human_pos(b.owner) {
                Some(a) if dist(a, pp) <= FIND_RADIUS => a,
                _ => continue,
            },
            Err(_) => continue,
        };
        if !in_front(at) {
            continue;
        }
        let d = dist(at, pp) - if Some(b.owner) == cur { HOVER_KEEP_M } else { 0.0 };
        if best.map_or(true, |(bd, _)| d < bd) {
            best = Some((d, b.owner));
        }
    }
    best.map(|(_, o)| o)
}

/// 10 Hz: decide the prompt and update the native entry only when its text changes.
unsafe fn prompt_tick() {
    let now = clock();
    if now - f64::from_bits(PROMPT_AT.load(Ordering::Relaxed)) < PROMPT_EVERY_MS {
        return;
    }
    PROMPT_AT.store(now.to_bits(), Ordering::Relaxed);
    if !input::prompt_enabled() {
        return;
    }
    let label = &input::binding().label;
    let carrying = INTER.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map(|it| it.req.phase);
    let busy = HOLSTER.lock().unwrap_or_else(|e| e.into_inner()).is_some() || lifecycle::in_progress();
    let want = match carrying {
        Some(Phase::In) | Some(Phase::Out) => None,
        Some(_) => Some(format!("Put down body [{label}]")),
        None if busy || FRAME.load(Ordering::Relaxed) < RETAIN_WARMUP_FRAMES || !prompt::hud_ready() => None,
        None => {
            let pick = if KNOWN.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
                None
            } else {
                player().and_then(|p| human_pos(p).and_then(|pp| hover_pick(p, pp)))
            };
            *HOVER.lock().unwrap_or_else(|e| e.into_inner()) = pick;
            pick.map(|_| format!("Pick up body [{label}]"))
        }
    };
    if carrying.is_some() || busy {
        *HOVER.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    let line = PROMPT.lock().unwrap_or_else(|e| e.into_inner()).set(want.as_deref());
    if let Some(line) = line {
        log!("{line}");
    }
}

/// A recycled corpse this close to Sean gets its ragdoll back in advance (silent wake), so F7 grabs it at once.
const PREWAKE_RADIUS: f32 = 2.5;
/// Passive wakes: at most one per this many ms (game clock), only within 75 deg of Sean's heading.
const PASSIVE_WAKE_GAP_MS: f64 = 3000.0;
const PASSIVE_WAKE_COS: f32 = 0.26;
static LAST_PASSIVE_WAKE: AtomicU64 = AtomicU64::new(0);

/// Bounded retention (lifecycle B): the nearest live corpses within RETAIN_RADIUS keep their ragdolls past the
/// engine's settle release, at most RETAIN_MAX, re-chosen every RETAIN_EVERY_MS. While Sean is not dragging, the
/// nearest recycled corpse within PREWAKE_RADIUS is woken passively (one at a time).
unsafe fn retain_nearby(busy: bool) {
    let now = clock();
    if now - f64::from_bits(RETAIN_AT.load(Ordering::Relaxed)) < RETAIN_EVERY_MS {
        return;
    }
    RETAIN_AT.store(now.to_bits(), Ordering::Relaxed);
    let _t = perf::Timer::new("retention / passive-wake scan", perf::Kind::Note);
    // v15 crashed to desktop during a level load before any F7, at the first camera ticks, where this was the only
    // new engine call (Sean's root through his motion component). Only ask for it once the world is clearly live:
    // a corpse registered, a ragdoll update within RETAIN_LIVE_MS, and RETAIN_WARMUP_FRAMES frames since attach.
    let live_ms = GetTickCount().wrapping_sub(LAST_RAGDOLL_MS.load(Ordering::Relaxed));
    if FRAME.load(Ordering::Relaxed) < RETAIN_WARMUP_FRAMES || live_ms > RETAIN_LIVE_MS {
        return;
    }
    let known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    if known.is_empty() {
        return;
    }
    drop(known);
    if !RETAIN_STARTED.swap(true, Ordering::Relaxed) {
        log!("[lifecycle] clock={now:.0} retention starting (frame {}, last ragdoll update {live_ms} ms ago)", FRAME.load(Ordering::Relaxed));
    }
    let Some(p) = player() else { return };
    let Some(pp) = human_pos(p) else { return };
    // passive wakes are engine-heavy (new ragdoll state, pool acquire / theft, activation): at most one per
    // PASSIVE_WAKE_GAP_MS, and only for a corpse in front of Sean, not every body he walks past
    let wake_ok = !busy && now - f64::from_bits(LAST_PASSIVE_WAKE.load(Ordering::Relaxed)) >= PASSIVE_WAKE_GAP_MS;
    let fwd = heading(p).map(|y| [y.sin(), y.cos()]);
    let in_front = |a: [f32; 3]| {
        let (dx, dz) = (a[0] - pp[0], a[2] - pp[2]);
        let n = (dx * dx + dz * dz).sqrt();
        n < 0.5 || fwd.is_some_and(|f| (dx * f[0] + dz * f[1]) / n >= PASSIVE_WAKE_COS)
    };
    let known = KNOWN.lock().unwrap_or_else(|e| e.into_inner());
    let mut cands: Vec<(f32, u32)> = Vec::new();
    let mut frozen: Option<(f32, Body)> = None;
    for b in known.iter() {
        match still_valid(b) {
            // Only low-detail (type 1) ragdolls are retained: the pool has just 3 type-0 objects for NPCs
            // (FUN_00590940), and keeping them busy pushes the next corpse Sean picks up onto type 1.
            Ok(f) if r32(f.rd + 0x36c) != Some(0) => cands.push((dist(f.pos, pp), f.owner)),
            Ok(_) => {}
            Err(_) if wake_ok && human_ok(b).is_ok() && frozen_state(b).is_some() && !lifecycle::recently_woken(b.owner) => {
                if let Some(d) = human_pos(b.owner).filter(|&a| in_front(a)).map(|a| dist(a, pp)).filter(|&d| d <= PREWAKE_RADIUS) {
                    if frozen.as_ref().map_or(true, |f| d < f.0) {
                        frozen = Some((d, *b));
                    }
                }
            }
            Err(_) => {}
        }
    }
    drop(known);
    let picked = lifecycle::pick_retained(cands, lifecycle::dragged());
    let txt = picked.iter().map(|o| format!("0x{o:08x}")).collect::<Vec<_>>().join(", ");
    if lifecycle::set_retained(picked) {
        log!("[lifecycle] clock={now:.0} retained ragdolls (max {}, {} m): [{txt}]", lifecycle::RETAIN_MAX, lifecycle::RETAIN_RADIUS);
    }
    if let Some((d, b)) = frozen {
        if HOLSTER.lock().unwrap_or_else(|e| e.into_inner()).is_none() {
            LAST_PASSIVE_WAKE.store(now.to_bits(), Ordering::Relaxed);
            match lifecycle::start(b.owner, b.vt, false) {
                Ok(txt) => log!("[lifecycle] clock={now:.0} wake 0x{:08x} ({d:.2} m, passive): {txt}", b.owner),
                Err(why) => log!("[lifecycle] clock={now:.0} wake 0x{:08x} ({d:.2} m, passive) skipped: {why}", b.owner),
            }
        }
    }
}

/// Voice settings for this grab: `sab_bodydrag_voice.ini` beside the exe if present (re-read every grab), else defaults.
fn voice_cfg() -> voice::Cfg {
    let path = crate::game_file("sab_bodydrag_voice.ini");
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let (cfg, bad) = voice::Cfg::parse(&text);
            if !bad.is_empty() {
                log!("[bodydrag] {}: ignored lines {:?}", path.display(), bad);
            }
            log!("[bodydrag] voice settings from {}: {cfg:?}", path.display());
            cfg
        }
        Err(_) => voice::Cfg::default(),
    }
}

/// Start an interaction on a validated body.
unsafe fn begin(p: u32, b: Body, grip: Grip) -> Interaction {
    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let mut it = Interaction {
        gen,
        player: p,
        hold: Some((b, grip)),
        frame_dt: 16.0,
        sh_i: [0.0; 2],
        grab_clock: clock(),
        req: Request { phase: Phase::In, queued: clock(), before: anim_state(p), active: None },
        pending: None,
        probe: None,
        trail: VecDeque::new(),
        last_diag: 0.0,
        cancel_queued: false,
        voice: voice::Voice::new(voice_cfg(), GetTickCount()),
        last_recover: -1.0e9,
        clear: None,
        posture: (0, 0),
        downgrade: None,
        want_diag: false,
    };
    lifecycle::set_dragged(b.owner);
    log!(
        "[bodydrag] clock={:.0} gen {gen}: interaction started (player 0x{p:08x}, body 0x{:08x}, ragdoll 0x{:08x} type {}); ragdoll protected from recycling",
        clock(), b.owner, b.rd, r32(b.rd + 0x36c).map_or("?".into(), |t| t.to_string())
    );
    let before = anim_state(p);
    start_request(&mut it, Phase::In, before);
    it
}

/// One-line state for the detach log; never blocks (the game thread may have died holding a lock).
pub fn exit_summary() -> String {
    let inter = match INTER.try_lock() {
        Ok(g) => g.as_ref().map_or("no interaction".to_string(), |it| format!("interaction gen {} ACTIVE ({:?})", it.gen, it.req.phase)),
        Err(_) => "interaction state locked".to_string(),
    };
    format!("{inter}, last frame {} ms before detach", unsafe { GetTickCount() }.wrapping_sub(LAST_FRAME_MS.load(Ordering::Relaxed)))
}

static LAST_FRAME_MS: AtomicU32 = AtomicU32::new(0);

/// Called from the console thread on F7.
pub fn request_toggle() {
    TOGGLE_MS.store(unsafe { GetTickCount() }.max(1), Ordering::SeqCst);
}


#[cfg(test)]
mod tests {
    use super::*;

    fn rotate(v: [f32; 3], w: [f32; 3], dt: f32) -> [f32; 3] {
        // small-angle rotation of v by angular velocity w over dt: v + (w x v) dt
        let c = cross(w, v);
        norm([v[0] + c[0] * dt, v[1] + c[1] * dt, v[2] + c[2] * dt])
    }

    #[test]
    fn low_detail_ragdoll_gets_the_underarm_grip_on_its_upper_arms() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let full = grip_bones(&s(&["RB_HIP", "RB_TORSO", "RB_HEAD", "RB_LEFT_SHOULDER", "RB_RIGHT_SHOULDER", "RB_LEFT_UPPER_ARM", "RB_RIGHT_UPPER_ARM"]));
        assert_eq!((full.lsh, full.rsh, full.arms), (Some(3), Some(4), false));
        let low = grip_bones(&s(&["RB_HIP", "RB_TORSO", "RB_HEAD", "RB_LEFT_UPPER_ARM", "RB_RIGHT_UPPER_ARM"]));
        assert_eq!((low.lsh, low.rsh, low.arms), (Some(3), Some(4), true), "upper arms stand in");
        let none = grip_bones(&s(&["RB_HIP", "RB_TORSO"]));
        assert!(none.lsh.is_none() && grip_txt(&none, &s(&["RB_HIP", "RB_TORSO"])).starts_with("TORSO PULL"));
    }

    #[test]
    fn posture_needs_orientation_not_just_target_distance() {
        assert!(posture_ok(0.1, 0.1, 10.0, 12.0));
        assert!(!posture_ok(0.1, 0.1, 113.0, 12.0), "v19 gen 43: small misses, shoulder line 113 deg off");
        assert!(!posture_ok(0.1, 0.1, 10.0, f32::NAN), "spine unknown is not OK");
        assert!(!posture_ok(0.3, 0.1, 10.0, 12.0));
    }

    #[test]
    fn spine_pointing_the_wrong_way_still_gets_a_pitch_correction() {
        // audit case: yaw 0, spine exactly opposite the wanted one, shoulder line already right
        let want = [0.0f32, 0.951057, -0.309017];
        let now = [0.0f32, -0.951057, 0.309017];
        let line = [-1.0f32, 0.0, 0.0];
        assert!(cross(now, want).iter().all(|c| c.abs() < 1e-6), "the plain cross product is zero here");
        let e = roll_term(now, want, line);
        assert!((e[0].abs() - 1.0).abs() < 1e-6, "unit pitch about the shoulder line: {e:?}");
        // and it converges
        let mut v = norm([0.0, -0.95, 0.31]);
        let mut t = 0.0;
        while angle_deg(v, want) > 10.0 && t < 3.0 {
            let w = roll_term(v, want, line).map(|c| (c * SPIN_GAIN).clamp(-SPIN_MAX, SPIN_MAX));
            v = rotate(v, w, 0.01);
            t += 0.01;
        }
        assert!(angle_deg(v, want) <= 10.0, "{:.0} deg after {t:.2} s", angle_deg(v, want));
    }

    #[test]
    fn lateral_spread_only_once_the_shoulder_line_is_roughly_right() {
        assert_eq!(lateral_weight(-1.0), 0.0); // reversed: both shoulders to the midpoint
        assert_eq!(lateral_weight(0.0), 0.0); // 90 deg
        assert!((lateral_weight(0.5) - 0.577).abs() < 0.01); // 60 deg
        assert_eq!(lateral_weight(0.9), 1.0); // within 30 deg: the v14 targets
        assert_eq!(lateral_weight(1.0), 1.0);
    }

    #[test]
    fn reversed_corpse_rolls_round_about_its_spine() {
        // corpse shoulder line exactly opposite Sean's right, spine along Sean's forward-up: the old term
        // cross(line, right) is zero here; the new one must roll the line back within a few seconds
        let right = [-1.0f32, 0.0, 0.0];
        let spine = norm([0.0, 0.95, 0.3]);
        let old = cross([1.0, 0.0, 0.0], right);
        assert!(old.iter().all(|c| c.abs() < 1e-6));
        let mut axis = norm([1.0, 0.0, -0.3 * 0.0]); // reversed
        // keep the line perpendicular to the spine like a rigid torso
        let d = axis[0] * spine[0] + axis[1] * spine[1] + axis[2] * spine[2];
        axis = norm([axis[0] - d * spine[0], axis[1] - d * spine[1], axis[2] - d * spine[2]]);
        let mut t = 0.0;
        while angle_deg(axis, right) > 10.0 && t < 3.0 {
            let w = roll_term(axis, right, spine).map(|c| (c * SPIN_GAIN).clamp(-SPIN_MAX, SPIN_MAX));
            axis = rotate(axis, w, 0.01);
            t += 0.01;
        }
        assert!(angle_deg(axis, right) <= 10.0, "still {:.0} deg off after {t:.2} s", angle_deg(axis, right));
        // within 90 deg the term is the v14 cross product
        let a = norm([-1.0, 0.0, 0.5]);
        assert_eq!(roll_term(a, right, spine), cross(a, right));
    }

    #[test]
    fn right_vector_is_seans_right() {
        // heading convention: forward = (sin, 0, cos); his left is +X at yaw 0, so right = -X
        for yaw in [0.0f32, 1.0, -2.0, 3.0] {
            let f = [yaw.sin(), 0.0, yaw.cos()];
            let right = [-yaw.cos(), 0.0, yaw.sin()];
            let up = [0.0, 1.0, 0.0];
            // right = forward x up for a Y-up frame whose left is +X at yaw 0
            let fxu = cross(f, up);
            for k in 0..3 {
                assert!((fxu[k] - right[k]).abs() < 1e-5, "yaw {yaw}: {fxu:?} vs {right:?}");
            }
        }
    }

    #[test]
    fn spine_target_points_back_and_up() {
        let yaw = 0.7f32;
        let f = [yaw.sin(), 0.0, yaw.cos()];
        let p = SPINE_PITCH.to_radians();
        let want = norm([-f[0] * p.cos(), p.sin(), -f[2] * p.cos()]);
        assert!((want[1] - p.sin()).abs() < 1e-5, "up component {}", want[1]);
        assert!(want[0] * f[0] + want[2] * f[2] < -0.2, "must point back toward Sean");
    }

    #[test]
    fn spin_correction_reduces_orientation_error() {
        let want = norm([0.3, 0.8, -0.5]);
        let mut now = norm([0.9, -0.2, 0.4]);
        let start = angle_deg(now, want);
        for _ in 0..200 {
            let e = cross(now, want);
            let w = [e[0] * SPIN_GAIN, e[1] * SPIN_GAIN, e[2] * SPIN_GAIN];
            now = rotate(now, w, 1.0 / 60.0);
        }
        let end = angle_deg(now, want);
        assert!(end < start * 0.1, "error {start:.1} -> {end:.1} deg");
    }

    #[test]
    fn integral_term_removes_steady_sag() {
        // 1-D model of one shoulder: each frame its velocity is set to P + gravity feed-forward + integral, then the
        // body's weight pulls it down by an unknown constant per frame (the legs hanging from it).
        let (dt, target) = (1.0f32 / 30.0, 1.22f32);
        let (mut y, mut i) = (0.6f32, 0.0f32);
        let weight_loss = 1.5f32; // m/s lost every frame to the hanging body (the v8 log showed ~0.2 m sag at P gain 8)
        let mut max_over = 0.0f32;
        for frame in 0..600 {
            if frame > 48 {
                i = (i + (target - y) * SH_I_GAIN * dt).clamp(-SH_I_MAX, SH_I_MAX);
            }
            let v = (target - y) * SH_GAIN + GRAVITY * dt + i - weight_loss;
            y += v * dt;
            max_over = max_over.max(y - target);
        }
        assert!((target - y).abs() < 0.01, "steady error {:.3} m", target - y);
        assert!(max_over < 0.15, "overshoot {max_over:.3} m");
        assert!(i.abs() < SH_I_MAX, "integral saturated at {i}");
    }

    /// Fake human / ragdoll-state / ragdoll objects in test memory (i686: addresses fit u32).
    struct Fake {
        human: Vec<u8>,
        state: Vec<u8>,
        rd: Vec<u8>,
    }
    impl Fake {
        fn new() -> Self {
            Fake { human: vec![0; 0x1300], state: vec![0; 0x200], rd: vec![0; 0x400] }
        }
        fn put(buf: &mut [u8], off: usize, v: u32) {
            buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn h(&self) -> u32 {
            self.human.as_ptr() as u32
        }
        /// A corpse in state 3 with a WSHumanStateRagdoll object; `own` = the ragdoll points back at it.
        fn corpse(&mut self, own: bool, flag: u8) -> Body {
            let (h, st, rd) = (self.h(), self.state.as_ptr() as u32, self.rd.as_ptr() as u32);
            Self::put(&mut self.human, 0, 0x00fa_0000);
            Self::put(&mut self.human, 0x123c, STATE_RAGDOLL);
            Self::put(&mut self.human, 0x1238, st);
            Self::put(&mut self.human, 0x10d0, rd);
            Self::put(&mut self.state, 0, STATE_RAGDOLL_VTABLE);
            self.state[0xbd] = flag;
            Self::put(&mut self.rd, 0x348, if own { h } else { 0x1234_5678 });
            Body { owner: h, vt: 0x00fa_0000, rd, body: 0, pos: [0.0; 3], ms: 0 }
        }
    }

    #[test]
    fn identity_check_rejects_replaced_or_revived_entities() {
        let mut f = Fake::new();
        let b = f.corpse(true, 1);
        assert_eq!(unsafe { human_ok(&b) }, Ok(STATE_RAGDOLL));
        assert!(unsafe { human_ok(&Body { vt: 0x00fb_0000, ..b }) }.is_err(), "address reused by another class");
        Fake::put(&mut f.human, 0x123c, 0);
        assert!(unsafe { human_ok(&b) }.is_err(), "got up / reused as a live human");
        assert!(unsafe { human_ok(&Body { owner: 0x100, ..b }) }.is_err(), "unreadable memory");
    }

    #[test]
    fn frozen_corpse_is_recognised_only_without_its_own_ragdoll() {
        let mut f = Fake::new();
        let b = f.corpse(false, 1);
        let st = f.state.as_ptr() as u32;
        assert_eq!(unsafe { frozen_state(&b) }, Some(st), "pool gave its ragdoll to another human");
        Fake::put(&mut f.human, 0x10d0, 0);
        assert_eq!(unsafe { frozen_state(&b) }, Some(st), "link cleared by the engine");
        let b = f.corpse(true, 1);
        assert_eq!(unsafe { frozen_state(&b) }, None, "has its own ragdoll: live, not frozen");
        let b = f.corpse(false, 0);
        assert_eq!(unsafe { frozen_state(&b) }, None, "acquire flag clear: engine is already acquiring");
        let b = f.corpse(false, 1);
        Fake::put(&mut f.state, 0, 0x00fa_b000);
        assert_eq!(unsafe { frozen_state(&b) }, None, "not a WSHumanStateRagdoll");
        let b = f.corpse(false, 1);
        Fake::put(&mut f.human, 0x123c, STATE_DYING);
        assert_eq!(unsafe { frozen_state(&b) }, None, "dying state has no acquire flag");
    }

    #[test]
    fn release_starts_at_the_hold_and_hands_over_at_the_marker() {
        let hold = release_pose(None);
        let start = release_pose(Some(0.0));
        for (a, b) in [(hold.0, start.0), (hold.1, start.1), (hold.2, start.2), (hold.3, start.3)] {
            assert!((a - b).abs() < 1e-5, "release frame 0 must continue the hold: {hold:?} vs {start:?}");
        }
        assert_eq!(release_pose(Some(RELEASE_LET_S)).3, 1.0, "full support while the hands still grip");
        assert!((release_pose(Some(0.9)).3 - 0.5).abs() < 1e-4, "support fades linearly");
        assert_eq!(release_pose(Some(RELEASE_S)).3, 0.0, "no support at Velvet's ragdoll marker");
        let (fwd, up, spine, _) = release_pose(Some(RELEASE_S));
        assert!(fwd > 0.3 && up < -0.4, "lowered and moved out in front of Sean: fwd {fwd} up {up}");
        assert!(spine.cos() > 0.3, "spine leans away from Sean at the release ({:.0} deg)", spine.to_degrees());
        assert!(hold.2.cos() < 0.0, "spine leans back on Sean while held");
        // monotonic time sampling, independent of the frame step
        let a = release_pose(Some(0.5));
        let b = release_pose(Some(0.5 + 1e-4));
        assert!((a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3);
    }

    #[test]
    fn keys_pick_camera_relative_travel() {
        let look = 0.4f32;
        let right = [-look.cos(), look.sin()]; // (x, z), see right_vector_is_seans_right
        let dir = |h: f32| [h.sin(), h.cos()];
        let dot = |a: [f32; 2], b: [f32; 2]| a[0] * b[0] + a[1] * b[1];
        let t = |w, a, s, d| travel_heading(look, w, a, s, d).unwrap();
        assert!(dot(dir(t(true, false, false, false)), dir(look)) > 0.999, "W along the look");
        assert!(dot(dir(t(false, false, true, false)), dir(look)) < -0.999, "S toward the camera");
        assert!(dot(dir(t(false, false, false, true)), right) > 0.999, "D to the right");
        assert!(dot(dir(t(false, true, false, false)), right) < -0.999, "A to the left");
        let wd = dir(t(true, false, false, true));
        assert!((dot(wd, dir(look)) - dot(wd, right)).abs() < 1e-4 && dot(wd, right) > 0.7, "W+D halfway");
        assert!(travel_heading(look, true, false, true, false).is_none(), "W+S cancel");
    }

    #[test]
    fn steering_tiers_by_error_with_hysteresis() {
        let pi = core::f32::consts::PI;
        let deg = |d: f32| d.to_radians();
        assert_eq!(steer_clip(pi, 0.01, Phase::Idle), Phase::TurnL, "180 deg: pivot");
        assert_eq!(steer_clip(deg(-140.0), 0.0, Phase::Walk), Phase::HardR, "140 deg right: effort arc, no pivot");
        assert_eq!(steer_clip(deg(90.0), 0.0, Phase::Idle), Phase::HardL, "90 deg: effort arc");
        assert_eq!(steer_clip(deg(45.0), 0.0, Phase::Walk), Phase::HardL, "45 deg: effort arc");
        assert_eq!(steer_clip(deg(-20.0), 0.0, Phase::Walk), Phase::WalkR, "20 deg: normal arc");
        assert_eq!(steer_clip(deg(120.0), 0.0, Phase::TurnL), Phase::TurnL, "pivot holds above PIVOT_EXIT");
        assert_eq!(steer_clip(deg(100.0), 0.0, Phase::TurnL), Phase::HardL, "pivot ends below PIVOT_EXIT");
        assert_eq!(steer_clip(deg(20.0), 0.0, Phase::HardL), Phase::HardL, "effort holds above EFFORT_EXIT");
        assert_eq!(steer_clip(deg(12.0), 0.0, Phase::HardL), Phase::WalkL, "effort ends below EFFORT_EXIT");
        assert_eq!(steer_clip(deg(6.0), 0.0, Phase::WalkL), Phase::WalkL, "arc holds above STEER_EXIT");
        assert_eq!(steer_clip(deg(6.0), 0.0, Phase::Walk), Phase::Walk, "6 deg: straight");
        assert_eq!(steer_clip(pi - 0.3, -pi + 0.1, Phase::Walk), Phase::WalkR, "wraps across +-180 (23 deg right)");
    }

    /// Heading rate (deg/s) and whether Sean walks, per clip, from the v17 bake (yaw per loop / loop length).
    fn rate(p: Phase) -> (f32, bool) {
        match p {
            Phase::TurnL | Phase::TurnR => (45.0 / 0.587, false),
            Phase::HardL | Phase::HardR => (45.0 / 0.611, true),
            Phase::WalkL | Phase::WalkR => (30.0 / 0.733, true),
            _ => (0.0, true),
        }
    }

    /// Time until the heading is within STEER_EXIT of the wanted one, and the time spent turning in place.
    fn align(start_err_deg: f32) -> (f32, f32) {
        let (mut h, want) = (0.0f32, start_err_deg.to_radians());
        let (mut cur, mut t, mut still) = (Phase::Walk, 0.0f32, 0.0f32);
        let dt = 0.01;
        while t < 10.0 {
            cur = steer_clip(want, h, cur);
            if cur == Phase::Walk {
                break;
            }
            let (r, walks) = rate(cur);
            let s = if matches!(cur, Phase::TurnL | Phase::HardL | Phase::WalkL) { 1.0 } else { -1.0 };
            h += s * r.to_radians() * dt;
            t += dt;
            if !walks {
                still += dt;
            }
        }
        (t, still)
    }

    #[test]
    fn direction_changes_align_quickly_and_mostly_while_moving() {
        let (t45, s45) = align(45.0);
        let (t90, s90) = align(90.0);
        let (t135, s135) = align(-135.0);
        let (t180, s180) = align(179.0);
        // v16 (100/45 pivot, 48 deg/s pivot, 41 deg/s arc): 90 deg = 2.1 s, 180 deg = 2.8 s in place + 1.0 s arcing
        assert!(t45 < 0.8 && s45 == 0.0, "45 deg: {t45:.2} s");
        assert!(t90 < 1.4 && s90 == 0.0, "90 deg: {t90:.2} s");
        assert!(t135 < 2.0 && s135 == 0.0, "135 deg turns while walking: {t135:.2} s");
        assert!(s180 < 1.0 && t180 < 2.6, "180 deg: {s180:.2} s in place, {t180:.2} s total");
    }
}
