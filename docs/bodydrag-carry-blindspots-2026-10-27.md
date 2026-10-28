# Body-drag carry placement: faults and blind spots

Audit date: 2026-10-27. Scope: source review, existing gameplay logs, authored animation metadata, and the existing native pool decompilation. No gameplay launch, injection, source patch, rebuild, or deployment was performed by this audit.

## Finding

The evidence does not support explaining inconsistent carrying solely as a corpse being uneven on the ground. The mod selects materially different carry controllers according to the acquired ragdoll's available bones. One controller reproduces the old uncontrolled single-point hold. The other uses approximate contact targets and has incomplete orientation and convergence checks. Ground contacts can contribute, but they have not been isolated as the cause.

Labels: PROVEN-CODE = explicit source behavior; OBSERVED-LOG = recorded diagnostics, not independent visual observation; INFERRED = consequence requiring runtime confirmation.

## Artifact boundary

- Installed `C:/Saboteur/The Saboteur/scripts/sab_asi.asi` SHA-256: `0462820c8855f686653ef10003a09a3aa7c6a89416f3bfe7ade03f172dbda04e`, matching `saboteur-camera-mod/bodydrag/versions/v20_hold_static_wake/sab_asi.asi`.
- Installed DLC animation pack SHA-256: `44d290cc676cf481c256e8e3a31df9d4d301a8b3aa0a3d111927a7d7afe818cd`, matching that snapshot's `Animations.pack.v20`.
- Live `tools/sab_asi/src/bodydrag.rs` and `bodydrag_v20.rs` SHA-256 both: `1f73ad2245687d02c336c93bfeabb17bbe886aa7e9ca9628aad1ebddab4f708a`.
- Existing log's latest attach starts at `sab_devlog.txt:33988`, from the preceding v19 run. There is no new v20 runtime session in the inspected log. The text from `unsafe fn underarm` through the end of `pull` is byte-identical between v19 and v20 source snapshots, so these carry faults remain applicable to the installed controller. That does not prove v20's complete runtime behavior.
- Source references below use the stable `saboteur-camera-mod/bodydrag/versions/v20_hold_static_wake/bodydrag_v20.rs`, abbreviated **B20**. The wake source is `lifecycle_v20.rs`, abbreviated **L20**. These are sibling-repository paths, not files under this repository.

## 1. P1: the fallback restores the old carry defect

PROVEN-CODE: B20:902 requires both shoulder bodies before underarm control can run. B20:1011-1027 otherwise uses a single torso pull, or a pelvis rope. The fallback torso target is 0.60 m along the player-to-pelvis direction and 1.15 m above Sean's root at full ramp. It has no torso orientation control, no two-point shoulder alignment, no Sean-velocity feed-forward, and no shoulder gravity/integral compensation. The underarm target instead uses Sean's heading and two shoulder targets at 1.22 m high, 0.32 m forward, and +/-0.22 m lateral.

OBSERVED-LOG: latest session has 33 actual `interaction started` records, excluding holster-only requests:

| Pool type | Interactions | Diagnostic/controller route |
|---|---:|---|
| 0 | 18 | All emit underarm diagnostics |
| 1 | 15 | All emit torso-fallback diagnostics |

The log proves the routing difference; it does not individually identify which shoulder lookup/read failed on type 1. There is no explicit reason code for the downgrade. Do not claim the exact missing bone name from this log alone.

Mature sample filter: nonqueued active state-2 Idle/Walk/Hard/Turn diagnostics, at least 3 seconds after interaction start; underarm additionally requires `ramp 1.00`.

- Type 1: 156 samples from 15 interactions. Median torso-target miss 0.38 m; 151 exceed 0.25 m; 114 exceed 0.35 m; maximum 0.51 m.
- Type 0: 154 samples from 14 interactions. Either shoulder-target miss exceeds 0.25 m in 2 samples; maximum 0.27 m. Other type-0 interactions lack mature sampled carry time under this filter.
- These are different error definitions, not a direct comparison of actual hand contact.

Repeatable example: tracked body `0x4f668c90` uses fallback in generations 53/55/57/61. Mean mature misses are approximately 0.372/0.347/0.393/0.370 m. Log lines 37425-37437 show Sean moving at 1.65-1.69 m/s with torso-target miss 0.36-0.38 m and torso height about 0.93 m. This is an evolving movement sequence, not the ambiguous frozen tail of the session.

INFERRED-MATH: with fallback `v = gain * position_error` and gain 6/s (B20:756-763,1029), movement at 1.67 m/s needs approximately `1.67 / 6 = 0.278 m` tracking error merely to command that velocity. Gravity and contact loads add further effects. The fallback structurally needs a lag that the underarm controller attempts to compensate. Levelling the initial corpse cannot remove this controller property.

