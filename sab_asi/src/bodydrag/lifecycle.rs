//! Corpse lifecycle: keep the dragged and a few nearby corpses' ragdolls out of the engine's recycling, and give a
//! recycled corpse a ragdoll again through the engine's own go-ragdoll (the explosion path), with the static pose the
//! engine captured for it applied to the activated bodies.
//! Native trace: saboteur-camera-mod/bodydrag/lifecycle/LIFECYCLE_NOTES.md.
//!
//! Everything here runs on the game thread: `before_update` / `after_update` inside the ragdoll-update detour
//! (FUN_00590db0), `poll` / `set_retained` from the game-camera tick.

use super::{clock, human_pos, r32, r8, ragdoll_bone_names, readable, rvec, skeleton_bone_names, MemInfo, VirtualQuery, FRAME};
use core::ffi::c_void;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

// native (GOG exe; LIFECYCLE_NOTES.md)
/// WSHuman go-ragdoll (thiscall human, 13 args): builds a new WSHumanStateRagdoll from the arguments and switches to
/// it; with `force` it also runs for a human already in state 3. Callers: the explosion/impact handler 0x99bd00 and
/// WSHumanStateRagdoll itself (0x5783d7: 0.0, 1.0, zero vectors, force 1).
const GO_RAGDOLL: usize = 0x004f_fa50;
const SET_BODY: usize = 0x0058_ea90; // thiscall(rd, bone, {pos, quat}*, linvel*): setPosition/Rotation, velocities
const STATE_SET_ACQUIRED: usize = 0x0057_5b20; // thiscall(WSHumanStateRagdoll, u8): [state+0xbd] = arg (ret 4)
/// A woken body whose bodies end further than this from the captured pose at the handover is reported as not held.
const SYNC_OK_M: f32 = 0.25;
const RING_BASE: u32 = 0x0123_fd30; // static pose ring: per type 30 x {human, hkaPose*, type}
const RING_TYPE_STRIDE: u32 = 0x16c;
const RING_ENTRY: u32 = 0xc;
const RING_SLOTS: u32 = 30;
const RD_OWNER: u32 = 0x348;
const RD_TYPE: u32 = 0x36c;
const RD_ACTIVE: u32 = 0xb0;
const RD_AGE: u32 = 0xc4;
const H_RAGDOLL: u32 = 0x10d0;
const H_STATIC: u32 = 0x10d4;

/// The dragged body's age is held below every theft candidate's (FUN_00590940 starts its oldest-search at -1.0) and
/// far below the 7.0 s release age.
pub const DRAGGED_AGE: f32 = -2.0;
/// Retained corpses: above the engine's 5.0 s early-wake window and theft preference threshold, below the 7.0 s release
/// age. They stay stealable under pool pressure (and are then captured natively, so they can be restored later).
pub const RETAIN_AGE_CAP: f32 = 6.0;
pub const RETAIN_MAX: usize = 3;
pub const RETAIN_RADIUS: f32 = 15.0;
const RESTORE_TIMEOUT_MS: f64 = 1500.0;
/// After the F7 result is reported as a timeout, the new ragdoll state keeps asking the pool natively; a ragdoll
/// it gets within this window still has the captured pose applied.
const RESTORE_WATCH_MS: f64 = 30000.0;
const MAX_BONES: u32 = 64;
const MAX_BONE_OFFSET: f32 = 3.0; // m, any captured bone from the root
const MAX_PELVIS_OFFSET: f32 = 2.5;

#[repr(C, align(16))]
struct Xf {
    pos: [f32; 4],
    rot: [f32; 4],
}
#[repr(C, align(16))]
struct V4([f32; 4]);

/// One captured model-space bone (hkQsTransform: translation, rotation quaternion xyzw, scale).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bone {
    pub t: [f32; 3],
    pub q: [f32; 4],
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    Requested,
    /// Bodies placed at the captured pose in this frame; the static pose is handed over on the next update.
    Placed { rd: u32, frame: u32 },
    Done { rd: u32 },
}

struct Restore {
    owner: u32,
    vt: u32,
    /// The engine's captured pose (ragdoll type, model-space bones, its skeleton's bone names), copied before
    /// go-ragdoll; the acquire clears the slot's owner field but leaves the pose object in place.
    snap: Option<(u32, Vec<Bone>, Vec<String>)>,
    /// Static pose slot and its pose object (0 if none): drawn again from the acquire until the bodies are placed.
    entry: u32,
    pose_obj: u32,
    holding: bool,
    /// The WSHumanStateRagdoll go-ragdoll created (its acquire flag +0xbd is what withdraws an unserved request).
    state: u32,
    /// Placed bodies and their target positions, checked at the handover.
    targets: Vec<(u32, [f32; 3])>,
    root: [f32; 3],
    started: f64,
    stage: Stage,
    reported: bool,
    /// F7 restore (grab when done) or a passive wake (no grab).
    grab: bool,
}

