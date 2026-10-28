//! Sean's voice and face during a body drag: retail Wwise events (`Sound.AttachSoundEvent`) and retail FaceExpression
//! templates (`Actor.AddFaceExpression`, which drives Sean's FaceFX "Male" actor; the expression clears itself after its
//! duration). Cues are timed on the playing drag clip (game clock since it became active), never on the F7 press.
//!
//! Timing from Velvet's clips (bodydrag/FINDINGS.md): qol_drag_in pulls the torso up to sitting over 0-0.27 s and heaves
//! the body off the ground from ~1.03 s (victim pelvis leaves the floor, dragger stands up until 1.5 s);
//! qol_drag_out heaves the body up and forward at 0.47-0.67 s before the supported descent.

/// Settings, read from `sab_bodydrag_voice.ini` beside the exe at every grab (`key = value`, `#` comments). Missing
/// keys keep the defaults; an empty event or face name disables that cue.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Cfg {
    pub enabled: bool,
    pub faces: bool,
    pub lift_event: String,
    pub lift_at_s: f64,
    pub lift_face: String,
    pub lift_face_s: f32,
    pub lower_event: String,
    pub lower_at_s: f64,
    pub lower_face: String,
    pub lower_face_s: f32,
    pub breath_event: String,
    pub breath_after_s: f64,
    pub breath_gap_s: f64,
    pub breath_chance: f32,
    pub min_gap_s: f64,
    pub remarks: bool,
    pub pickup_chance: f32,
    pub pickup_at_s: f64,
    pub release_chance: f32,
    pub release_at_s: f64,
    pub drag_chance: f32,
    pub drag_after_s: f64,
    pub drag_check_s: f64,
    pub remark_cooldown_s: f64,
    pub remark_no_repeat: usize,
}

impl Default for Cfg {
    fn default() -> Self {
        Cfg {
            enabled: true,
            faces: true,
            // Sean's own melee effort grunts (vo_streamed bank; names verified in Sound\WWiseIDTable.bin: pandemic_hash(name)
            // next to the Wwise id of "Play_" + name). The heavy one for the heave, the light one for the release.
            lift_event: "Sean_Mis_GrntPnchL".into(),
            lift_at_s: 1.0,
            lift_face: "FaceExp_Pain".into(),
            lift_face_s: 0.9,
            lower_event: "Sean_Mis_GrntPnchS".into(),
            lower_at_s: 0.5,
            lower_face: "FaceExp_BrowPain".into(),
            lower_face_s: 0.8,
            breath_event: String::new(), // no native breathing / panting line for Sean (vo_streamed has none)
            breath_after_s: 8.0,
            breath_gap_s: 15.0,
            breath_chance: 0.5,
            min_gap_s: 2.5,
            remarks: true,
            pickup_chance: 0.20,
            pickup_at_s: 1.70, // after the lift grunt (1.0 s + <= 0.61 s), as qol_drag_in (1.833 s) ends
            release_chance: 0.12,
            release_at_s: 1.15, // after the release grunt and the detach at 1.0 s: the body is down
            drag_chance: 0.15,
            drag_after_s: 6.0,
            drag_check_s: 6.0,
            remark_cooldown_s: 30.0,
            remark_no_repeat: 4,
        }
    }
}

/// One retail Sean voice event used as a remark. `words` = what the recording says (transcribed from the decoded
/// wems with faster-whisper small.en and checked against GameText.dlg; voice/V2_CATALOGUE.md). Random containers
/// (several recordings) are listed only when every recording suits the moment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Line {
    pub event: &'static str,
    pub words: &'static str,
    pub weight: u32,
    pub secs: f32, // longest recording
}

