//! The body-drag HUD prompt ("Pick up body [F7]" / "Put down body [F7]") through the game's own button-prompt widget,
//! the one stock pickup / disguise prompts use (FUN_005a84d0). Investigation and ABI evidence:
//! notes repo docs/bodydrag-context-prompt-investigation-2026-10-27.md; ABI re-checked against the GOG exe bytes:
//!   0x009bbb20 thiscall(manager [0x014a9b14], slot 0x13) -> widget, ret 4
//!   widget vfunc +8 -> bool ready (the stock caller skips the prompt when false)
//!   0x00770870 thiscall(widget, f32 delay, f32 duration, *u16 text, *u16 target, u32 callback) -> id, ret 0x14
//!     refuses (-1) when the four ordinary entries are full ([widget+0xc88] == 4); copies the text synchronously
//!     (<= 0x80 UTF-16 units); duration 0 = f32::MAX (stays until cleared)
//!   0x007709f0 thiscall(widget, id, f32 duration, *u16 text, *u16 target, u8 force) -> bool, ret 0x14
//!   0x0076fa00 thiscall(widget, id) -> bool exists, ret 4 (ids at widget+0x78c, stride 0x138, 5 entries)
//!   0x00770ee0 thiscall(widget, id) -> bool, ret 4: marks only that entry for native removal
//! Only our own entry id is ever touched; a full widget means no prompt this time (retried), never an eviction.
//! Game thread only (called from the game-camera tick).

use super::{in_text, r32, readable};

const HUD_MANAGER: u32 = 0x014a_9b14;
const HUD_SLOT_PROMPTS: u32 = 0x13;
const GET_HUD_OBJECT: usize = 0x009b_bb20;
const ADD_PROMPT: usize = 0x0077_0870;
const UPDATE_PROMPT: usize = 0x0077_09f0;
const HAS_PROMPT: usize = 0x0076_fa00;
const CLEAR_PROMPT: usize = 0x0077_0ee0;
/// The add helper's text buffer holds 0x80 UTF-16 units; stay well inside it.
pub const MAX_UNITS: usize = 120;

pub struct Prompt {
    widget: u32,
    id: i32,
    text: String,
}

impl Prompt {
    pub const fn new() -> Self {
        Prompt { widget: 0, id: -1, text: String::new() }
    }
}

/// UTF-16, NUL-terminated, cut at MAX_UNITS. Angle brackets become parentheses: the native copy (0x770760) treats
/// `<...>` as a binding token and dereferences the target-text argument (we pass null) for `<TARG>`.
pub fn wide(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.replace('<', "(").replace('>', ")").encode_utf16().take(MAX_UNITS).collect();
    v.push(0);
    v
}

/// The prompt widget if the HUD has it and reports ready.
unsafe fn widget() -> Option<u32> {
    let mgr = r32(HUD_MANAGER).filter(|&m| m != 0 && readable(m, 0x34 + 4 * (HUD_SLOT_PROMPTS as usize + 1)))?;
    let get: unsafe extern "thiscall" fn(u32, u32) -> u32 = core::mem::transmute(GET_HUD_OBJECT);
    let w = get(mgr, HUD_SLOT_PROMPTS);
    if w == 0 || !readable(w, 0xc90) {
        return None;
    }
    let ready = r32(r32(w)?.wrapping_add(8)).filter(|&f| in_text(f))?;
    let ready: unsafe extern "thiscall" fn(u32) -> u8 = core::mem::transmute(ready as usize);
    (ready(w) != 0).then_some(w)
}

/// The HUD prompt widget exists and is ready (gameplay HUD up; false while loading).
pub unsafe fn hud_ready() -> bool {
    widget().is_some()
}

unsafe fn has(w: u32, id: i32) -> bool {
    let f: unsafe extern "thiscall" fn(u32, i32) -> u8 = core::mem::transmute(HAS_PROMPT);
    id != -1 && f(w, id) != 0
}

impl Prompt {
    /// Show `text`, or hide our entry with None. Native calls only when something changed. Returns a log line when
    /// the visible state changes (or the widget refuses).
    pub unsafe fn set(&mut self, text: Option<&str>) -> Option<String> {
        let Some(w) = widget() else {
            // HUD not available (loading, menus): our id belongs to whatever widget existed before
            if self.id != -1 {
                self.id = -1;
                self.text.clear();
                return Some("[prompt] HUD widget gone; prompt dropped".into());
            }
            return None;
        };
        if w != self.widget {
            self.widget = w;
            self.id = -1;
            self.text.clear();
        }
        let alive = has(w, self.id);
        match text {
            None => {
                if alive {
                    let clear: unsafe extern "thiscall" fn(u32, i32) -> u8 = core::mem::transmute(CLEAR_PROMPT);
                    clear(w, self.id);
                }
                let was = self.id != -1;
                self.id = -1;
                self.text.clear();
                was.then(|| "[prompt] hidden".into())
            }
            Some(t) if alive && t == self.text => None,
            Some(t) => {
                let buf = wide(t);
                if alive {
                    let update: unsafe extern "thiscall" fn(u32, i32, f32, *const u16, *const u16, u8) -> u8 =
                        core::mem::transmute(UPDATE_PROMPT);
                    if update(w, self.id, 0.0, buf.as_ptr(), core::ptr::null(), 1) != 0 {
                        self.text = t.to_string();
                        return Some(format!("[prompt] \"{t}\""));
                    }
                }
                if r32(w + 0xc88).map_or(true, |n| n >= 4) {
                    self.id = -1;
                    return (self.text != "<full>").then(|| {
                        self.text = "<full>".into();
                        "[prompt] widget full (4 stock prompts); retrying".into()
                    });
                }
                let add: unsafe extern "thiscall" fn(u32, f32, f32, *const u16, *const u16, u32) -> i32 =
                    core::mem::transmute(ADD_PROMPT);
                let id = add(w, 0.0, 0.0, buf.as_ptr(), core::ptr::null(), 0);
                if id == -1 {
                    self.id = -1;
                    return Some("[prompt] add refused".into());
                }
                self.id = id;
                self.text = t.to_string();
                Some(format!("[prompt] \"{t}\" (entry {id})"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_text_fits_the_native_buffer() {
        let w = wide("Pick up body [F7]");
        assert_eq!(*w.last().unwrap(), 0);
        assert_eq!(w.len(), "Pick up body [F7]".len() + 1);
        let long = "x".repeat(500);
        assert_eq!(wide(&long).len(), MAX_UNITS + 1, "cut well inside the 0x80-unit native buffer");
        let t = wide("Pick up <TARG> [<SAN>]");
        assert!(!t.contains(&(b'<' as u16)) && !t.contains(&(b'>' as u16)), "no native token / target expansion");
    }
}