/// A corpse woken passively is not woken passively again within this time (each wake costs a pool ragdoll).
pub const PASSIVE_WAKE_COOLDOWN_MS: f64 = 60000.0;
static WOKEN: Mutex<Vec<(u32, f64)>> = Mutex::new(Vec::new());

/// True if `owner` was woken passively within PASSIVE_WAKE_COOLDOWN_MS.
pub fn recently_woken(owner: u32) -> bool {
    let now = clock();
    let mut w = lock(&WOKEN);
    w.retain(|&(_, t)| now - t < PASSIVE_WAKE_COOLDOWN_MS);
    w.iter().any(|&(o, _)| o == owner)
}

#[derive(Default)]
struct Protect {
    dragged: u32,
    retained: Vec<u32>,
}

static RESTORE: Mutex<Option<Restore>> = Mutex::new(None);
static PROTECT: Mutex<Protect> = Mutex::new(Protect { dragged: 0, retained: Vec::new() });

fn lock<T>(m: &'static Mutex<T>) -> std::sync::MutexGuard<'static, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// True if `[p, p+len)` is committed read-write memory (the only pages we write).
fn writable(p: u32, len: usize) -> bool {
    if !readable(p, len) {
        return false;
    }
    let mut mi: MemInfo = unsafe { core::mem::zeroed() };
    if unsafe { VirtualQuery(p as *const c_void, &mut mi, core::mem::size_of::<MemInfo>()) } == 0 {
        return false;
    }
    const WRITE_MASK: u32 = 0x04 | 0x08 | 0x40 | 0x80;
    mi.protect & WRITE_MASK != 0
}

unsafe fn wf32(p: u32, v: f32) -> bool {
    writable(p, 4) && {
        core::ptr::write_unaligned(p as *mut f32, v);
        true
    }
}

unsafe fn w32(p: u32, v: u32) -> bool {
    writable(p, 4) && {
        core::ptr::write_unaligned(p as *mut u32, v);
        true
    }
}

fn rf(p: u32) -> Option<f32> {
    unsafe { r32(p) }.map(f32::from_bits).filter(|v| v.is_finite())
}

// ───────────────────────── pure checks (unit-tested) ─────────────────────────

/// `e` is a slot of the static pose ring of `typ` (address arithmetic of FUN_0058f390).
pub fn entry_in_ring(e: u32, typ: u32) -> bool {
    if typ >= 2 {
        return false;
    }
    let first = RING_BASE + typ * RING_TYPE_STRIDE;
    e >= first && (e - first) % RING_ENTRY == 0 && (e - first) / RING_ENTRY < RING_SLOTS
}

/// Sanity of a captured pose: every bone finite, unit quaternion, near the root; the first bone (RB_HIP) closer.
pub fn pose_ok(pose: &[Bone]) -> Result<(), String> {
    if pose.len() < 2 || pose.len() > MAX_BONES as usize {
        return Err(format!("{} bones", pose.len()));
    }
    for (i, b) in pose.iter().enumerate() {
        if !b.t.iter().chain(b.q.iter()).all(|v| v.is_finite()) {
            return Err(format!("bone {i} not finite"));
        }
        let qn = b.q.iter().map(|v| v * v).sum::<f32>().sqrt();
        if (qn - 1.0).abs() > 0.05 {
            return Err(format!("bone {i} quaternion length {qn:.3}"));
        }
        let d = b.t.iter().map(|v| v * v).sum::<f32>().sqrt();
        if d > MAX_BONE_OFFSET {
            return Err(format!("bone {i} {d:.2} m from the root"));
        }
    }
    let d0 = pose[0].t.iter().map(|v| v * v).sum::<f32>().sqrt();
    if d0 > MAX_PELVIS_OFFSET {
        return Err(format!("pelvis {d0:.2} m from the root"));
    }
    Ok(())
}

/// World placement of a captured bone: the native activation's formula (FUN_00590db0: root translation + model-space
/// translation, model-space rotation; worldFromModel has identity rotation in both capture and activation).
pub fn place(root: [f32; 3], b: &Bone) -> ([f32; 3], [f32; 4]) {
    ([root[0] + b.t[0], root[1] + b.t[1], root[2] + b.t[2]], b.q)
}

type M3 = [[f32; 3]; 3]; // columns

fn q2m(q: [f32; 4]) -> M3 {
    let [x, y, z, w] = q;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y + z * w), 2.0 * (x * z - y * w)],
        [2.0 * (x * y - z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z + x * w)],
        [2.0 * (x * z + y * w), 2.0 * (y * z - x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

fn m2q(m: M3) -> [f32; 4] {
    // m[c][r]: column c, row r
    let (m00, m11, m22) = (m[0][0], m[1][1], m[2][2]);
    let tr = m00 + m11 + m22;
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        [(m[1][2] - m[2][1]) / s, (m[2][0] - m[0][2]) / s, (m[0][1] - m[1][0]) / s, 0.25 * s]
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        [0.25 * s, (m[1][0] + m[0][1]) / s, (m[2][0] + m[0][2]) / s, (m[1][2] - m[2][1]) / s]
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        [(m[1][0] + m[0][1]) / s, 0.25 * s, (m[2][1] + m[1][2]) / s, (m[2][0] - m[0][2]) / s]
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        [(m[2][0] + m[0][2]) / s, (m[2][1] + m[1][2]) / s, 0.25 * s, (m[0][1] - m[1][0]) / s]
    };
    let n = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    q.map(|v| v / n)
}

fn mul(a: M3, b: M3) -> M3 {
    let mut c = [[0.0f32; 3]; 3];
    for j in 0..3 {
        for i in 0..3 {
            c[j][i] = (0..3).map(|k| a[k][i] * b[j][k]).sum();
        }
    }
    c
}

fn transpose(a: M3) -> M3 {
    [[a[0][0], a[1][0], a[2][0]], [a[0][1], a[1][1], a[2][1]], [a[0][2], a[1][2], a[2][2]]]
}

fn apply(a: M3, v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| a[0][i] * v[0] + a[1][i] * v[1] + a[2][i] * v[2])
}