/// Pickup: complaints and resigned exclamations, said as the body comes up.
pub(super) const PICKUP: [Line; 8] = [
    Line { event: "vo_cin_117_InG_TailD_Sean_Sus02", words: "Ah, Christ!", weight: 3, secs: 1.02 },
    Line { event: "vo_mis_P4M1b_Escape_LongDriveToLaHavre01_Sean_Susp01", words: "Ah, bollocks.", weight: 3, secs: 1.36 },
    Line { event: "vo_mis_P2M5_Start_Sean_Susp01", words: "Ah, Jesus, here we go!", weight: 3, secs: 1.86 },
    Line { event: "vo_mis_S1M6_Truck_Start_Sean_Susp01", words: "Bollocks, here we go again!", weight: 2, secs: 2.05 },
    Line { event: "vo_cin_216_Con_Cemetary_Sean_Susp01", words: "Ah, it's always something!", weight: 2, secs: 1.61 },
    Line { event: "vo_cin_203_InG_Depot_Sean_Susp01", words: "You're fucking kidding me!", weight: 1, secs: 1.49 },
    Line { event: "vo_mis_P2FP_RadioRescue_Escape_Sean_Susp01", words: "Shite! Hang on!", weight: 1, secs: 1.44 },
    Line { event: "Sean_Mis_ExclAwfl", words: "Brutal! / Awful, just fucking awful. / Clean diabolical, that was.", weight: 1, secs: 1.80 },
];

/// Dragging (rare): impatience, sarcasm, and Sean's own idle grumbles about the war.
pub(super) const DRAG: [Line; 4] = [
    Line { event: "vo_gen_starter_Accept_Sean_04", words: "Time's a-wastin'!", weight: 2, secs: 0.93 },
    Line { event: "vo_cin_328b_Con_Breakout_Sean_Susp01", words: "Great fucking timing, eh?", weight: 2, secs: 1.65 },
    Line { event: "vo_mis_P4M1_Escape_ExitToHQ_Sean_Susp01", words: "You're fuckin' kiddin' me.", weight: 1, secs: 1.17 },
    Line {
        event: "Sean_idl_Cmpl_Low",
        words: "How'd a simple Irish lad wind up in this mess? / Fuckin' war. Bloody stupid way to settle things. / ... (6)",
        weight: 3,
        secs: 4.29,
    },
];

/// Release (uncommon): done-with-it remarks, sarcasm at the corpse, and Sean's hiding-spot lines.
pub(super) const RELEASE: [Line; 8] = [
    Line { event: "vo_mis_DLC_Nude_Toggle_Off_1_Sean_01", words: "Oh, that's more like it!", weight: 2, secs: 1.92 },
    Line { event: "vo_cin_203_InG_Depot_Sean_Int01", words: "Now, where were we?", weight: 2, secs: 1.45 },
    Line { event: "vo_cin_311_Con_TrainGo_Sean_Int01", words: "Where were we?", weight: 1, secs: 1.14 },
    Line { event: "vo_cin_307_Con_BigGun_Sean_Int01", words: "Ah, you were saying...", weight: 2, secs: 1.65 },
    Line { event: "vo_gen_starter_cancel_Sean_04", words: "Hold that thought, will ya?", weight: 2, secs: 1.05 },
    Line { event: "vo_gen_starter_cancel_Sean_05", words: "I'll get back to ya.", weight: 1, secs: 0.99 },
    Line { event: "vo_gen_starter_cancel_Sean_08", words: "Gotta see a man about a horse.", weight: 1, secs: 1.47 },
    Line { event: "Sean_com_HidingSpotStart", words: "That should do it. / This looks like a good spot. / There! / Perfect. ... (10)", weight: 3, secs: 2.21 },
];

/// Remarks across grabs: game-clock time of the last one and the most recent events (no immediate repeats).
struct History {
    last: f64,
    recent: std::collections::VecDeque<&'static str>,
}
static HISTORY: std::sync::Mutex<History> =
    std::sync::Mutex::new(History { last: f64::NEG_INFINITY, recent: std::collections::VecDeque::new() });

