# Body-drag contextual pickup prompt

Investigation date: 2026-10-27. Request: a stock-style `Pick up body [current binding]` prompt near an eligible body, with modular binding/display behavior. This pass traced saved native decompilation, verified critical calling conventions against installed executable bytes, and read current mod/input code. It did not change or deploy mod code or test the prompt in-game.

## Recommended route

Use the existing native `WSHUDButtonPrompt` widget, driven by a mod-owned candidate selector and binding provider. Call its native text-entry add/update/remove helpers through one version-checked adapter. The native text API accepts an already formatted UTF-16 string; a new blueprint or game-text asset is not necessary for an initial implementation.

Keep prompt presentation, target eligibility, and input binding separate. A single binding snapshot must drive both the accepted input and its displayed label. A single selected-body snapshot must drive both the visible prompt and the pickup request. This avoids a label that advertises E while pickup still requires F7, or a prompt for one body followed by selection of another.

## Evidence identity

- Installed `C:/Saboteur/The Saboteur/Saboteur.exe` SHA-256: `15f5ef3fcd5c311c259f3664c9ec7f00ad622535272fa72af32da2671ccf60fe`.
- Installed ASI at inspection: SHA-256 `0462820c8855f686653ef10003a09a3aa7c6a89416f3bfe7ade03f172dbda04e`, the v20 snapshot. Current source is being edited independently and already contains later performance work; do not equate that source with a new runtime test.
- Saved native decompilation: `output/_ghidra_saboteur/saboteur_all_functions_decomp.txt`. Missing low-address function bodies were followed directly in the installed exe with the existing `saboteur-camera-mod/bodydrag/lifecycle/follow.py` and `scripts/sabdis.py` helpers. Static analysis does not establish rendered behavior.
- Existing binding documentation: [UI/HUD Lua family](sab-engine-lua-seam/19-family-ui-hud-tutorial.md).

## 1. What stock disguise/pickup actions use

PROVEN-STATIC: generic action prompt path `0x005a84d0` obtains HUD widget slot `0x13`, checks its readiness through vtable `+8`, obtains the action's prompt blueprint, resolves localized text, and either updates an existing prompt ID or adds a text entry.

The disguise override at `0x005bcb60` has an additional `DisguiseSneakingPrompt` choice, then uses the same native update/add path. The `WSPlayerPickupAction` RTTI vtable at `0x00fb114c` uses generic prompt helper `0x005a84d0` at slot 18. `WSPlayerDisguiseAction` at `0x00fb122c` overrides that slot with `0x005bcb60`. See `data/symbol_map/pc_vtables.tsv:24715` and `:24767`.

Relevant call sequence in the installed exe:

```text
0x005a84d7  ECX = [0x014a9b14]             HUD manager
0x005a84dd  push 0x13
0x005a84e7  call 0x009bbb20                get existing prompt widget
0x005a84ee..f5                            readiness vfunc +8
0x005a8540  call 0x0076fa00                test whether saved prompt ID exists
0x005a856a  call 0x007709f0                update by ID
0x005a8598  call 0x00770870                add raw-text entry if needed
```

The native HUD functions display a supplied prompt; they do not select a corpse, check reach, or execute bodydrag. Those remain mod responsibilities. The ordinary native prompt is a contextual HUD message, not evidence of a world-space label attached above a body.

## 2. Native adapter surface

PROVEN-STATIC from decompilation plus disassembly. These are reconstructed ABI declarations for the matching 32-bit executable, not exported SDK declarations or runtime-tested calls:

```rust
type GetHudObject = unsafe extern "thiscall" fn(manager: u32, slot: u32) -> u32;
// 0x009bbb20: [ECX + 0x34 + slot*4], ret 4; manager pointer at 0x014a9b14.

type AddPrompt = unsafe extern "thiscall" fn(
    hud: u32,
    delay: f32,
    duration: f32,
    text: *const u16,
    target_text: *const u16,
    callback: u32,
) -> i32;
// 0x00770870, ret 0x14; ID in EAX, failure -1.

type UpdatePrompt = unsafe extern "thiscall" fn(
    hud: u32,
    id: i32,
    duration: f32,
    text: *const u16,
    target_text: *const u16,
    force_dirty: u8,
) -> u8;
// 0x007709f0 is the entry thunk; ret 0x14; result in AL.

type HasPrompt = unsafe extern "thiscall" fn(hud: u32, id: i32) -> u8;
// 0x0076fa00, existence check across five entries.

type ClearPrompt = unsafe extern "thiscall" fn(hud: u32, id: i32) -> u8;
// 0x00770ee0, ret 4; result in AL. Marks entry for native removal.
```

Add accepts two floats, followed by text, optional target text, and callback. The generic action caller pushes the blueprint's `+0x24` and `+0x28` fields using FPU float loads. Property dispatch `0x00770700` recognizes `Delay` hash `0x067fdff6`, `duration` hash `0x216e8465`, and `Prompt` hash `0x888505c3`. The saved decompiler's first numeric add argument is typed as an integer; do not copy that type into an FFI declaration.