/// A rotation matrix read from game memory is used only if its columns are orthonormal.
fn orthonormal(a: M3) -> bool {
    let d = |u: [f32; 3], v: [f32; 3]| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    (0..3).all(|i| (d(a[i], a[i]) - 1.0).abs() < 0.02) && d(a[0], a[1]).abs() < 0.02 && d(a[0], a[2]).abs() < 0.02 && d(a[1], a[2]).abs() < 0.02
}

/// World transform of a body the captured pose lacks: its offset from `anchor` as the engine's activation placed them
/// (the ragdoll reference pose, joint-consistent), carried over onto the anchor's captured transform.
pub fn anchored(cap_anchor: ([f32; 3], [f32; 4]), act_anchor: ([f32; 3], M3), act_child: ([f32; 3], M3)) -> ([f32; 3], [f32; 4]) {
    let ra = q2m(cap_anchor.1);
    let rel = mul(ra, transpose(act_anchor.1));
    let d = [0, 1, 2].map(|k| act_child.0[k] - act_anchor.0[k]);
    let o = apply(rel, d);
    ([0, 1, 2].map(|k| cap_anchor.0[k] + o[k]), m2q(mul(rel, act_child.1)))
}

/// Bones to fall back on, nearest first, for a ragdoll bone the captured skeleton does not have (NPC ragdoll names
/// RB_<SIDE>_<PART>, PROVEN from the v2..v19 logs; the low-detail type has fewer bones).
pub fn anchor_chain(name: &str) -> Vec<String> {
    let side = ["LEFT_", "RIGHT_"].into_iter().find(|s| name.contains(s)).unwrap_or("");
    let part = |p: &str| format!("RB_{side}{p}");
    let core = |v: &mut Vec<String>| v.extend(["RB_TORSO".to_string(), "RB_HIP".to_string()]);
    let mut v = Vec::new();
    if name.ends_with("HAND") {
        v.extend([part("LOWER_ARM"), part("UPPER_ARM"), part("SHOULDER")]);
        core(&mut v);
    } else if name.ends_with("LOWER_ARM") {
        v.extend([part("UPPER_ARM"), part("SHOULDER")]);
        core(&mut v);
    } else if name.ends_with("UPPER_ARM") {
        v.push(part("SHOULDER"));
        core(&mut v);
    } else if name.ends_with("SHOULDER") || name.ends_with("HEAD") {
        core(&mut v);
    } else if name.ends_with("FOOT") {
        v.extend([part("LOWER_LEG"), part("UPPER_LEG"), "RB_HIP".to_string()]);
    } else if name.ends_with("LOWER_LEG") {
        v.extend([part("UPPER_LEG"), "RB_HIP".to_string()]);
    } else if name.ends_with("UPPER_LEG") || name.ends_with("TORSO") {
        v.push("RB_HIP".to_string());
    }
    v
}

/// How each body of the woken ragdoll (`dst` bone names) gets its transform from the captured pose (`src` names).
#[derive(Clone, Debug, PartialEq)]
pub enum Plan {
    Direct(usize),
    /// (anchor body index in dst, anchor bone index in src)
    Anchored(usize, usize),
    Keep,
}

pub fn plan(dst: &[String], src: &[String]) -> Vec<Plan> {
    let pos = |names: &[String], n: &str| names.iter().position(|x| x == n);
    dst.iter()
        .map(|n| match pos(src, n) {
            Some(j) => Plan::Direct(j),
            None => anchor_chain(n)
                .iter()
                .find_map(|a| Some(Plan::Anchored(pos(dst, a)?, pos(src, a)?)))
                .unwrap_or(Plan::Keep),
        })
        .collect()
}

/// Which owner gets which protection: the dragged one is never stealable, retained ones are capped below release.
pub fn protected_age(owner: u32, age: f32, dragged: u32, retained: &[u32]) -> Option<f32> {
    if owner != 0 && owner == dragged {
        return Some(DRAGGED_AGE);
    }
    if owner != 0 && retained.contains(&owner) && age > RETAIN_AGE_CAP {
        return Some(RETAIN_AGE_CAP);
    }
    None
}

