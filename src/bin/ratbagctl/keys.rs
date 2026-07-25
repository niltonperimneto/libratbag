/* Linux key names for `ratbagctl button N key <KEY>`, so a mapping can be written as `a`, `enter`
 * or `volumeup` instead of 30, 28 or 115.
 *
 * Values are transcribed from linux/input-event-codes.h; canonical names are the lowercased KEY_*
 * suffix, which is what lets name_of() reconstruct the KEY_* spelling for display. The table is
 * deliberately a curated subset — anything missing is still reachable as a raw code, so no key is
 * unmappable. */

/* Canonical names, in evdev code order. */
const KEYS: &[(&str, u16)] = &[
    ("esc", 1),
    ("1", 2),
    ("2", 3),
    ("3", 4),
    ("4", 5),
    ("5", 6),
    ("6", 7),
    ("7", 8),
    ("8", 9),
    ("9", 10),
    ("0", 11),
    ("minus", 12),
    ("equal", 13),
    ("backspace", 14),
    ("tab", 15),
    ("q", 16),
    ("w", 17),
    ("e", 18),
    ("r", 19),
    ("t", 20),
    ("y", 21),
    ("u", 22),
    ("i", 23),
    ("o", 24),
    ("p", 25),
    ("leftbrace", 26),
    ("rightbrace", 27),
    ("enter", 28),
    ("leftctrl", 29),
    ("a", 30),
    ("s", 31),
    ("d", 32),
    ("f", 33),
    ("g", 34),
    ("h", 35),
    ("j", 36),
    ("k", 37),
    ("l", 38),
    ("semicolon", 39),
    ("apostrophe", 40),
    ("grave", 41),
    ("leftshift", 42),
    ("backslash", 43),
    ("z", 44),
    ("x", 45),
    ("c", 46),
    ("v", 47),
    ("b", 48),
    ("n", 49),
    ("m", 50),
    ("comma", 51),
    ("dot", 52),
    ("slash", 53),
    ("rightshift", 54),
    ("kpasterisk", 55),
    ("leftalt", 56),
    ("space", 57),
    ("capslock", 58),
    ("f1", 59),
    ("f2", 60),
    ("f3", 61),
    ("f4", 62),
    ("f5", 63),
    ("f6", 64),
    ("f7", 65),
    ("f8", 66),
    ("f9", 67),
    ("f10", 68),
    ("numlock", 69),
    ("scrolllock", 70),
    ("kp7", 71),
    ("kp8", 72),
    ("kp9", 73),
    ("kpminus", 74),
    ("kp4", 75),
    ("kp5", 76),
    ("kp6", 77),
    ("kpplus", 78),
    ("kp1", 79),
    ("kp2", 80),
    ("kp3", 81),
    ("kp0", 82),
    ("kpdot", 83),
    ("102nd", 86),
    ("f11", 87),
    ("f12", 88),
    ("kpenter", 96),
    ("rightctrl", 97),
    ("kpslash", 98),
    ("sysrq", 99),
    ("rightalt", 100),
    ("home", 102),
    ("up", 103),
    ("pageup", 104),
    ("left", 105),
    ("right", 106),
    ("end", 107),
    ("down", 108),
    ("pagedown", 109),
    ("insert", 110),
    ("delete", 111),
    ("mute", 113),
    ("volumedown", 114),
    ("volumeup", 115),
    ("power", 116),
    ("kpequal", 117),
    ("pause", 119),
    ("kpcomma", 121),
    ("leftmeta", 125),
    ("rightmeta", 126),
    ("compose", 127),
    ("stop", 128),
    ("again", 129),
    ("undo", 131),
    ("copy", 133),
    ("open", 134),
    ("paste", 135),
    ("find", 136),
    ("cut", 137),
    ("help", 138),
    ("menu", 139),
    ("calc", 140),
    ("sleep", 142),
    ("wakeup", 143),
    ("www", 150),
    ("screenlock", 152),
    ("mail", 155),
    ("bookmarks", 156),
    ("computer", 157),
    ("back", 158),
    ("forward", 159),
    ("nextsong", 163),
    ("playpause", 164),
    ("previoussong", 165),
    ("stopcd", 166),
    ("record", 167),
    ("rewind", 168),
    ("homepage", 172),
    ("refresh", 173),
    ("scrollup", 177),
    ("scrolldown", 178),
    ("kpleftparen", 179),
    ("kprightparen", 180),
    ("new", 181),
    ("redo", 182),
    ("f13", 183),
    ("f14", 184),
    ("f15", 185),
    ("f16", 186),
    ("f17", 187),
    ("f18", 188),
    ("f19", 189),
    ("f20", 190),
    ("f21", 191),
    ("f22", 192),
    ("f23", 193),
    ("f24", 194),
    ("print", 210),
    ("search", 217),
    ("brightnessdown", 224),
    ("brightnessup", 225),
    ("media", 226),
    ("send", 231),
    ("reply", 232),
    ("save", 234),
    ("documents", 235),
];

