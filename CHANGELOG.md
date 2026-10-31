# Changelog

## 1.1.0 (2026-10-31)
- Fix: frame-rate drop and stutter while carrying a body. Every guarded memory read used to call `VirtualQuery`
  (a WOW64 kernel call in this 32-bit game), thousands of times per frame. `readable()` and `lifecycle::writable()`
  now share a per-frame cache of the queried regions (`region()` in `sab_asi/src/bodydrag.rs`), emptied every frame,
  so the safety check is unchanged. Measured in game: plugin cost while carrying ~22 ms/frame -> 0.2-1.1 ms/frame.
- Animations (`bodydrag_anims.subpack`), installer logic, controls and settings unchanged.

## 1.0.0 (2026-10-28)
- First release.