## 2. P1: acquisition history can change the carry controller

OBSERVED-LOG: tracked body address `0x4f67c6a0` starts as type 1 in gen 7 at line 34652, emitting torso fallback at 34684. It later starts as type 0 in gen 17 at 35137, emitting underarm control at 35188-35189. Similar address/type changes occur for `0x4f62c970` (35499 -> 37127) and `0x4f5f5a10` (36439 -> 38030). The registry accepts these as tracked bodies; allocator reuse cannot be conclusively excluded from pointer identity alone.

PROVEN-NATIVE-STATIC: the existing decompilation of `FUN_00590940` tries free type-0 slots for eligible nearby humans, then falls back to the type-1 pool; it does not guarantee a type-0 acquisition. Existing pool notes derive three NPC type-0 slots plus Sean's reserved slot, and eleven type-1 slots. See `saboteur-camera-mod/bodydrag/lifecycle/LIFECYCLE_NOTES.md`, Objects/Transitions, and the saved decomp at `output/_ghidra_saboteur/saboteur_all_functions_decomp.txt:227839`.

INFERRED: retaining up to three nearby ragdolls (L20:37) can occupy all three NPC type-0 slots, making subsequent acquisitions more likely to use type 1. Retention does not reserve capacity for the selected carry target. The v20 wake fix maps poses across pool types; it does not make their carry capabilities equivalent.

This is a concrete recurrence mechanism: the success of a previous grab does not establish which controller a later acquisition will use.

## 3. P1: small "hand miss" does not establish a correct carry pose

PROVEN-CODE: B20:913-918 constructs proxy targets from Sean's root and heading. B20:997-999 reports distances from ragdoll shoulder origins to those proxies as `R-hand miss` and `L-hand miss`. Neither measurement reads Sean's animated hand positions. When the corpse is reversed, `lateral_weight` collapses both targets to the same midpoint (B20:879-918), so a small miss can coexist with the wrong shoulder orientation.

OBSERVED-LOG: during genuinely moving gen 43, line 36974 reports hand misses 0.21/0.07 m but shoulder-line error 113 degrees and `lateral 0.00`. Line 36975 still shows 105 degrees with misses 0.14/0.09 m. At line 36984 the error remains 88 degrees. It recovers to 23 degrees at 36985 after the turn direction changes. These samples occur many seconds into an established carry, not only during initial pickup.

Consequences: successful grabbing and acceptable proxy distances do not establish facing, arm separation, contact, or convergence during turns. The log needs to preserve separate contact and posture acceptance conditions.

Also, `spin` is calculated before the 3 rad/s command clamp (B20:961-966). A logged 4.1 rad/s is the unclamped requested magnitude, not measured actual spin or the final written command. Likewise `|v|` is constructed shoulder velocity before `separate` subsequently modifies velocities (B20:999,1012).

## 4. P2: clip changes move the hands without matching the hold target

PROVEN-CODE: underarm hold targets remain constant across Idle/Walk/Turn, apart from heading, the pickup height ramp, and release offsets. Pickup uses a linear height ramp from interaction creation (B20:908), not the actual per-frame hand trajectory. Clip activation can arrive later (B20:1112-1123). Sean's velocity feed-forward is averaged over approximately 100 ms (B20:1073-1077), and there is no explicit rotational `omega x offset` feed-forward for targets moving around him during turns.

Authored v17 metadata (`animpack/out/v17/*.hkobj.json`, `hands_model`, generated by `bake.py:122`) shows hand-midpoint height/forward ranges:

| Clip | Height, m | Forward, m |
|---|---:|---:|
| Idle | 1.030-1.070 | 0.364-0.366 |
| Walk | 1.132-1.152 | 0.207-0.224 |
| In | 0.182-1.140 | 0.343-0.650 |

These are authored FK metadata, not measurements of live blended playback. The forward midpoint changes by about 0.14 m between idle and walk while the hold target stays at 0.32 m. The intended underarm grip deliberately offsets shoulders above the hands, so shoulder-target height is not itself an error. What is unverified is the contact offset throughout each clip and blend, including lateral hand positions, not whether shoulders exactly coincide with hands.

## 5. P2: orientation correction still has untested geometry

PROVEN-CODE: the controlled `spine_now` is shoulder midpoint minus the torso body's origin (B20:950-958). The donor's 72-degree target was described/measured from pelvis to upper-arm midpoint; `animpack/release_curve.py:49-53` also derives its spine from victim pelvis to upper-arm midpoint. These are different geometric axes. No runtime calibration establishes that the torso-to-shoulder vector should use that same angle.

