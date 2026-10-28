# Body-drag stutter and lag investigation

> Status (2026-10-27, later): addressed in body-drag v23 (saboteur-camera-mod/bodydrag/versions/v23_perf/README.md):
> asynchronous logger (no lock retry sleeps, no file I/O on the game thread), body-setup research hook opt-in,
> carry diagnostics built only when logged, passive wake throttled, and a `[perf]` self-measurement for the next
> session. Not yet measured in game.

Audit date: 2026-10-27. User reports whole-game/camera hitches and may also be seeing character/body jerks. These symptoms have not yet been isolated. This pass inspected source and historical logs and ran an isolated 32-bit Windows benchmark. It did not launch the game, inject a profiler, deploy an ASI, or change production mod source.

## Result and evidence boundary

There is a concrete hitch-capable blocking path in the logger. Deliberately exercising its retry schedule in a separate process took 65-76 ms in the recorded benchmark run. The mod also performs avoidable per-frame diagnostic work and repeatedly submits animation requests during turning. None of the existing logs measures the duration of these operations, so this audit cannot attribute the user's actual hitches to one operation yet.

The game was not running during this pass. Installed ASI SHA-256 is `0462820c8855f686653ef10003a09a3aa7c6a89416f3bfe7ade03f172dbda04e`, matching `saboteur-camera-mod/bodydrag/versions/v20_hold_static_wake/sab_asi.asi`. A v21 snapshot exists, but its ASI hash differs and it was not installed at inspection. The installed snapshot's `lib_v20.rs` hash is `839825314040b19a4346ab7cb1c16e54b0047ab08f2f216085ab5041103f3afe`; current `src/lib.rs` matches it. Its carry source also matches current `src/bodydrag.rs`.

References below: **LIB20** = sibling repository `saboteur-camera-mod/bodydrag/versions/v20_hold_static_wake/lib_v20.rs`; **B20** = `bodydrag_v20.rs` in the same folder; **L20** = `lifecycle_v20.rs`. Historical gameplay evidence is `C:/Saboteur/The Saboteur/sab_devlog.txt`, latest attach at line 33988. That session predates v20; it establishes historical behavior, not measured v20 performance.

## 1. Confirmed blocking design: the logger can sleep on the game thread

PROVEN-CODE, LIB20:88-105: `logf` tries a shared file mutex up to 50 times, sleeping for 1 ms whenever it is busy. Carry diagnostics, animation queue/execution logs, and body-setup logs reach this sink from engine hooks. The mutex is also available to the console thread.

The comment's approximately 50 ms is not a wall-clock deadline. Fifty sleep requests can take longer because scheduling adds delay. A successful lock then formats and writes the record synchronously to a plain `std::fs::File`. The retry limit does not bound the later write duration.

MEASURED-SYNTHETIC: the standalone probe deliberately holds an unrelated test mutex while executing the same retry schedule. Three trials in the final recorded run took **75.880, 76.224 and 64.539 ms**, each making 50 sleeps. At a 60 FPS target, this is roughly four frames' worth of time. An earlier run also measured 75-76 ms.

LIMIT: this is a reproducible mechanism, not evidence that the game's log mutex actually contended during a user hitch. Normal console polling does not log every 100 ms. Thread IDs, contention counters, wait durations and write durations are absent from the game log. Do not call this the confirmed runtime cause without those measurements.

## 2. Ordinary file logging is synchronous, but measured average cost is much smaller

PROVEN-CODE: LOG is a `Mutex<File>`, not a buffered writer. The sample carry record's formatting made **32 calls to a counting writer for 219 bytes**. This demonstrates fragmented formatting writes; it does not itself count kernel calls in the game.

MEASURED-LOCAL: a 32-bit process wrote 1,000 representative records to new files under the workspace's ignored `tools/sab_asi/target` directory:

| Local benchmark | Median per record | p95 per record | Maximum per record | Total for 1,000 |
|---|---:|---:|---:|---:|
| Plain File, flush call per record | 64.9 us | 73.4 us | 493.2 us | 67.235 ms |
| BufWriter, flush once after batch | 0.6 us | 0.7 us | 74.2 us | 1.031 ms |