The add helper copies the supplied text synchronously through `0x00770760` into native storage, using capacity `0x80` UTF-16 code units. Plain text must be at most 126 UTF-16 code units plus termination to pass that helper's strict source-length check. Bound the fully formatted label conservatively, e.g. at 120 units. Avoid relying on `<TARG>` expansion for unrestricted-length text: the initial length check does not prove the later expanded string fits.

For duration, zero is replaced by the float at `0x010a45c8`, read from the installed image as `3.4028234663852886e38` (f32 MAX). Thus a zero-duration native entry is effectively persistent until explicitly cleared. Delay zero means the mod need not inherit a blueprint's appearance delay. Pass null optional target/callback pointers when not using them. Revalidate these semantics in the first controlled game test.

## 3. Why HUD.AddButtonPrompt("Pick up body [F7]") is insufficient

PROVEN-STATIC: Lua `HUD.AddButtonPrompt` at `0x0072fed0` hashes its first string and looks up a registered blueprint in map `0x0142e6f0` (lookup at `0x0072ff44`). Missing entries return without creating a prompt. It then obtains slot `0x13`, resolves blueprint text, and calls `0x00770870` at `0x007300c4`.

The public Lua argument is a blueprint ID, not arbitrary display text. The native raw-text route reaches the same widget without requiring a new blueprint. An implementation should not replace the stock `ButtonPrompt.Pickup` string globally: that would also alter normal pickups.

## 4. Bindings: native token support and its boundary

PROVEN-STATIC: `0x00770760` copies a string, optionally expands `<TARG>`, and sends angle-bracket tokens through `0x0076fa50`. That path reads current input-action bindings with `0x00684020` and formats keyboard/mouse/gamepad labels with `0x00685b90`. `Modules/__MagicNumbers.lua:224` defines `cSOLDIER_ACTION = "<SAN>"`.

However, the existing mod separately polls Windows `VK_F7` through `GetAsyncKeyState`. The installed `InputTemplates.txt` maps `SOLDIER_ACTION` to E in DEFAULT, and Xbox Y in XBOX360; DISGUISE is F1 by default. There is no bodydrag action in that file. `<SAN>` would therefore advertise the stock action binding, not F7.

Also, token recognition at `0x0076f290` uses a fixed table of 95 three-character tokens at `0x00febf40`. Binding banks have a matching 95-action stride (`0x00684020`/`0x00684040`). Adding a `BODYDRAG` row to a text file is not established as a safe way to extend those arrays.

Recommended binding provider contract:

```text
BindingSnapshot {
    revision,
    supported input device,
    trigger code / chord,
    localized display label
}
```

- For the current mod's keyboard path, keep F7 as the default but make it a mod action setting. Read that same setting for input polling and prompt text. Do not maintain a separately configurable prompt label that can drift from the accepted key.
- Format keyboard labels using the matching Windows layout/key-name APIs, or a separately verified native key-label wrapper. Windows VK codes and DirectInput scan codes differ; F7's VK is `0x76`, while its usual DirectInput scan code is `0x41`. They are not interchangeable arguments.
- A future native-action provider may use stock tokens, but only when the actual input handler consumes that same action. A controller provider must supply working controller activation before displaying a controller button hint. The current raw F7 polling does not implement it.
- Refresh on binding revision, input-device change, keyboard-layout change, and UI recreation. Stock token substitution occurs while preparing/updating text; do not assume an already expanded string updates itself after rebinding.

This supports modularity without first expanding the game's fixed action enum. Integration into the game's Controls menu would be a separate investigation.

## 5. Prompt ownership, capacity, and display behavior

PROVEN-STATIC:

- `0x00770870` refuses additions when the ordinary queued count at `hud+0xc88` is already four. It does not grant the mod a permanently reserved display slot.
- `0x00770970` writes a separate fifth entry. It must not be treated as a free mod-only slot: native actions already use that path.
- Constructor `0x0076f4e0` initializes `ButtonPrompt.Prompt00` through `Prompt04`; helper `0x0076f390` changes native text and visibility.
- `0x00770ee0` removes only the identified entry. Ordinary entries are copied/compacted by native update logic; own the returned ID rather than a numeric row index or raw entry pointer.
- The widget also controls mission/district title, PromptTip, and other shared visibility (`0x0076f940`). Never hide, unload, or clear the whole widget just to dismiss bodydrag text.
- HUD manager `0x009bd690` adjusts all five prompt `_y` fields using existing manager offsets. Movie creation follows the stock viewport path (`0x0079e620`/`0x0079e740` -> `0x0079dd70`).

INFERRED: reusing the native movie/entry path is the most compatible starting point for resolution changes, localization, and existing HUD scaling mods. It avoids introducing a second pixel-coordinate overlay or Direct3D hook. Exact ultrawide, aspect-ratio, live-resize, text wrapping, and FusionFix behavior still require visual testing; the ActionScript layout was not inspected here.