/// Weighted pick among `lines` not in `recent`; `r` in [0, 1). None if every line is recent.
pub(super) fn pick(lines: &[Line], recent: &std::collections::VecDeque<&'static str>, r: f32) -> Option<Line> {
    let open: Vec<&Line> = lines.iter().filter(|l| !recent.contains(&l.event)).collect();
    let total: u32 = open.iter().map(|l| l.weight).sum();
    if total == 0 {
        return None;
    }
    let mut x = (r * total as f32) as u32;
    for l in open {
        if x < l.weight {
            return Some(*l);
        }
        x -= l.weight;
    }
    None
}

/// Wwise event / template names go into a Lua string literal: identifier characters only.
fn name_ok(s: &str) -> bool {
    s.len() <= 64 && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

impl Cfg {
    pub fn parse(text: &str) -> (Cfg, Vec<String>) {
        let mut c = Cfg::default();
        let mut bad = Vec::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
            let flag = |v: &str| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on");
            let ok = match k.as_str() {
                "enabled" => { c.enabled = flag(v); true }
                "faces" => { c.faces = flag(v); true }
                "lift_event" | "lower_event" | "breath_event" | "lift_face" | "lower_face" if name_ok(v) => {
                    let s = v.to_string();
                    match k.as_str() {
                        "lift_event" => c.lift_event = s,
                        "lower_event" => c.lower_event = s,
                        "breath_event" => c.breath_event = s,
                        "lift_face" => c.lift_face = s,
                        _ => c.lower_face = s,
                    }
                    true
                }
                "lift_at_s" => v.parse().map(|x| c.lift_at_s = x).is_ok(),
                "lower_at_s" => v.parse().map(|x| c.lower_at_s = x).is_ok(),
                "lift_face_s" => v.parse().map(|x| c.lift_face_s = x).is_ok(),
                "lower_face_s" => v.parse().map(|x| c.lower_face_s = x).is_ok(),
                "breath_after_s" => v.parse().map(|x| c.breath_after_s = x).is_ok(),
                "breath_gap_s" => v.parse().map(|x| c.breath_gap_s = x).is_ok(),
                "breath_chance" => v.parse().map(|x: f32| c.breath_chance = x.clamp(0.0, 1.0)).is_ok(),
                "min_gap_s" => v.parse().map(|x| c.min_gap_s = x).is_ok(),
                "remarks" => { c.remarks = flag(v); true }
                "pickup_chance" => v.parse().map(|x: f32| c.pickup_chance = x.clamp(0.0, 1.0)).is_ok(),
                "release_chance" => v.parse().map(|x: f32| c.release_chance = x.clamp(0.0, 1.0)).is_ok(),
                "drag_chance" => v.parse().map(|x: f32| c.drag_chance = x.clamp(0.0, 1.0)).is_ok(),
                "pickup_at_s" => v.parse().map(|x| c.pickup_at_s = x).is_ok(),
                "release_at_s" => v.parse().map(|x| c.release_at_s = x).is_ok(),
                "drag_after_s" => v.parse().map(|x| c.drag_after_s = x).is_ok(),
                "drag_check_s" => v.parse().map(|x: f64| c.drag_check_s = x.max(1.0)).is_ok(),
                "remark_cooldown_s" => v.parse().map(|x| c.remark_cooldown_s = x).is_ok(),
                "remark_no_repeat" => v.parse().map(|x: usize| c.remark_no_repeat = x.min(8)).is_ok(),
                _ => false,
            };
            if !ok {
                bad.push(line.to_string());
            }
        }
        (c, bad)
    }
}

/// What the drag is doing this frame, as seen by the voice.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Stage {
    Lifting,
    Holding,
    Walking,
    Lowering,
}

/// Per-interaction voice state.
pub(super) struct Voice {
    cfg: Cfg,
    lift_done: bool,
    lower_done: bool,
    last_vocal: f64,
    /// Game clock until which a line of ours is still playing (grunt or remark): nothing else starts before it.
    speaking_until: f64,
    walk_since: Option<f64>,
    next_breath: f64,
    pickup_rolled: bool,
    release_rolled: bool,
    drag_remarked: bool,
    next_drag_check: f64,
    rng: u32,
}

