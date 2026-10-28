//! The body-drag action key: one binding drives both the input poll (console thread) and the HUD prompt label, so
//! they cannot drift apart. `sab_bodydrag.ini` beside the exe: `action_key = F7` (default). A key the game already
//! uses (any DIK_ name in InputTemplates.txt) or this plugin's own console keys (F9, F10) is refused, and F7 is kept.

use std::sync::OnceLock;

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub vk: i32,
    pub name: String,
    pub label: String,
    pub note: String,
}

static BINDING: OnceLock<Binding> = OnceLock::new();

const DEFAULT: &str = "F7";
/// This plugin's own keys (lib.rs console thread): F9 runs sab_run.lua, F10 starts a camera capture.
const OWN_KEYS: [&str; 2] = ["F9", "F10"];

/// Key name -> (Windows virtual-key code, DirectInput name as written in InputTemplates.txt).
pub fn parse_key(name: &str) -> Option<(i32, String)> {
    let n = name.trim().to_ascii_uppercase();
    if let Some(f) = n.strip_prefix('F').and_then(|d| d.parse::<i32>().ok()).filter(|f| (1..=12).contains(f)) {
        return Some((0x70 + f - 1, format!("DIK_F{f}")));
    }
    if n.len() == 1 {
        let c = n.as_bytes()[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some((c as i32, format!("DIK_{}", c as char)));
        }
    }
    if let Some(d) = n.strip_prefix("NUMPAD").and_then(|d| d.parse::<i32>().ok()).filter(|d| (0..=9).contains(d)) {
        return Some((0x60 + d, format!("DIK_NUMPAD{d}")));
    }
    let named = [("INSERT", 0x2d, "DIK_INSERT"), ("DELETE", 0x2e, "DIK_DELETE"), ("HOME", 0x24, "DIK_HOME"), ("END", 0x23, "DIK_END")];
    named.iter().find(|(k, _, _)| *k == n).map(|&(_, vk, dik)| (vk, dik.to_string()))
}

/// The binding from the ini text and the game's InputTemplates.txt text (both may be empty).
pub fn resolve(ini: &str, templates: &str) -> (String, i32, String) {
    let wanted = ini
        .lines()
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("action_key"))
        .map(|(_, v)| v.trim().to_string());
    let (dvk, _) = parse_key(DEFAULT).expect("default key parses");
    let Some(w) = wanted else { return (DEFAULT.into(), dvk, "default".into()) };
    let Some((vk, dik)) = parse_key(&w) else {
        return (DEFAULT.into(), dvk, format!("\"{w}\" is not a supported key name; F7 kept"));
    };
    let used_by_game = templates.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).any(|t| t == dik);
    if used_by_game {
        return (DEFAULT.into(), dvk, format!("{w} is bound by the game (InputTemplates.txt {dik}); F7 kept"));
    }
    if OWN_KEYS.iter().any(|k| k.eq_ignore_ascii_case(&w)) {
        return (DEFAULT.into(), dvk, format!("{w} is this plugin's console key; F7 kept"));
    }
    (w.to_ascii_uppercase(), vk, "from sab_bodydrag.ini".into())
}

#[link(name = "user32")]
extern "system" {
    fn MapVirtualKeyW(code: u32, map_type: u32) -> u32;
    fn GetKeyNameTextW(lparam: i32, buf: *mut u16, len: i32) -> i32;
}

/// Display label of a virtual key in the current keyboard layout (e.g. "F7"), else the configured name.
fn key_label(vk: i32, fallback: &str) -> String {
    const MAPVK_VK_TO_VSC: u32 = 0;
    unsafe {
        let sc = MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC);
        if sc == 0 {
            return fallback.to_string();
        }
        let mut buf = [0u16; 32];
        let n = GetKeyNameTextW((sc << 16) as i32, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return fallback.to_string();
        }
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

/// The binding, resolved once (first call; the console thread calls this at startup).
pub fn binding() -> &'static Binding {
    BINDING.get_or_init(|| {
        let ini = std::fs::read_to_string(crate::game_file("sab_bodydrag.ini")).unwrap_or_default();
        let templates = std::fs::read_to_string(crate::game_file("InputTemplates.txt")).unwrap_or_default();
        let (name, vk, note) = resolve(&ini, &templates);
        let label = key_label(vk, &name);
        Binding { vk, name, label, note }
    })
}

/// Other settings in the same file: `prompt = off` hides the HUD prompt. Read once.
pub fn prompt_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let ini = std::fs::read_to_string(crate::game_file("sab_bodydrag.ini")).unwrap_or_default();
        !ini.lines()
            .map(|l| l.split('#').next().unwrap_or("").trim().to_ascii_lowercase())
            .filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .any(|(k, v)| k == "prompt" && matches!(v.as_str(), "off" | "0" | "no" | "false"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATES: &str = "DIK_W DIK_E DIK_F5 DIK_F6 DIK_F1 DIK_LSHIFT\nKEY = DIK_R";

    #[test]
    fn key_names_map_to_vk_and_dik() {
        assert_eq!(parse_key("F7"), Some((0x76, "DIK_F7".into())));
        assert_eq!(parse_key("g"), Some((0x47, "DIK_G".into())));
        assert_eq!(parse_key("Numpad5"), Some((0x65, "DIK_NUMPAD5".into())));
        assert_eq!(parse_key("F13"), None);
        assert_eq!(parse_key("Ctrl"), None);
    }

    #[test]
    fn binding_refuses_game_and_console_keys() {
        assert_eq!(resolve("", TEMPLATES).0, "F7");
        assert_eq!(resolve("action_key = G", TEMPLATES).0, "G");
        let (k, _, why) = resolve("action_key = E", TEMPLATES);
        assert_eq!(k, "F7");
        assert!(why.contains("DIK_E"), "{why}");
        assert_eq!(resolve("action_key = F6 # quickload", TEMPLATES).0, "F7");
        assert_eq!(resolve("action_key = F9", TEMPLATES).0, "F7", "console key");
        assert_eq!(resolve("action_key = R", TEMPLATES).0, "F7", "token after '='");
        assert_eq!(resolve("action_key = banana", TEMPLATES).0, "F7");
        assert_eq!(resolve("action_key = F8", TEMPLATES).1, 0x77);
    }
}