/// Up to RETAIN_MAX nearest (distance, owner) within RETAIN_RADIUS, excluding the dragged one.
pub fn pick_retained(mut cands: Vec<(f32, u32)>, dragged: u32) -> Vec<u32> {
    cands.retain(|&(d, o)| o != dragged && d <= RETAIN_RADIUS);
    cands.sort_by(|a, b| a.0.total_cmp(&b.0));
    cands.into_iter().take(RETAIN_MAX).map(|c| c.1).collect()
}

// ───────────────────────── engine reads ─────────────────────────

/// The human's static pose entry if the engine's own validity rule holds (`[entry] == human`, FUN_004f37f0) and it is
/// a real ring slot: (entry, hkaPose*, type).
pub unsafe fn static_entry(owner: u32) -> Result<(u32, u32, u32), String> {
    let e = r32(owner + H_STATIC).ok_or("static pose pointer unreadable")?;
    if e == 0 {
        return Err("no static pose (never captured, or the capture conditions failed)".into());
    }
    if r32(e) != Some(owner) {
        return Err("static pose slot reused by a newer corpse (ring of 30 per type wrapped)".into());
    }
    let typ = r32(e + 8).ok_or("slot type unreadable")?;
    if !entry_in_ring(e, typ) {
        return Err(format!("0x{e:08x} is not a ring slot of type {typ}"));
    }
    let pose = r32(e + 4).filter(|&p| readable(p, 0x28)).ok_or("slot has no pose object")?;
    Ok((e, pose, typ))
}

/// Copy of an hkaPose's model-space transforms (`+0x10` array, `+0x14` size, `+0x1c` flags; flag 2 = model dirty).
pub unsafe fn read_pose(pose: u32) -> Result<Vec<Bone>, String> {
    let (arr, n) = (r32(pose + 0x10).ok_or("model array unreadable")?, r32(pose + 0x14).ok_or("size unreadable")?);
    let (flags, nf) = (r32(pose + 0x1c).ok_or("flags unreadable")?, r32(pose + 0x20).ok_or("flag count unreadable")?);
    if !(2..=MAX_BONES).contains(&n) || nf != n {
        return Err(format!("pose sizes {n}/{nf}"));
    }
    if !readable(arr, 48 * n as usize) || !readable(flags, 4 * n as usize) {
        return Err("pose arrays unreadable".into());
    }
    let mut out = Vec::with_capacity(n as usize);
    for i in 0..n {
        if core::ptr::read_unaligned((flags + 4 * i) as *const u32) & 2 != 0 {
            return Err(format!("bone {i} model space not in sync"));
        }
        let f = core::ptr::read_unaligned((arr + 48 * i) as *const [f32; 12]);
        out.push(Bone { t: [f[0], f[1], f[2]], q: [f[4], f[5], f[6], f[7]] });
    }
    pose_ok(&out)?;
    Ok(out)
}

// ───────────────────────── protection (A + B) ─────────────────────────

pub fn set_dragged(owner: u32) {
    lock(&PROTECT).dragged = owner;
}

/// Replace the retained set; returns true if it changed.
pub fn set_retained(owners: Vec<u32>) -> bool {
    let mut p = lock(&PROTECT);
    let mut a = owners.clone();
    let mut b = p.retained.clone();
    a.sort_unstable();
    b.sort_unstable();
    p.retained = owners;
    a != b
}

pub fn dragged() -> u32 {
    lock(&PROTECT).dragged
}

// ───────────────────────── restore (C) ─────────────────────────