/// Longest V1 grunt recording (Sean_Mis_GrntPnchL 0.61 s) plus a short pause.
const GRUNT_MS: f64 = 650.0;
const REMARK_TAIL_MS: f64 = 250.0;

impl Voice {
    pub fn new(cfg: Cfg, seed: u32) -> Self {
        Voice {
            cfg,
            lift_done: false,
            lower_done: false,
            last_vocal: f64::NEG_INFINITY,
            speaking_until: f64::NEG_INFINITY,
            walk_since: None,
            next_breath: 0.0,
            pickup_rolled: false,
            release_rolled: false,
            drag_remarked: false,
            next_drag_check: 0.0,
            rng: seed | 1,
        }
    }

    /// Roll `chance` and, if it hits and nothing blocks it, queue one remark from `lines`. Blocks: our own line still
    /// playing, the cooldown since the last remark of any grab, and the recent-lines list. In game the call is also
    /// skipped while Sean is in a conversation (mission dialogue).
    fn remark(&mut self, now: f64, what: &str, lines: &[Line], chance: f32, out: &mut Vec<(String, String)>) -> bool {
        let r = self.roll();
        if r >= chance {
            out.push((format!("remark ({what}) not this time (roll {r:.2} >= {chance:.2})"), String::new()));
            return false;
        }
        if now < self.speaking_until {
            out.push((format!("remark ({what}) skipped: a line is still playing"), String::new()));
            return false;
        }
        let mut h = HISTORY.lock().unwrap_or_else(|e| e.into_inner());
        if now - h.last < self.cfg.remark_cooldown_s * 1000.0 {
            out.push((format!("remark ({what}) skipped: cooldown ({:.0} s since the last)", (now - h.last) / 1000.0), String::new()));
            return false;
        }
        let r2 = self.roll();
        let Some(line) = pick(lines, &h.recent, r2) else {
            out.push((format!("remark ({what}) skipped: every line was used recently"), String::new()));
            return false;
        };
        h.last = now;
        h.recent.push_back(line.event);
        while h.recent.len() > self.cfg.remark_no_repeat {
            h.recent.pop_front();
        }
        drop(h);
        self.speaking_until = now + line.secs as f64 * 1000.0 + REMARK_TAIL_MS;
        self.last_vocal = now;
        out.push((
            format!("remark ({what}) {} \"{}\"", line.event, line.words),
            format!("if not Cin.IsHumanInConversation(h) then Sound.AttachSoundEvent(h, \"{}\") end", line.event),
        ));
        true
    }