If all four ordinary entries are occupied, keep the mod prompt absent and retry on a bounded cadence. Do not evict or overwrite an unrelated native entry. Cache the HUD object plus a level/UI generation; discard owned IDs when that generation changes. Re-resolve readiness after loading or HUD-template changes.

## 6. Shared target eligibility is necessary

The current bodydrag target search is distance-based and can start restoration and emit logs. It is not a side-effect-free hover query. Repeatedly calling it to paint a prompt would risk waking bodies, changing target selection, and adding log/per-frame work.

Factor a pure candidate query used by both UI and input:

```text
CandidateSnapshot {
    owner identity / generation,
    current body reference,
    distance and view relevance,
    eligible / blocked reason,
    action: pick up | put down
}
```

The prompt selector should:

1. Use the same reach, player-state, corpse-state, physics-capability and busy checks as the actual action. Define how a frozen/restorable body qualifies without starting restoration just to show UI.
2. Use the visible body/grip position where available. The frozen human root can differ from the visible pose; do not let a prompt select a body through a wall or far behind Sean merely because a root is close.
3. Apply an explicit view/facing/visibility policy for the requested hover-like behavior. Native raw text insertion alone provides no targeting policy. If targeting changes, pickup must revalidate the displayed body's identity; it must not silently grab a different nearest body after the input queue delay.
4. Use a small selection hysteresis so a corpse pile does not flicker between bodies.
5. Treat disguise availability separately from carry availability. A body whose uniform has already been taken can still qualify for carrying. Preserve both actions and their actual distinct bindings when applicable.

Body capability should incorporate the [carry-controller audit](bodydrag-carry-blindspots-2026-10-27.md): current type-0 and type-1 controller paths are materially different. The UI must not imply that a rejected/unsupported carry is available.

## 7. Proposed behavior and module boundaries

| State | Prompt |
|---|---|
| Eligible selected body, player free | `Pick up body [binding]` |
| Carrying, release accepted | `Put down body [binding]` |
| Pickup/release transition or other busy state | Hide the mod's entry |
| No eligible body, player invalid, HUD unavailable, load/cutscene/pause as appropriate | Hide or invalidate the owned entry |
| Binding or selected action changes | Update the same owned entry |

Recommended modules: `bodydrag/input` owns binding/activation; `bodydrag/target` supplies pure eligibility; `bodydrag/prompt` maps snapshots to presentation; `bodydrag/native_hud` contains executable identity checks and FFI details. Localized mod text belongs in a small label provider rather than in native pointer code.

Run native HUD calls on the existing game-thread seam. Poll/calculate candidate eligibility on a bounded cadence such as 10 Hz with immediate invalidation on interaction transitions; choose the rate during testing. Format/update text only when its label, action, binding, target-policy output, HUD generation, or required lifetime refresh changes. Do not compile and execute Lua every render frame. Prompt painting must not wake ragdolls, run pickup, or perform synchronous diagnostic logging.

## Validation before deployment

| Check | Evidence needed |
|---|---|
| One nearby corpse; walk/look away and back | Correct contextual appearance/disappearance and exact target identity |
| Two overlapping corpses or corpse behind wall | Stable selection and no hidden/incorrect pickup |
| Weapon pickup + usable disguise nearby | Coexistence; removal affects only the mod's entry |
| All ordinary prompt entries occupied | Graceful absence/retry; no stock message overwritten |
| Rebind F7 to another key; alternate keyboard layout | Label and actual input change together |
| Supported controller activation and device switching | Actual action works and label follows the same provider |
| 16:9, ultrawide, resolution changes, HUD scaling mod | Native layout remains legible and correctly placed |
| Load/restart, pause, cutscene, HUD-template change | No stale prompt ID or native object dereference |
| Carry -> release -> re-grab; frozen/unsupported body | Correct action/state label and shared eligibility |
| Long carry with profiling enabled | No per-frame text/Lua/log churn added |

## Reproduction of the native trace

The saved dump contains `0x00770870`, `0x007709f6`, `0x00770ee0`, `0x00770760`, `0x005a84d0`, `0x005bcb60`, and `0x009bbb20`. The update entry is the thunk `0x007709f0`, not the fractured body at `0x007709f6`.

The existing `follow.py <VA> [instruction count]` follows unconditional split-code jumps and stops at a return. For conditional branches, inspect the additional regions explicitly: `770883`, `7708dd`, `770a2e`, `770ae2`, `770f19`, `5a8506`, `72ff23`, `72ff66`, `730072`, and `771670`. This pass ran that helper with an existing Miniconda Python and the already installed pefile/capstone modules; no dependency installation was performed.

The trace establishes a concrete native integration route. In-game rendering, input-device behavior, and coexistence remain unverified until a controlled implementation test.