/// Give a recycled corpse a ragdoll again the way an explosion does: the engine's go-ragdoll with `force`, here with
/// the arguments WSHumanStateRagdoll uses for its own restart (0.0, 1.0, zero vectors, no impulse). The new state
/// acquires from the pool by its own rules, arms and activates the object. A recycled corpse has no animation pose
/// (human+0x10ac is null, v18 log), so the activation places the bodies in the ragdoll's standing reference pose;
/// the acquire also retires the static pose (FUN_004c4160). So: the static pose is drawn again from the first update
/// of the new object, the bodies are moved to the captured pose right after the activation (by bone name, so either
/// ragdoll type works), and the static pose is handed over one update later. Preconditions checked by the caller:
/// identity (`human_ok`), state 3, no live ragdoll.
pub unsafe fn start(owner: u32, vt: u32, grab: bool) -> Result<String, String> {
    if lock(&RESTORE).as_ref().is_some_and(|r| matches!(r.stage, Stage::Requested | Stage::Placed { .. }) && !r.reported) {
        return Err("another restore is in progress".into());
    }
    let root = human_pos(owner).ok_or("corpse root unreadable")?;
    let entry = static_entry(owner);
    let (entry_addr, pose_obj) = entry.as_ref().map_or((0, 0), |&(e, p, _)| (e, p));
    let snap = entry.and_then(|(e, pose, typ)| {
        let names = r32(pose).map(|s| skeleton_bone_names(s)).unwrap_or_default();
        read_pose(pose).map(|b| (e, typ, b, names))
    });
    let snap_txt = match &snap {
        Ok((e, typ, b, names)) => format!(
            "captured pose: slot 0x{e:08x} type {typ}, {} bones [{}]",
            b.len(), if names.len() == b.len() { names.join(" ") } else { format!("{} names", names.len()) }
        ),
        Err(why) => format!("no captured pose ({why})"),
    };
    if !grab && snap.is_err() {
        // without a captured pose the activation would show the ragdoll reference (A/T) pose: leave it frozen
        lock(&WOKEN).push((owner, clock()));
        return Err(format!("{snap_txt}; not woken"));
    }
    let zero2 = V4([0.0; 4]);
    let zero = V4([0.0; 4]);
    type GoRagdoll = unsafe extern "thiscall" fn(
        u32, f32, f32, *const V4, *const V4, u8, *const V4, u8, u8, *const V4, u8, u8, u8, u8,
    ) -> u32;
    let go: GoRagdoll = core::mem::transmute(GO_RAGDOLL);
    let accepted = {
        let _t = super::perf::Timer::new("go-ragdoll (wake / restore)", super::perf::Kind::Note);
        go(owner, 0.0, 1.0, &zero2, &zero, 0, &zero, 1, 0, &zero, 0, 0, 0, 0) & 0xff
    };
    if accepted == 0 {
        return Err("the engine refused go-ragdoll".into());
    }
    if !grab {
        lock(&WOKEN).push((owner, clock()));
    }
    *lock(&RESTORE) = Some(Restore {
        owner,
        vt,
        snap: snap.ok().map(|(_, typ, b, names)| (typ, b, names)),
        entry: entry_addr,
        pose_obj,
        holding: false,
        state: r32(owner + 0x1238).unwrap_or(0),
        targets: Vec::new(),
        root,
        started: clock(),
        stage: Stage::Requested,
        reported: false,
        grab,
    });
    Ok(format!("go-ragdoll accepted; root ({:.2},{:.2},{:.2}); {snap_txt}", root[0], root[1], root[2]))
}

/// Before the original ragdoll update: protection ages. Returns true when this update may activate the ragdoll of
/// the corpse being restored (its own object, still inactive), so `after_update` can see the activation.
pub unsafe fn before_update(rd: u32) -> bool {
    let Some(owner) = r32(rd + RD_OWNER).filter(|&o| o != 0) else { return false };
    if let Some(age) = rf(rd + RD_AGE) {
        let p = lock(&PROTECT);
        if let Some(a) = protected_age(owner, age, p.dragged, &p.retained) {
            wf32(rd + RD_AGE, a);
        }
    }
    let mut g = lock(&RESTORE);
    let Some(r) = g.as_mut() else { return false };
    let watching = r.stage == Stage::Requested
        && r.owner == owner
        && r32(owner) == Some(r.vt)
        && r32(owner + H_RAGDOLL) == Some(rd)
        && r8(rd + RD_ACTIVE) == Some(0);
    if watching && r.entry != 0 && !r.holding {
        // the acquire cleared the slot's owner and human+0x10d4 (FUN_004c4160); the pose object is untouched. Put both
        // back so the renderer (FUN_00504100) keeps drawing the lying pose until the bodies are placed.
        if r32(r.entry + 4) == Some(r.pose_obj) && matches!(r32(r.entry), Some(0)) && matches!(r32(owner + H_STATIC), Some(0)) {
            r.holding = w32(r.entry, owner) && w32(owner + H_STATIC, r.entry);
        }
    }
    watching
}

/// Transform of body `i` of an activated ragdoll: position and rotation matrix (motion state transform at body+0xf0:
/// three rotation columns, then the translation at +0x120).
unsafe fn body_xf(bodies: u32, map: u32, i: u32) -> Option<([f32; 3], M3)> {
    let m = r32(map + 4 * i).filter(|&m| (m as i32) >= 0)?;
    let b = r32(bodies + 4 * m)?;
    let rot = [rvec(b + 0xf0)?, rvec(b + 0x100)?, rvec(b + 0x110)?];
    Some((rvec(b + 0x120)?, rot))
}