    fn roll(&mut self) -> f32 {
        // xorshift32
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng >> 8) as f32 / (1u32 << 24) as f32
    }

    fn vocal(&mut self, now: f64, event: &str, out: &mut Vec<(String, String)>) {
        if event.is_empty() {
            return;
        }
        if now - self.last_vocal < self.cfg.min_gap_s * 1000.0 || now < self.speaking_until {
            out.push((format!("voice {event} skipped ({:.0} ms after the last)", now - self.last_vocal), String::new()));
            return;
        }
        self.last_vocal = now;
        self.speaking_until = now + GRUNT_MS;
        out.push((format!("voice {event}"), format!("Sound.AttachSoundEvent(h, \"{event}\")")));
    }

    fn face(&self, name: &str, secs: f32, out: &mut Vec<(String, String)>) {
        if self.cfg.faces && !name.is_empty() && secs > 0.0 {
            out.push((format!("face {name} {secs:.2} s"), format!("Actor.AddFaceExpression(h, \"{name}\", {secs:.2})")));
        }
    }

    /// One frame: `clip_s` = seconds since the current drag clip became active (None while queued). Returns the
    /// (log label, Lua call) pairs to queue; an empty call is a log-only entry.
    pub fn tick(&mut self, stage: Stage, clip_s: Option<f64>, now: f64) -> Vec<(String, String)> {
        let mut out = Vec::new();
        if !self.cfg.enabled {
            return out;
        }
        if stage == Stage::Walking {
            self.walk_since.get_or_insert(now);
        } else {
            self.walk_since = None;
        }
        match (stage, clip_s) {
            (Stage::Lifting, Some(t)) if !self.lift_done && t >= self.cfg.lift_at_s => {
                self.lift_done = true;
                let (e, f, s) = (self.cfg.lift_event.clone(), self.cfg.lift_face.clone(), self.cfg.lift_face_s);
                self.vocal(now, &e, &mut out);
                self.face(&f, s, &mut out);
            }
            (Stage::Lowering, Some(t)) if !self.lower_done && t >= self.cfg.lower_at_s => {
                self.lower_done = true;
                let (e, f, s) = (self.cfg.lower_event.clone(), self.cfg.lower_face.clone(), self.cfg.lower_face_s);
                self.vocal(now, &e, &mut out);
                self.face(&f, s, &mut out);
            }
            _ => {}
        }
        if self.cfg.remarks {
            match (stage, clip_s) {
                (Stage::Lifting, Some(t)) if !self.pickup_rolled && t >= self.cfg.pickup_at_s => {
                    self.pickup_rolled = true;
                    let c = self.cfg.pickup_chance;
                    self.remark(now, "pickup", &PICKUP, c, &mut out);
                }
                (Stage::Lowering, Some(t)) if !self.release_rolled && t >= self.cfg.release_at_s => {
                    self.release_rolled = true;
                    let c = self.cfg.release_chance;
                    self.remark(now, "release", &RELEASE, c, &mut out);
                }
                _ => {}
            }
            if let Some(since) = self.walk_since {
                if !self.drag_remarked && now - since >= self.cfg.drag_after_s * 1000.0 && now >= self.next_drag_check {
                    self.next_drag_check = now + self.cfg.drag_check_s * 1000.0;
                    let c = self.cfg.drag_chance;
                    self.drag_remarked = self.remark(now, "dragging", &DRAG, c, &mut out);
                }
            }
        }
        if let Some(since) = self.walk_since {
            if !self.cfg.breath_event.is_empty() && now - since >= self.cfg.breath_after_s * 1000.0 && now >= self.next_breath {
                self.next_breath = now + self.cfg.breath_gap_s * 1000.0;
                if self.roll() < self.cfg.breath_chance && now - self.last_vocal >= self.cfg.breath_gap_s * 1000.0 {
                    let e = self.cfg.breath_event.clone();
                    self.vocal(now, &e, &mut out);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(v: &[(String, String)]) -> Vec<&str> {
        v.iter().filter(|(_, c)| !c.is_empty()).map(|(_, c)| c.as_str()).collect()
    }

    #[test]
    fn config_overrides_and_rejects_unsafe_names() {
        let (c, bad) = Cfg::parse("enabled = yes\nlift_event = vo_test_01 # comment\nlift_at_s=1.2\nlower_event = x\"); os.exit(\nbreath_chance = 7\nnonsense = 1\n");
        assert_eq!(c.lift_event, "vo_test_01");
        assert_eq!(c.lift_at_s, 1.2);
        assert_eq!(c.lower_event, Cfg::default().lower_event, "a name with quotes is rejected, default kept");
        assert_eq!(c.breath_chance, 1.0, "clamped");
        assert_eq!(bad.len(), 2, "{bad:?}");
        assert_eq!(Cfg::parse("").0, Cfg::default());
    }

    #[test]
    fn lift_cue_fires_once_on_clip_time_not_on_the_press() {
        let mut v = Voice::new(Cfg::default(), 7);
        assert!(v.tick(Stage::Lifting, None, 0.0).is_empty(), "queued clip: nothing");
        assert!(v.tick(Stage::Lifting, Some(0.5), 500.0).is_empty(), "before the heave");
        let a = v.tick(Stage::Lifting, Some(1.0), 1000.0);
        assert_eq!(calls(&a), ["Sound.AttachSoundEvent(h, \"Sean_Mis_GrntPnchL\")", "Actor.AddFaceExpression(h, \"FaceExp_Pain\", 0.90)"]);
        assert!(calls(&v.tick(Stage::Lifting, Some(1.5), 1500.0)).is_empty(), "once per grab");
    }

    #[test]
    fn lowering_cue_and_min_gap_holds() {
        let mut v = Voice::new(Cfg::default(), 7);
        v.tick(Stage::Lifting, Some(1.0), 1000.0);
        let a = v.tick(Stage::Lowering, Some(0.5), 2000.0); // 1 s after the lift grunt: inside min_gap
        assert_eq!(calls(&a), ["Actor.AddFaceExpression(h, \"FaceExp_BrowPain\", 0.80)"], "voice skipped, face kept");
        let mut v = Voice::new(Cfg::default(), 7);
        assert_eq!(
            calls(&v.tick(Stage::Lowering, Some(0.5), 9000.0)),
            ["Sound.AttachSoundEvent(h, \"Sean_Mis_GrntPnchS\")", "Actor.AddFaceExpression(h, \"FaceExp_BrowPain\", 0.80)"]
        );
        let mut v = Voice::new(Cfg { lower_event: String::new(), ..Cfg::default() }, 7);
        assert_eq!(calls(&v.tick(Stage::Lowering, Some(0.5), 9000.0)).len(), 1, "empty event: face only");
    }

    #[test]
    fn breathing_is_rare_and_never_on_every_loop() {
        let cfg = Cfg { breath_event: "vo_breath".into(), breath_chance: 1.0, remarks: false, ..Cfg::default() };
        let mut v = Voice::new(cfg, 7);
        let mut n = 0;
        for k in 0..(60 * 60) {
            let now = k as f64 * 1000.0 / 60.0; // one minute of walking at 60 fps
            n += calls(&v.tick(Stage::Walking, Some((now % 733.0) / 1000.0), now)).len();
        }
        assert!((3..=4).contains(&n), "breaths in 60 s of walking with chance 1: {n} (first after 8 s, then >= 15 s apart)");
        let mut v = Voice::new(Cfg { breath_event: "vo_breath".into(), breath_chance: 1.0, remarks: false, ..Cfg::default() }, 7);
        for k in 0..600 {
            let now = k as f64 * 100.0;
            let st = if k % 50 < 40 { Stage::Walking } else { Stage::Holding }; // stops every 4 s
            assert!(calls(&v.tick(st, Some(0.1), now)).is_empty(), "short walks never reach breath_after_s");
        }
    }

    fn remarks_in(v: &[(String, String)]) -> Vec<&str> {
        calls(v).into_iter().filter(|c| c.contains("IsHumanInConversation")).collect()
    }

    /// One test, run sequentially, because remark history is shared across grabs (and test threads).
    #[test]
    fn remarks_are_occasional_varied_guarded_and_never_overlap() {
        // every line is a retail event name the Lua literal accepts; weighted pick skips recent lines
        for l in PICKUP.iter().chain(DRAG.iter()).chain(RELEASE.iter()) {
            assert!(name_ok(l.event), "{}", l.event);
        }
        let mut recent = std::collections::VecDeque::new();
        recent.extend(PICKUP[..7].iter().map(|l| l.event));
        assert_eq!(pick(&PICKUP, &recent, 0.99).unwrap().event, PICKUP[7].event, "only the non-recent line is left");
        recent.push_back(PICKUP[7].event);
        assert!(pick(&PICKUP, &recent, 0.5).is_none());

        // ~20 % of pickups, no event twice within REMARK_NO_REPEAT remarks; grabs 60 s apart (past the cooldown)
        let cfg = Cfg::default();
        let (mut said, mut seq) = (0, Vec::new());
        let base = 1.0e9;
        for k in 0..2000u32 {
            let t0 = base + k as f64 * 60_000.0;
            let mut v = Voice::new(cfg.clone(), k.wrapping_mul(2654435761));
            v.tick(Stage::Lifting, Some(1.0), t0);
            let r = v.tick(Stage::Lifting, Some(1.70), t0 + 700.0);
            let rm = remarks_in(&r);
            assert!(rm.len() <= 1);
            if let Some(c) = rm.first() {
                said += 1;
                seq.push(c.to_string());
            }
        }
        let share = said as f32 / 2000.0;
        assert!((0.16..0.24).contains(&share), "pickup remark share {share:.3}");
        for w in seq.windows(cfg.remark_no_repeat + 1) {
            assert!(!w[1..].contains(&w[0]), "repeat within {} remarks: {w:?}", cfg.remark_no_repeat);
        }
        assert!(seq[0].starts_with("if not Cin.IsHumanInConversation(h) then Sound.AttachSoundEvent(h, \""), "{}", seq[0]);

        // cooldown: a sure remark, then another grab 10 s later gets none; after 30 s it may speak again
        let sure = Cfg { pickup_chance: 1.0, release_chance: 1.0, drag_chance: 1.0, ..Cfg::default() };
        let t0 = 2.0e9;
        let pickup = |seed: u32, t: f64| {
            let mut v = Voice::new(sure.clone(), seed);
            v.tick(Stage::Lifting, Some(1.0), t - 700.0); // the lift grunt first, as in game
            remarks_in(&v.tick(Stage::Lifting, Some(1.70), t)).len()
        };
        assert_eq!(pickup(3, t0), 1);
        assert_eq!(pickup(4, t0 + 10_000.0), 0, "inside the cooldown");
        assert_eq!(pickup(5, t0 + 31_000.0), 1, "after the cooldown");
        let mut v = Voice::new(sure.clone(), 6);
        assert!(remarks_in(&v.tick(Stage::Lifting, Some(1.70), t0 + 90_000.0)).is_empty(), "a late grunt in the same frame blocks it");

        // no overlap: the lift grunt at 1.0 s and the remark at 1.70 s both play; a release remark while a long drag
        // remark is still playing is skipped
        let t1 = 3.0e9;
        let mut v = Voice::new(sure.clone(), 9);
        let g = v.tick(Stage::Lifting, Some(1.0), t1);
        assert_eq!(calls(&g)[0], "Sound.AttachSoundEvent(h, \"Sean_Mis_GrntPnchL\")");
        assert_eq!(remarks_in(&v.tick(Stage::Lifting, Some(1.70), t1 + 700.0)).len(), 1, "after the grunt ends");
        let mut v = Voice::new(sure.clone(), 11);
        let t2 = 4.0e9;
        let mut drag_said = 0;
        for k in 0..=200 {
            drag_said += remarks_in(&v.tick(Stage::Walking, Some(0.1), t2 + k as f64 * 100.0)).len(); // 20 s walking
        }
        assert_eq!(drag_said, 1, "one drag remark per drag, after drag_after_s");
        let rel = v.tick(Stage::Lowering, Some(1.15), t2 + 6_000.0 + 1_000.0); // 1 s after the drag remark started
        assert!(remarks_in(&rel).is_empty(), "skipped while the drag remark plays (and the cooldown)");

        // off switch: V1 grunts stay, no remark
        let mut v = Voice::new(Cfg { remarks: false, ..sure }, 13);
        v.tick(Stage::Lifting, Some(1.0), 5.0e9);
        assert!(remarks_in(&v.tick(Stage::Lifting, Some(1.70), 5.0e9 + 700.0)).is_empty());
        assert!(Cfg::parse("remarks = off").0.remarks == false && Cfg::parse("pickup_chance = 3").0.pickup_chance == 1.0);
    }

    #[test]
    fn disabled_is_silent() {
        let mut v = Voice::new(Cfg { enabled: false, ..Cfg::default() }, 7);
        assert!(v.tick(Stage::Lifting, Some(2.0), 2000.0).is_empty());
    }
}
