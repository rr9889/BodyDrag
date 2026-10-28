# BodyDrag for The Saboteur

Pick up knocked-out or dead soldiers, drag them by the shoulders and put them down again to hide bodies from
patrols. F7 picks up / puts down, WASD drags. Requires the GOG release of The Saboteur and Ultimate ASI Loader.
See [`installer/README.txt`](installer/README.txt) for full install, settings and uninstall instructions.

## Layout

- `sab_asi/` - the ASI plugin (Rust). BodyDrag lives in `src/bodydrag.rs` and `src/bodydrag/`; the rest is the
  `sab_asi` engine SDK it is built on.
- `installer/` - the end-user installer scripts, README and optional settings files shipped in the release.
- `docs/` - research and audit notes written while building the mod.

## Building

The game is 32-bit, so the plugin must be built for `i686-pc-windows-msvc` (set in `sab_asi/.cargo/config.toml`).

```
rustup target add i686-pc-windows-msvc
cd sab_asi
cargo build --release --no-default-features   # public build; drop the flag for the dev/debug surface
```

Output: `target/i686-pc-windows-msvc/release/sab_asi.dll` - rename to `sab_asi.asi`.

## Credits

`sab_asi` and the engine research it is built on come from
[Mercenaries-Fan-Build/notes-on-reversing-the-saboteur](https://github.com/Mercenaries-Fan-Build/notes-on-reversing-the-saboteur)
(MIT). MinHook by Tsuda Kageyu (BSD-2-Clause, see `installer/THIRD-PARTY-LICENSES.txt`).

The Saboteur is a trademark of Electronic Arts. This project ships no game files; the installer builds the mod's
animation pack from your own copy of the game.