/// After the original ragdoll update. Activation frame: put every body at the captured pose (by bone name; bones the
/// captured skeleton lacks follow their anchor with the activation's own joint offsets). Next update: hand the
/// static pose over to the live ragdoll (clear the slot owner and human+0x10d4 as the acquire did).
pub unsafe fn after_update(rd: u32, watching: bool) {
    let mut g = lock(&RESTORE);
    let Some(r) = g.as_mut() else { return };
    let frame = FRAME.load(Ordering::Relaxed);
    if let Stage::Placed { rd: prd, frame: f } = r.stage {
        if prd == rd && f != frame {
            // pose check: where the placed bodies are now, one update after placing them
            let ri = r32(rd + 0xac).unwrap_or(0);
            let (map, bodies) = (r32(ri + 0x20).unwrap_or(0), r32(ri + 8).unwrap_or(0));
            let worst = r.targets.iter().filter_map(|&(i, t)| {
                let (p, _) = body_xf(bodies, map, i)?;
                Some(((p[0] - t[0]).powi(2) + (p[1] - t[1]).powi(2) + (p[2] - t[2]).powi(2)).sqrt())
            }).fold(None, |m: Option<f32>, d| Some(m.map_or(d, |m| m.max(d))));
            let sync = match worst {
                Some(d) if d <= SYNC_OK_M => format!("pose held: {} bodies within {d:.2} m of the captured pose", r.targets.len()),
                Some(d) => format!("POSE NOT HELD: a body is {d:.2} m from the captured pose"),
                None => "POSE NOT CHECKED: no body placed".to_string(),
            };
            let released = !r.holding || (r32(r.entry) == Some(r.owner) && w32(r.entry, 0) && w32(r.owner + H_STATIC, 0));
            log!(
                "[lifecycle] clock={:.0} restore 0x{:08x}: {sync}; {}",
                clock(), r.owner, if !r.holding { "static pose was NOT held over the wake" } else if released { "static pose handed over to the live ragdoll" } else { "static slot changed meanwhile, left alone" }
            );
            r.holding = false;
            r.stage = Stage::Done { rd };
        }
        return;
    }
    if !watching || r.stage != Stage::Requested || r8(rd + RD_ACTIVE) != Some(1) {
        return;
    }
    let rt = r32(rd + RD_TYPE).unwrap_or(u32::MAX);
    let ri = r32(rd + 0xac).unwrap_or(0);
    let (nb, map, bodies) = (r32(ri + 0xc).unwrap_or(0), r32(ri + 0x20).unwrap_or(0), r32(ri + 8).unwrap_or(0));
    let note = match &r.snap {
        None => "native activation pose (nothing captured)".to_string(),
        Some((typ, pose, src_names)) => {
            let dst_names = ragdoll_bone_names(rd);
            let plans: Vec<Plan> = if dst_names.len() == nb as usize && src_names.len() == pose.len() {
                plan(&dst_names, src_names)
            } else if *typ == rt && nb as usize <= pose.len() {
                (0..nb as usize).map(Plan::Direct).collect() // same skeleton: by index
            } else {
                Vec::new()
            };
            // read every activated transform before moving any body (anchors use the activation pose)
            let act: Vec<Option<([f32; 3], M3)>> = (0..nb).map(|i| body_xf(bodies, map, i)).collect();
            let set_body: unsafe extern "thiscall" fn(u32, u32, *const Xf, *const V4) = core::mem::transmute(SET_BODY);
            let (mut direct, mut anch, mut kept, mut moved) = (0, 0, 0, 0.0f32);
            for (i, p) in plans.iter().enumerate() {
                let target = match *p {
                    Plan::Direct(j) => Some(place(r.root, &pose[j])),
                    Plan::Anchored(a, j) => match (act.get(a).copied().flatten(), act[i]) {
                        (Some(aa), Some(ac)) if orthonormal(aa.1) && orthonormal(ac.1) => Some(anchored(place(r.root, &pose[j]), aa, ac)),
                        _ => None,
                    },
                    Plan::Keep => None,
                };
                let Some((pos, q)) = target else {
                    kept += 1;
                    continue;
                };
                // FUN_0058ea90 indexes the body list through this map without a negative check of its own
                if r32(map + 4 * i as u32).map_or(true, |m| (m as i32) < 0) {
                    kept += 1;
                    continue;
                }
                if let Some((was, _)) = act[i] {
                    moved = moved.max(((was[0] - pos[0]).powi(2) + (was[1] - pos[1]).powi(2) + (was[2] - pos[2]).powi(2)).sqrt());
                }
                set_body(rd, i as u32, &Xf { pos: [pos[0], pos[1], pos[2], 0.0], rot: q }, &V4([0.0; 4]));
                r.targets.push((i as u32, pos));
                if matches!(p, Plan::Direct(_)) { direct += 1 } else { anch += 1 }
            }
            format!(
                "captured type {typ} -> pool type {rt}: {direct} bodies placed directly, {anch} via their anchor, {kept} left (of {nb}); largest move {moved:.2} m; static pose {}",
                if r.holding { "held" } else { "NOT held" }
            )
        }
    };
    log!(
        "[lifecycle] clock={:.0} restore 0x{:08x}: ragdoll 0x{rd:08x} activated {:.0} ms after go-ragdoll; {note}",
        clock(), r.owner, clock() - r.started
    );
    r.stage = Stage::Placed { rd, frame };
}

/// A restore or wake is being served right now (no side effects; for the HUD prompt).
pub fn in_progress() -> bool {
    lock(&RESTORE).as_ref().is_some_and(|r| matches!(r.stage, Stage::Requested | Stage::Placed { .. }) && !r.reported)
}

