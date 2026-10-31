BODY DRAG for The Saboteur - version 1.1.0
==========================================

Sean can pick up knocked-out or dead soldiers, drag them by the shoulders, and put them down again - to hide bodies
from patrols. Works on fresh bodies and on bodies that have been lying around for a while.

Controls
--------
  F7 near a body ......... pick it up ("Pick up body [F7]" appears on screen when one is in reach)
  W A S D ................ drag (relative to the camera; Sean walks backwards pulling the body)
  F7 while dragging ...... put the body down
  If Sean has a weapon out, he holsters it first.
  Sean grunts with effort and now and then makes a remark (his original voice lines).

REQUIREMENTS
------------
  1. The Saboteur, GOG release (the retail GOG Saboteur.exe). Other versions (Steam, EA App, retail disc, or an exe
     modified at the same places) are not supported: the mod checks the game code at startup and, if it does not
     match, it does nothing at all (the game runs normally; the reason is written to sab_devlog.txt).
  2. Ultimate ASI Loader by ThirteenAG (dinput8.dll in the game folder) - NOT included, install it first.
     If you already use another ASI mod or Fusion Fix, you most likely have it already.
  3. The game's original DLC\01\Animations.pack (present in the GOG release). The installer checks it.

CHANGES
-------
  1.1.0  Fixes the frame-rate drop and stutter while a body is carried. The plugin checked memory with a slow Windows
         call before every single read (thousands per frame); it now checks each memory region once per frame. In a
         test session the plugin's cost while carrying fell from about 22 ms to about 1 ms per frame.
         Animations, controls and settings are unchanged.
  1.0.0  First release.

UPGRADING FROM 1.0.0
--------------------
  Close the game, extract this archive over the old BodyDrag folder and run  BodyDrag\Install BodyDrag.bat  again.
  It sees the animations are already installed and only replaces scripts\sab_asi.asi.

INSTALL
-------
  1. Install Ultimate ASI Loader (copy its dinput8.dll next to Saboteur.exe).
  2. Extract this archive into the game folder, so that the "BodyDrag" folder (this folder) sits next to Saboteur.exe.
  3. Close the game and run  BodyDrag\Install BodyDrag.bat
     It builds the mod's animation pack from YOUR original DLC\01\Animations.pack (the mod ships only its own new
     animations), backs up the original to BodyDrag\backup, and copies sab_asi.asi into the "scripts" folder.
     It refuses to touch the pack if it is not the original file (e.g. changed by another mod).

UNINSTALL
---------
  Close the game and run  BodyDrag\Uninstall BodyDrag.bat  (restores the original DLC pack, removes the plugin).
  Then delete the BodyDrag folder if you like. The ASI loader is left in place.

SETTINGS (optional)
-------------------
  Copy a file from BodyDrag\optional\ next to Saboteur.exe and edit it:
    sab_bodydrag.ini ........ action key (default F7; keys the game already uses are refused), prompt on/off
    sab_bodydrag_voice.ini .. effort grunts, facial expressions, how often Sean makes remarks (remarks = off)

COMPATIBILITY
-------------
  - Conflicts with any other mod that replaces DLC\01\Animations.pack.
  - Uses the file name scripts\sab_asi.asi.
  - Game files changed: DLC\01\Animations.pack (original backed up). Saves are not modified.

KNOWN LIMITATIONS
-----------------
  - Keyboard only (no controller button for pick up / put down).
  - The body is held by physics, so on stairs, slopes and in tight spaces it can snag or look awkward.
  - A sab_devlog.txt log is written next to the game (kept under 5 MB); include it with bug reports.

Credits
-------
  Mod and engine research myself the author!
  Uses MinHook (Tsuda Kageyu) and minhook-sys (Ivan Molodetskikh), BSD 2-Clause - see THIRD-PARTY-LICENSES.txt.