PROVEN-MATH: the pitch correction is `cross(spine_now, spine_want)`. Exactly opposite vectors produce zero correction. If the shoulder line already matches Sean's right, the roll term is also zero. At yaw 0, `spine_want = (0, 0.951057, -0.309017)` and `spine_now = -spine_want` therefore request zero torso rotation despite a 180-degree pitch error. The shoulder pulls/constraints may still disturb that state; this is a blind spot in the angular term, not proof that the entire ragdoll must remain stuck.

The reversed-roll test (B20:1638) covers one axis with an idealized rotation update. The pitch test (1687) uses a favorable starting vector. The sag test (1701) is a one-dimensional artificial constant-load model. None exercises both controllers, contact with Sean/terrain, coupled joints, opposing pose goals, or sustained turning in Havok.

## 6. P2: conflicting writes and degraded state are accepted without a pose-quality gate

- PROVEN-CODE: `separate` pushes every body inside Sean's cylindrical exclusion region, after underarm control has chosen velocities (B20:768-785,1012). Head clearance separately pushes/lifts the head (979-981). Torso spin and shoulder pulling act on linked bodies. This architecture permits competing commands; the amount of resulting visual error requires contact/physics-step evidence.
- PROVEN-CODE: if right-shoulder velocity succeeds and left fails, the short-circuit at B20:944 returns `None` after a partial write, then the fallback runs. No dedicated downgrade reason distinguishes this from absent shoulder data.
- PROVEN-CODE: `still_valid` follows a replacement ragdoll (474-496), but `pull` continues using the previously captured `Grip` indices (1005-1011,1296). There is no rebind of grip indices on pool/skeleton replacement. This is a latent path; this audit does not establish that replacement occurred during a protected hold.
- PROVEN-CODE: grab eligibility checks distance and pelvis physics (645-662), not that two-shoulder control is available or that the desired pose can be achieved. There is no convergence failure condition based on sustained posture/contact error. The hard range escape is 6 m (1007-1009).
- PROVEN-CODE: animation ownership can follow an unexpected Animate request instead of verifying the intended clip (1127-1131), and activation timeout continues on a timer (1121-1123). Clip/body synchronization is therefore not guaranteed on these paths.
- INFERRED: integral clamping limits bias magnitude, but it does not stop integration when output velocity saturates, contact blocks movement, or geometry is stale (923-938). Gen 65's repeated identical pose with growing integral is ambiguous because the log contains no physics-step/pause marker; do not present it as proof of a physical equilibrium or a pause bug.

## Why this keeps recurring

1. Fixes to the two-shoulder controller do not affect the single-torso fallback, which was used by 15 of 33 recorded interactions.
2. Pool reacquisition can change the branch on a later grab; nearby-body retention can affect capacity.
3. Root-level targets and a hand-labelled proxy metric obscure actual animated contact and clip changes.
4. Local roll/sag tests do not cover full coupled pose recovery or type-1 behavior.
5. There is no acceptance gate for a stable, correctly oriented carry. Allocation, movement, and small proxy error can all pass while the visible pose is wrong.

The earlier `FINDINGS.md` v8 section already attributes floppy posture to a single-body grip with no orientation control. The current fallback retains that mechanism. Ground-level tuning cannot close that branch.

## Focused verification still required

| Check | Purpose / pass evidence |
|---|---|
| Same model and initial transform, type 0 versus type 1 | Establish whether both routes provide the same visible carry; log resolved bones, controller and downgrade reason. |
| Repeat with 0, 1, 2, 3 nearby retained type-0 ragdolls | Establish pool-capacity influence on controller selection and repeated grabbing. |
| Flat-ground face-up/down, both sides, reversed, feet toward Sean | Separate initial orientation from terrain. Require bounded posture recovery before declaring pickup complete. |
| Idle -> walk -> hard turns -> stop | Measure live hand/contact frames and corpse pose across blends; log final commands and actual next-step response. |
| Contact/separation isolation in a diagnostic build | Distinguish torso/shoulder control, head-clearance and separation forces. No such build was deployed here. |
| Slopes/stairs after flat-ground controls pass | Quantify the contribution of ground/contact geometry without assuming it is the sole cause. |
| Both pool types, missing shoulder/write failure, mid-hold replacement | Verify explicit failure/degradation handling, grip rebinding and pose acceptance. |

Method: read stable v19/v20 sources; hash installed artifacts; compare the exact underarm/pull text; segment the existing log at the last attach; associate diagnostics with `gen` and `interaction started` type; filter mature active samples as specified above; inspect moving sequences directly; inspect `hands_model` arrays without treating them as runtime hand measurements; check existing native allocation decompilation. Sample maxima describe logged samples only, not unrecorded frame peaks.
