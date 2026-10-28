# sab_asi — in-process engine SDK for The Saboteur

A 32-bit DLL you inject into `Saboteur.exe`. It gives two live seams into the engine, both running on
the game's own threads at full speed — **no debugger, no breakpoints, no process freeze**, so it can't
trigger the "resume after a full-process pause" crash that a debugger read does.

1. **Dynamic hooks** (MinHook detours) that read engine state and append it to `sab_devlog.txt` next to
   the exe. Shipped hook: the human body-setup at `0x518490`, logging `[ecx+0x60]` (the combined-LOD
   model handle) plus the neighbouring blueprint / LOD-slot fields.
2. A **`Dev.*` Lua binding surface**: our own C functions registered into the live `lua_State` by
   piggy-backing on the engine's own table-publish routine (`0x6f8a90`). Callable from the game console
   or any script. Shipped: `Dev.Mark()`, `Dev.DumpNextBodySetup()`.

All engine addresses (retail GOG `Saboteur.exe`, imagebase `0x400000`, no ASLR) live in one place:
[`src/lib.rs` → `mod addrs`](src/lib.rs). A patched exe = update that module only.

## Build

```
cargo build --release           # target i686-pc-windows-msvc is pinned in .cargo/config.toml
```
One-time: `rustup target add i686-pc-windows-msvc`. Output: `target/i686-pc-windows-msvc/release/sab_asi.dll`.

## Inject

**Ultimate ASI Loader** (recommended):
1. Drop Ultimate ASI Loader in the game folder as `dinput8.dll` (the game loads `dinput8` from its own
   dir before SysWOW64 — confirmed; it is not a KnownDLL).
2. Copy `sab_asi.dll` → the game folder as **`sab_asi.asi`**.
3. Launch. The loader loads every `*.asi` at startup — early enough to hook the per-level VM publish.

**DXwrapper** works too (proxy `d3d9.dll`); point its plugin/ASI loading at `sab_asi.asi`.

## Use

- **Passive:** launch, reach gameplay, **swap a disguise** (rebuilds the player body). Watch
  `sab_devlog.txt` for `[bodysetup] this=… +0x60=…`. Do it somewhere quiet so the hit is unambiguously
  the player.
- **Targeted:** open the game console and call `Dev.DumpNextBodySetup()`, then swap the disguise — the
  next body-setup logs the full struct window (`+0x60`, `+0x78`, `+0xa4/a8/ac`) as `[bodysetup*]`.

## Lua console

No overlay: code comes from files next to the exe, output goes to `sab_devlog.txt`.

- **F9** (game window focused) runs `sab_run.lua`.
- **Saving `sab_cmd.lua`** runs it (polled every 100 ms; the version present at startup is not run),
  so an outside editor or script can drive the game.
- `print(...)` inside a chunk goes to the log (a one-line prelude rebinds it to `Dev.Log`, so error
  line numbers are unchanged); return values are logged as `[console] => …`; errors as `[console] error`.

Lua is single-threaded, so chunks are queued and run at the top of the engine's next `lua_pcall`
(`0x408310`, hooked), on the `L` it was given: `luaL_loadstring` (`0x15fe1a0`) + the original
`lua_pcall(L,0,LUA_MULTRET,0)`, then `lua_settop` (`0x4011e0`) back to the entry height. Same seam as
saboteur-team/saboteur-lua-hook (`loadstring` + `pcall`), whose addresses match this exe
(`docs/community_tooling.md`). Assumes the engine calls `lua_pcall` regularly during play; not yet run
in-process.

## Interpreting `+0x60`

The log prints the raw 32-bit value. If it's a heap-looking pointer (`0x0C……`/`0x1……`), it points at a
model object — add a follow-up read of its name field. If it's a scattered 32-bit value, it's a
`pandemic_hash` — resolve it against the asset name tables (`pandemic_hash` is in `sab_workshop/pack.rs`).
That resolved name is the combined-LOD WSModel to override / disable for the transparency fix.

## Camera log

`WSGameCamera::Tick` (`0x671ae0`, thiscall `(this, float dt)`, prologue `55 8B EC 83 EC 08` checked
before hooking; otherwise the log is disabled and the refusal logged) is hooked. `OdinCamera::SetFOV`
(`0xe05000`, Fusion Fix's hook site) is **not** hooked, and no hook lies near any of Fusion Fix's 24
pattern sites. While a capture runs, each Tick of the active camera (`this == [0x1321b74]`,
`[0x1321b78] >= 1`) appends a row, after the original Tick, to a new `sab_camlog_<ms>.csv` next to the exe:

`t_ms,cam,dt,fov_deg,rmb,lmb,px,py,pz,ax,ay,az,rot,pitch,ntrans`

- `fov_deg`: `+0xb0`; `px..az`: row 3 / row 2 of the camera matrix `+0x70`; `rot` / `pitch`: blended
  RotSpeed / PitchSpeed (`+0x1e8` / `+0x1ec`); `ntrans`: live transitions in the 4-slot ring (`+0xad4`).
  All reads go through `this`.
- Rows are buffered and flushed when the capture ends; the hook only `try_lock`s (drops the row when
  busy) and recovers a poisoned lock instead of panicking.
- Start: **F10** (15 s) or `Dev.CamLog(seconds)` (1-600 s) from the console.
- Analyse with `saboteur-camera-mod/scripts/camlog_analyze.py` (T95/T99 per RMB edge).

Other `Dev.*` functions: `Dev.Mark()`, `Dev.DumpNextBodySetup()`, `Dev.Log(text)` (append to
`sab_devlog.txt`), `Dev.CamLog(seconds)`.

## Status / next

- Builds clean as a 32-bit DLL; **not yet run in-process** (needs the game + a loader). First run =
  confirm the two log lines appear.
- Grow `Dev.*` once `L`-capture is confirmed: `Dev.PlayerModel()`, `Dev.OverrideCombinedLOD(hash)` to
  test the fix live without repacking assets.