Both files have SHA-256 `8f9723e17a273f03ac5c978f936221ab2b6ef6a8e3943c6bb326a1b5de381105`. No `sync_all`/fsync was performed; the source's `flush()` must not be described as forcing durable disk storage for every line. Buffered results describe CPU submission/buffering and the final flush, not durable storage throughput.

The historical latest session has 4,097 lines over approximately 619 seconds of logged game clock: 2,861 `[bodydrag]`, 341 `[lifecycle]`, 400 `[bodysetup]`, and 419 bodydrag console-run lines. Its average is about 6.6 total lines per game-clock second. This does not establish overwhelming sustained disk traffic, and local normal write times are much too small to explain a 75 ms hitch by themselves. Bursts, scheduling, lock contention, and storage outliers require in-game timing.

## 3. Confirmed wasted frame work: diagnostic throttling happens after formatting

PROVEN-CODE, B20:901-1000,1005-1041,1293-1305:

1. Every carry update resolves physics state and runs the controller.
2. It also computes diagnostic-only angles/distances and formats `head_txt`, `geo`, the full underarm message, and a wrapping message.
3. Only after `pull` returns does `step` check whether the 250 ms diagnostic interval has elapsed.

Thus a 60 FPS carry builds detailed strings about 60 times per second while writing roughly four regular diagnostic records per second. The allocations and floating-point formatting are avoidable on non-log frames. The fallback also formats a diagnostic string every frame.

LIMIT: this is proven unnecessary work, not a measured major frame stall. Moving only the final `log!` behind a timer would not eliminate work already performed inside `underarm` and `pull`.

## 4. Periodic animation submission can produce both work spikes and motion discontinuities

PROVEN-CODE, B20:1048-1050,1190-1216: turning clips are re-requested near each loop end because the engine applies their root yaw only during the first play-through. Each request formats/queues Lua, then the `lua_pcall` hook drains the queue and `run_chunk` compiles and executes it on the engine thread (LIB20:329-338,351-371). Drain has no per-frame time budget.

OBSERVED-LOG: the latest historical session queues **283 drag animation requests** and executes **419 total bodydrag Lua chunks**, including voice/face/holster work. Consecutive identical hard-turn requeues occur 33 times, with recorded intervals of 625-690 ms; four consecutive identical pivot requeues occur at 611-641 ms. Some one-second game-clock buckets contain four animation requests.

These are real request restarts, not merely an uninterrupted animation loop. They can explain character motion discontinuities and provide event markers for CPU-spike investigation. The existing log shows no unexpected Animate-request changes in this session, so such changes are not supported as its recurring trigger.

UNKNOWN: duration of native `Actor.PlayAnimation`, request allocation, blending, and any asset loads. Request-to-active delay is not a measurement of CPU time spent compiling Lua or starting the animation. Do not infer a global frame stall solely from a request restart.

## 5. Physics/contact load and controller fighting remain plausible engine-side costs

PROVEN-CODE, B20:768-798,943-981,1011-1014:

- Each shoulder velocity write calls the engine's activation function; separation and head-clearance writes may add more activation calls.
- The controller overwrites shoulder velocities and torso angular velocity, while the linked ragdoll also responds to Havok contacts and joint constraints.
- Separation runs after underarm control and changes velocities again for bodies overlapping Sean's exclusion cylinder.
- Dragged ragdoll age is held below pool-recycling thresholds, and up to three nearby ragdolls are retained (L20:31-38).

This keeps the selected carry under ongoing control and permits competing goals, especially around walls, stairs, the player, or reversed poses. See the companion [carry audit](bodydrag-carry-blindspots-2026-10-27.md).

LIMITS: retaining a ragdoll's age does not by itself prove that all retained bodies are awake or expensive every frame. Calling activate on an already active body is not evidence of a costly island wake each time. Neither Havok solver duration nor contact/island counts were measured. Body jitter is not evidence of global CPU overload. This needs profiling separately from logger waits.

## 6. Memory queries and growing lists: overhead exists, but no demonstrated large stall

PROVEN-CODE, B20:308-336: every guarded `r32`/`rvec` read issues `VirtualQuery`; there is no page-region cache. The carry path re-resolves bone maps and motion pointers, while the global ragdoll hook also validates/registers other bodies. Separation scans up to 32 bodies.

MEASURED-LOCAL, 32-bit process, 1,000 batches per size, repeated queries on valid local memory:

| Queries per synthetic batch | Median | p95 | Maximum |
|---|---:|---:|---:|
| 128 | 67.7 us | 70.5 us | 104.5 us |
| 256 | 139.5 us | 147.6 us | 217.6 us |
| 512 | 281.5 us | 293.5 us | 395.4 us |

These are hot local-memory measurements, not a counted in-game query workload. They argue against casually attributing multi-frame hitches to a few hundred memory queries, while leaving actual aggregate cost and outliers unknown. Removing memory validity checks indiscriminately is not a justified fix.

LIB20:159-165 keeps body-setup de-duplication in growing `Vec`s and searches them linearly on a frequently called hook. Synthetic worst-case missing-key lookups took about 18.8 ns at length 400, 187 ns at 4,000, and 1.86 us at 40,000. The latest session logged 400 new body-setup entries. Growth is a scalability concern, but these results do not support it as the present dominant hitch source.

The main carry trail is bounded to 90 entries, the known-body registry to 96, and voice history is bounded. No unbounded per-frame carry-history leak was established.

## Findings that prevent misleading diagnoses

- `after_update`'s comment says it pulls the body, but its current implementation only records the body. The actual pull is in the camera-tick interaction path. This audit found no evidence that the held body is pulled once for every corpse update.
- The console's 100 ms key/file polling sleeps occur on its worker thread (LIB20:402-444), not as a 100 ms sleep in the frame callback. File-change handling's 150 ms sleep is also on that worker. The logger is the path that can transfer waiting onto an engine thread.
- Voice configuration is read at the start of a grab, not every frame (B20:1556-1567,1587). Default voice cues are intermittent. They are lower-priority candidates for continuous drag-only stutter, although native audio/FaceFX event cost remains unmeasured.
- The game clock is summed camera-tick `dt`, not an independent CPU timer (B20:1375-1376). The 250 ms diagnostic cadence is not a frametime trace. Existing camera CSVs predate the latest session and lack carry/control-stage markers.
- A low average log rate does not rule out rare blocking; a synthetic 76 ms lock wait does not prove that the game's lock ever contended.

## Next measurement that would establish the actual cause

Use a diagnostic build with preallocated timing counters/ring storage. Read high-resolution wall time at hook entry/exit and record elapsed time for the frame's carry step, retention scan, animation drain/native request, logger lock wait/write, and original ragdoll updates. Record final commands separately from actual physics response. Track active ragdoll count, pool type, overlap pushes, diagnostic writes, and requeue markers. Export aggregates or buffered records outside the hot callback; per-event synchronous logging would contaminate the measurement.

Compare the same scene in this order:

| Condition | Separates |
|---|---|
| Mod enabled, no carry; hold still; straight drag | Baseline hook/retention overhead versus active attachment |
| Hard turns versus straight movement | Request restarts and turning/contact costs |
| Diagnostic logging/formatting off, controller unchanged | Logger/formatting overhead versus engine physics |
| Nearby retention on/off with the same target | Extra world-physics workload |
| Type 0 versus type 1, flat ground versus obstacles | Controller route and solver/contact sensitivity |
| Video plus wall-time frame capture | Whole-game stalls versus body-only motion discontinuities |

No such diagnostic build was installed by this audit. The current evidence establishes hazards and priorities, not the user's final runtime culprit.

## Reproduction

Probe source: [`tools/sab_probe/src/bin/bodydrag_perf_audit.rs`](../tools/sab_probe/src/bin/bodydrag_perf_audit.rs). It uses only Rust std and Windows `VirtualQuery`. Its mutex test uses a synthetic holder thread, and it never opens the game process.

From the repository root:

```powershell
rustc --edition=2021 --target i686-pc-windows-msvc -O tools/sab_probe/src/bin/bodydrag_perf_audit.rs -o tools/sab_asi/target/bodydrag_perf_audit.exe
tools/sab_asi/target/bodydrag_perf_audit.exe
```

Passing an existing output directory as the first argument additionally creates two new benchmark log files there and refuses to overwrite existing files. Use a new output directory for each such run. The recorded file benchmark used `tools/sab_asi/target`; both files were hash-checked for equal output. The baseline emits 1,000 lines per mode, with final buffered flush included in total time.

All numeric results above are environment-specific. No FPS improvement or confirmed in-game root cause is claimed from the isolated benchmark.