/* Spellings people actually type, mapped onto the canonical name above. */
const ALIASES: &[(&str, &str)] = &[
    ("escape", "esc"),
    ("return", "enter"),
    ("ret", "enter"),
    ("ctrl", "leftctrl"),
    ("control", "leftctrl"),
    ("lctrl", "leftctrl"),
    ("rctrl", "rightctrl"),
    ("alt", "leftalt"),
    ("lalt", "leftalt"),
    ("ralt", "rightalt"),
    ("altgr", "rightalt"),
    ("shift", "leftshift"),
    ("lshift", "leftshift"),
    ("rshift", "rightshift"),
    ("meta", "leftmeta"),
    ("super", "leftmeta"),
    ("win", "leftmeta"),
    ("cmd", "leftmeta"),
    ("lmeta", "leftmeta"),
    ("rmeta", "rightmeta"),
    ("caps", "capslock"),
    ("bs", "backspace"),
    ("del", "delete"),
    ("ins", "insert"),
    ("pgup", "pageup"),
    ("pgdn", "pagedown"),
    ("pagedn", "pagedown"),
    ("period", "dot"),
    ("dash", "minus"),
    ("hyphen", "minus"),
    ("equals", "equal"),
    ("printscreen", "sysrq"),
    ("prtsc", "sysrq"),
    ("sysreq", "sysrq"),
    ("menukey", "compose"),
    ("context", "compose"),
    ("spacebar", "space"),
    ("volup", "volumeup"),
    ("voldown", "volumedown"),
    ("next", "nextsong"),
    ("prev", "previoussong"),
    ("play", "playpause"),
    ("browser", "www"),
    ("calculator", "calc"),
    ("email", "mail"),
];

/// Resolve a key name to its evdev code.
///
/// Accepts the canonical name (`a`, `enter`, `volumeup`), the `KEY_`-prefixed evdev spelling in
/// any case (`KEY_A`, `key_a`), and the aliases above. Case-insensitive.
pub fn code_of(name: &str) -> Option<u16> {
    let name = name.trim().to_ascii_lowercase();
    let name = name.strip_prefix("key_").unwrap_or(&name);
    let canonical = ALIASES
        .iter()
        .find(|(alias, _)| *alias == name)
        .map_or(name, |(_, canonical)| *canonical);
    KEYS.iter().find(|(key, _)| *key == canonical).map(|(_, code)| *code)
}

/// Canonical name for an evdev code, if the table covers it.
pub fn name_of(code: u16) -> Option<&'static str> {
    KEYS.iter().find(|(_, value)| *value == code).map(|(name, _)| *name)
}

/// `a (KEY_A, 30)` for a known key, `keycode 700` otherwise.
pub fn display(code: u32) -> String {
    match u16::try_from(code).ok().and_then(name_of) {
        Some(name) => format!("{name} (KEY_{}, {code})", name.to_ascii_uppercase()),
        None => format!("keycode {code}"),
    }
}

/// A short, representative sample for error hints — the full table is far too long to print.
pub const EXAMPLES: &str = "a, 5, enter, space, tab, esc, f1, leftctrl, shift, up, kp1, volumeup";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_codes() {
        /* Spot-checks against linux/input-event-codes.h. */
        for (name, code) in [
            ("esc", 1),
            ("1", 2),
            ("0", 11),
            ("enter", 28),
            ("leftctrl", 29),
            ("a", 30),
            ("z", 44),
            ("space", 57),
            ("f1", 59),
            ("f12", 88),
            ("up", 103),
            ("delete", 111),
            ("volumeup", 115),
            ("leftmeta", 125),
            ("f13", 183),
            ("brightnessup", 225),
        ] {
            assert_eq!(code_of(name), Some(code), "{name} should be {code}");
        }
    }

    #[test]
    fn accepts_evdev_spelling_and_case() {
        assert_eq!(code_of("KEY_A"), Some(30));
        assert_eq!(code_of("key_a"), Some(30));
        assert_eq!(code_of("A"), Some(30));
        assert_eq!(code_of(" a "), Some(30));
        assert_eq!(code_of("KEY_VOLUMEUP"), Some(115));
    }

    #[test]
    fn aliases_resolve() {
        assert_eq!(code_of("ctrl"), code_of("leftctrl"));
        assert_eq!(code_of("super"), code_of("leftmeta"));
        assert_eq!(code_of("escape"), code_of("esc"));
        assert_eq!(code_of("pgup"), code_of("pageup"));
        assert_eq!(code_of("return"), code_of("enter"));
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert_eq!(code_of("nosuchkey"), None);
        assert_eq!(code_of(""), None);
    }

    #[test]
    fn names_are_unique_and_round_trip() {
        for (name, code) in KEYS {
            assert_eq!(code_of(name), Some(*code));
            assert_eq!(name_of(*code), Some(*name), "code {code} maps back to a different name");
        }
        let mut names: Vec<&str> = KEYS.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate key name in the table");
    }

    #[test]
    fn aliases_point_at_real_keys() {
        for (alias, canonical) in ALIASES {
            assert!(code_of(canonical).is_some(), "alias {alias} points at unknown {canonical}");
            assert!(
                !KEYS.iter().any(|(name, _)| name == alias),
                "alias {alias} shadows a canonical name",
            );
        }
    }

    #[test]
    fn display_includes_both_spellings() {
        assert_eq!(display(30), "a (KEY_A, 30)");
        assert_eq!(display(115), "volumeup (KEY_VOLUMEUP, 115)");
        assert_eq!(display(60000), "keycode 60000");
    }
}