pub enum Poll {
    Idle,
    Busy,
    Done(u32),
    Failed(u32, String),
}

/// From the frame tick: report the outcome once. A request the pool has not served within RESTORE_TIMEOUT_MS is
/// withdrawn: the new ragdoll state's acquire flag is set, as after its own successful acquire, so a ragdoll can no
/// longer arrive later unwatched (it would show the reference pose). The static pose was never retired (that happens
/// only at an acquire), so the corpse stays frozen as it lies and can be woken again.
pub unsafe fn poll() -> Poll {
    let mut g = lock(&RESTORE);
    let Some(r) = g.as_mut() else { return Poll::Idle };
    let age = clock() - r.started;
    match r.stage {
        Stage::Done { rd } => {
            let (owner, late, grab) = (r.owner, r.reported, r.grab);
            let live = r32(rd + RD_OWNER) == Some(owner) && r32(owner + H_RAGDOLL) == Some(rd);
            *g = None;
            match (live, late, grab) {
                (true, false, true) => Poll::Done(owner),
                (true, _, false) => {
                    log!("[lifecycle] clock={:.0} wake 0x{owner:08x}: ragdoll live ({age:.0} ms; pose check in the line above)", clock());
                    Poll::Idle
                }
                (true, true, true) => {
                    log!("[lifecycle] clock={:.0} restore 0x{owner:08x}: ragdoll arrived late ({age:.0} ms); press F7 to grab", clock());
                    Poll::Idle
                }
                (false, _, true) => Poll::Failed(owner, "ragdoll lost right after activation".into()),
                (false, _, false) => {
                    log!("[lifecycle] clock={:.0} wake 0x{owner:08x}: ragdoll lost right after activation", clock());
                    Poll::Idle
                }
            }
        }
        Stage::Requested if age > RESTORE_WATCH_MS => {
            log!("[lifecycle] clock={:.0} restore 0x{:08x}: acquired but never activated within {RESTORE_WATCH_MS} ms; stopped watching", clock(), r.owner);
            *g = None;
            Poll::Idle
        }
        Stage::Requested if age > RESTORE_TIMEOUT_MS => {
            let (owner, state, grab) = (r.owner, r.state, r.grab);
            let acquired = r32(owner + H_RAGDOLL).is_some_and(|rd| rd != 0 && r32(rd + RD_OWNER) == Some(owner));
            if acquired {
                return Poll::Busy; // a ragdoll is on its way to activation: keep watching it
            }
            let withdrawn = state != 0
                && r32(owner + 0x1238) == Some(state)
                && r8(state + 0xbd) == Some(0)
                && readable(state, 0x120)
                && {
                    let set: unsafe extern "thiscall" fn(u32, u8) = core::mem::transmute(STATE_SET_ACQUIRED);
                    set(state, 1);
                    true
                };
            *g = None;
            let why = format!(
                "no ragdoll from the pool within {RESTORE_TIMEOUT_MS} ms (all in use); {}",
                if withdrawn { "request withdrawn, the corpse stays frozen as it lies" } else { "request could not be withdrawn (state changed)" }
            );
            if grab {
                Poll::Failed(owner, why)
            } else {
                log!("[lifecycle] clock={:.0} wake 0x{owner:08x}: {why}", clock());
                Poll::Idle
            }
        }
        Stage::Requested | Stage::Placed { .. } if r.reported => Poll::Idle,
        Stage::Requested | Stage::Placed { .. } => Poll::Busy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bone(t: [f32; 3]) -> Bone {
        Bone { t, q: [0.0, 0.0, 0.0, 1.0] }
    }

    #[test]
    fn ring_slots_match_the_capture_arithmetic() {
        assert!(entry_in_ring(RING_BASE, 0));
        assert!(entry_in_ring(RING_BASE + 29 * 12, 0));
        assert!(!entry_in_ring(RING_BASE + 30 * 12, 0));
        assert!(entry_in_ring(RING_BASE + 0x16c + 5 * 12, 1));
        assert!(!entry_in_ring(RING_BASE + 0x16c + 5 * 12 + 4, 1));
        assert!(!entry_in_ring(RING_BASE + 0x16c, 0));
        assert!(!entry_in_ring(RING_BASE, 2));
        assert!(!entry_in_ring(RING_BASE - 12, 0));
    }

    #[test]
    fn pose_sanity_rejects_bad_captures() {
        let good: Vec<Bone> = (0..17).map(|i| bone([0.0, 0.1 + i as f32 * 0.05, 0.0])).collect();
        assert!(pose_ok(&good).is_ok());
        let mut far = good.clone();
        far[5].t = [10.0, 0.0, 0.0];
        assert!(pose_ok(&far).is_err());
        let mut nan = good.clone();
        nan[3].q[1] = f32::NAN;
        assert!(pose_ok(&nan).is_err());
        let mut skew = good.clone();
        skew[2].q = [0.0, 0.0, 0.0, 0.5];
        assert!(pose_ok(&skew).is_err());
        assert!(pose_ok(&good[..1]).is_err());
    }

    #[test]
    fn placement_is_root_plus_model_space() {
        let b = Bone { t: [0.1, 0.2, -0.3], q: [0.0, 0.7071, 0.0, 0.7071] };
        let (p, q) = place([10.0, 50.0, -5.0], &b);
        assert_eq!(p, [10.1, 50.2, -5.3]);
        assert_eq!(q, b.q); // rotation untouched: worldFromModel has identity rotation
    }

    #[test]
    fn dragged_is_never_stealable_and_retained_never_reaches_release() {
        // dragged: below FUN_00590940's -1.0 start of the oldest-search, whatever its age
        assert_eq!(protected_age(7, 100.0, 7, &[]), Some(DRAGGED_AGE));
        assert!(DRAGGED_AGE < -1.0);
        // retained: capped above the 5.0 s wake/theft threshold, below the 7.0 s release age
        assert_eq!(protected_age(8, 6.5, 7, &[8]), Some(RETAIN_AGE_CAP));
        assert!(RETAIN_AGE_CAP > 5.0 && RETAIN_AGE_CAP < 7.0);
        assert_eq!(protected_age(8, 3.0, 7, &[8]), None); // young: untouched
        assert_eq!(protected_age(9, 50.0, 7, &[8]), None); // not protected: native rules
        assert_eq!(protected_age(0, 50.0, 0, &[0]), None);
    }

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-4)
    }

    #[test]
    fn quaternion_matrix_round_trip() {
        for q in [[0.0, 0.0, 0.0, 1.0], [0.0, 0.7071068, 0.0, 0.7071068], [0.3, -0.5, 0.1, 0.8062258], [0.9, 0.1, -0.3, 0.3]] {
            let n = (q as [f32; 4]).iter().map(|v| v * v).sum::<f32>().sqrt();
            let q = q.map(|v: f32| v / n);
            let r = m2q(q2m(q));
            let same = (0..4).all(|k| (r[k] - q[k]).abs() < 1e-4) || (0..4).all(|k| (r[k] + q[k]).abs() < 1e-4);
            assert!(same, "{q:?} -> {r:?}");
            assert!(orthonormal(q2m(q)));
        }
        // 90 deg about +Y maps +X to -Z (right-handed)
        assert!(close(apply(q2m([0.0, 0.7071068, 0.0, 0.7071068]), [1.0, 0.0, 0.0]), [0.0, 0.0, -1.0]));
    }

    #[test]
    fn missing_bones_follow_their_anchor_rigidly() {
        // activation: anchor at the origin unrotated, child 0.3 m along -Y (a hanging forearm)
        let ident: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let act_a = ([0.0, 1.2, 0.0], ident);
        let act_c = ([0.0, 0.9, 0.0], ident);
        // captured: anchor lying on the ground, rotated 90 deg about Z (its -Y now points along +X)
        let qz = [0.0, 0.0, 0.7071068, 0.7071068];
        let (p, q) = anchored(([5.0, 0.1, 2.0], qz), act_a, act_c);
        assert!(close(p, [5.3, 0.1, 2.0]), "{p:?}");
        assert!(close(apply(q2m(q), [0.0, 1.0, 0.0]), apply(q2m(qz), [0.0, 1.0, 0.0])), "same orientation as the anchor");
    }

    #[test]
    fn bone_plan_maps_by_name_and_anchors_the_rest() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let dst = s(&["RB_HIP", "RB_TORSO", "RB_HEAD", "RB_LEFT_UPPER_ARM", "RB_LEFT_LOWER_ARM", "RB_LEFT_HAND", "RB_RIGHT_FOOT", "RB_ODD"]);
        let src = s(&["RB_TORSO", "RB_HIP", "RB_HEAD", "RB_LEFT_UPPER_ARM", "RB_RIGHT_UPPER_LEG"]);
        let p = plan(&dst, &src);
        assert_eq!(p[0], Plan::Direct(1));
        assert_eq!(p[1], Plan::Direct(0));
        assert_eq!(p[3], Plan::Direct(3));
        assert_eq!(p[4], Plan::Anchored(3, 3), "lower arm follows the upper arm");
        assert_eq!(p[5], Plan::Anchored(3, 3), "hand: lower arm missing too, so the upper arm");
        assert_eq!(p[6], Plan::Anchored(0, 1), "foot: no leg bones on the dst side -> the hip");
        assert_eq!(p[7], Plan::Keep);
        assert_eq!(anchor_chain("RB_RIGHT_HAND")[0], "RB_RIGHT_LOWER_ARM");
    }

    #[test]
    fn retention_is_bounded_nearest_first() {
        let c = vec![(12.0, 1), (3.0, 2), (20.0, 3), (1.0, 4), (5.0, 5), (2.0, 6)];
        let r = pick_retained(c, 4);
        assert_eq!(r, vec![6, 2, 5]); // 4 is dragged, 3 beyond the radius, capped at RETAIN_MAX
        assert!(r.len() <= RETAIN_MAX);
    }
}
