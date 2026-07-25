/* Argument value parsers: colours, keys, logical buttons and macro steps.
 *
 * Each is a clap `value_parser`, so bad input is rejected during parsing — with the argument name
 * and usage attached — before the CLI opens a DBus connection or touches the device. Every parser
 * still accepts the raw numeric form the wire uses, so nothing that was expressible before this
 * layer existed became unexpressible. */

use clap::ValueEnum;

use crate::codes;
use crate::dbus_client::Dpi;
use crate::keys;

/// A 24-bit colour.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Build from the `u32` triplet the DBus API uses, clamping out-of-range components.
    pub fn from_wire(r: u32, g: u32, b: u32) -> Self {
        let clamp = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
        Self::new(clamp(r), clamp(g), clamp(b))
    }
}

impl std::fmt::Display for Rgb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/* Names accepted for colours: the eight primaries plus the ones people reach for on RGB mice. */
const NAMED_COLORS: &[(&str, Rgb)] = &[
    ("black", Rgb::new(0, 0, 0)),
    ("white", Rgb::new(255, 255, 255)),
    ("red", Rgb::new(255, 0, 0)),
    ("green", Rgb::new(0, 255, 0)),
    ("blue", Rgb::new(0, 0, 255)),
    ("yellow", Rgb::new(255, 255, 0)),
    ("cyan", Rgb::new(0, 255, 255)),
    ("aqua", Rgb::new(0, 255, 255)),
    ("magenta", Rgb::new(255, 0, 255)),
    ("fuchsia", Rgb::new(255, 0, 255)),
    ("orange", Rgb::new(255, 165, 0)),
    ("purple", Rgb::new(128, 0, 128)),
    ("pink", Rgb::new(255, 105, 180)),
    ("teal", Rgb::new(0, 128, 128)),
    ("lime", Rgb::new(50, 205, 50)),
    ("gold", Rgb::new(255, 215, 0)),
];

/// Parse a colour: `red`, `#ff0000`, `ff0000`, `#f00`, `f00` or `255,0,0`.
pub fn parse_color(raw: &str) -> Result<Rgb, String> {
    let text = raw.trim();
    let lower = text.to_ascii_lowercase();

    if let Some((_, rgb)) = NAMED_COLORS.iter().find(|(name, _)| *name == lower) {
        return Ok(*rgb);
    }

    if text.contains(',') {
        let mut parts = text.split(',');
        let mut component = |which: &str| -> Result<u8, String> {
            parts
                .next()
                .ok_or_else(|| format!("colour '{raw}' is missing its {which} component"))?
                .trim()
                .parse::<u8>()
                .map_err(|_| format!("the {which} component of '{raw}' must be a number 0-255"))
        };
        let rgb = Rgb::new(component("red")?, component("green")?, component("blue")?);
        if parts.next().is_some() {
            return Err(format!("colour '{raw}' has too many components, expected r,g,b"));
        }
        return Ok(rgb);
    }

    /* Byte-wise, not by slicing: `&s[0..2]` on a multi-byte character panics, and
     * .agents/rules/rust-coding.md rules panics out. */
    let hex = lower.strip_prefix('#').unwrap_or(&lower).as_bytes();
    if !hex.iter().all(u8::is_ascii_hexdigit) {
        return Err(color_error(raw));
    }
    let nibble = |byte: u8| -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            _ => byte - b'a' + 10,
        }
    };
    match hex.len() {
        6 => Ok(Rgb::new(
            nibble(hex[0]) * 16 + nibble(hex[1]),
            nibble(hex[2]) * 16 + nibble(hex[3]),
            nibble(hex[4]) * 16 + nibble(hex[5]),
        )),
        /* #f00 is the CSS shorthand for #ff0000: each nibble is doubled. */
        3 => Ok(Rgb::new(nibble(hex[0]) * 17, nibble(hex[1]) * 17, nibble(hex[2]) * 17)),
        _ => Err(color_error(raw)),
    }
}

fn color_error(raw: &str) -> String {
    format!(
        "invalid colour '{raw}'\n  expected a name ({}), #rrggbb, rrggbb, #rgb, or r,g,b",
        NAMED_COLORS.iter().take(6).map(|(name, _)| *name).collect::<Vec<_>>().join(", "),
    )
}

/// Parse a key: a name (`a`, `enter`, `KEY_A`, `volumeup`), or `code:N` for a raw evdev code.
///
/// A bare number that is not a key name is also read as a raw code, so `30` still means `KEY_A`.
/// Single digits are the digit keys — `5` is the `5` key (code 6), not code 5 — because that is
/// what someone typing `key 5` means; `code:5` forces the raw reading.
pub fn parse_key(raw: &str) -> Result<u32, String> {
    let text = raw.trim();

    if let Some(rest) = text.strip_prefix("code:") {
        return rest
            .trim()
            .parse::<u16>()
            .map(u32::from)
            .map_err(|_| format!("'{rest}' is not a valid evdev key code (0-65535)"));
    }

    if let Some(code) = keys::code_of(text) {
        return Ok(u32::from(code));
    }

    if let Ok(code) = text.parse::<u16>() {
        return Ok(u32::from(code));
    }

    Err(format!(
        "unknown key '{raw}'\n  expected a key name ({}), a KEY_* name, or code:N for a raw \
         evdev code",
        keys::EXAMPLES,
    ))
}

/// Parse a logical mouse button: `left`, `right`, `middle`, `back`, `forward`, or a number.
pub fn parse_click(raw: &str) -> Result<u32, String> {
    let text = raw.trim();
    if let Some(code) = codes::logical_button_code(text) {
        return Ok(code);
    }
    if let Ok(code) = text.parse::<u32>() {
        return Ok(code);
    }
    Err(format!(
        "unknown mouse button '{raw}'\n  expected {}, or a raw logical button number",
        codes::LOGICAL_BUTTONS.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", "),
    ))
}

/// Parse a DPI: `1600`, or `800x1600` for a device with separate X and Y resolution.
pub fn parse_dpi(raw: &str) -> Result<Dpi, String> {
    let text = raw.trim();
    let number = |part: &str, which: &str| -> Result<u32, String> {
        part.trim()
            .parse::<u32>()
            .map_err(|_| format!("the {which} DPI in '{raw}' must be a whole number"))
    };

    if let Some((x, y)) = text.split_once(['x', 'X']) {
        return Ok(Dpi::Separate { x: number(x, "horizontal")?, y: number(y, "vertical")? });
    }
    Ok(Dpi::Unified(number(text, "")?))
}

/// One step of a macro, before press/release expansion.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum MacroStep {
    Press(u32),
    Release(u32),
    /// Press then release — what a bare key name means.
    Tap(u32),
}

const MACRO_SYNTAX: &str = "expected `key` (press and release), `+key` (press), `-key` (release), \
                           or the raw `CODE:DIRECTION` form (1 press, 0 release)";

/// Parse one macro step: `a`, `+ctrl`, `-ctrl`, `30:1`.
pub fn parse_macro_step(raw: &str) -> Result<MacroStep, String> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(format!("empty macro step\n  {MACRO_SYNTAX}"));
    }

    if let Some(name) = text.strip_prefix('+') {
        return parse_key(name).map(MacroStep::Press);
    }
    if let Some(name) = text.strip_prefix('-') {
        return parse_key(name).map(MacroStep::Release);
    }

    /* The legacy `CODE:DIRECTION` form. `code:N` is the raw-keycode escape and must not be
     * mistaken for it. */
    if let Some((name, direction)) = text.rsplit_once(':')
        && !name.eq_ignore_ascii_case("code")
    {
        let code = parse_key(name)?;
        return match direction.trim() {
            "1" => Ok(MacroStep::Press(code)),
            "0" => Ok(MacroStep::Release(code)),
            other => Err(format!(
                "invalid direction '{other}' in macro step '{raw}'\n  use 1 for press, 0 for \
                 release, or write +{name} / -{name}",
            )),
        };
    }

    parse_key(text).map(MacroStep::Tap)
}

/// Expand macro steps into the `(keycode, direction)` pairs the DBus API takes.
pub fn expand_macro(steps: &[MacroStep]) -> Vec<(u32, u32)> {
    let mut events = Vec::with_capacity(steps.len());
    for step in steps {
        match *step {
            MacroStep::Press(code) => events.push((code, 1)),
            MacroStep::Release(code) => events.push((code, 0)),
            MacroStep::Tap(code) => {
                events.push((code, 1));
                events.push((code, 0));
            }
        }
    }
    events
}

/// Render macro events back into the input syntax, for display.
pub fn format_macro(events: &[(u32, u32)]) -> String {
    if events.is_empty() {
        return "(empty)".to_string();
    }
    let mut parts = Vec::with_capacity(events.len());
    let mut index = 0;
    while index < events.len() {
        let (code, direction) = events[index];
        let name = u16::try_from(code)
            .ok()
            .and_then(keys::name_of)
            .map_or_else(|| format!("code:{code}"), ToString::to_string);
        /* A press immediately followed by its own release is how a tap round-trips. */
        if direction == 1 && events.get(index + 1) == Some(&(code, 0)) {
            parts.push(name);
            index += 2;
        } else {
            parts.push(format!("{}{name}", if direction == 1 { '+' } else { '-' }));
            index += 1;
        }
    }
    parts.join(" ")
}

/// A boolean argument spelled the way people say it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    #[value(alias = "true", alias = "yes", alias = "1", alias = "enable", alias = "enabled")]
    On,
    #[value(alias = "false", alias = "no", alias = "0", alias = "disable", alias = "disabled")]
    Off,
}

impl OnOff {
    pub const fn as_bool(self) -> bool {
        matches!(self, Self::On)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off => "off",
        }
    }

    pub const fn from_bool(value: bool) -> Self {
        if value { Self::On } else { Self::Off }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_by_name() {
        assert_eq!(parse_color("red"), Ok(Rgb::new(255, 0, 0)));
        assert_eq!(parse_color("RED"), Ok(Rgb::new(255, 0, 0)));
        assert_eq!(parse_color(" teal "), Ok(Rgb::new(0, 128, 128)));
    }

    #[test]
    fn colors_by_hex() {
        let red = Rgb::new(255, 0, 0);
        assert_eq!(parse_color("#ff0000"), Ok(red));
        assert_eq!(parse_color("ff0000"), Ok(red));
        assert_eq!(parse_color("FF0000"), Ok(red));
        assert_eq!(parse_color("#f00"), Ok(red));
        assert_eq!(parse_color("f00"), Ok(red));
        assert_eq!(parse_color("#abc"), Ok(Rgb::new(0xaa, 0xbb, 0xcc)));
    }

    #[test]
    fn colors_by_triplet() {
        assert_eq!(parse_color("255,0,0"), Ok(Rgb::new(255, 0, 0)));
        assert_eq!(parse_color("0, 128 ,255"), Ok(Rgb::new(0, 128, 255)));
        assert!(parse_color("1,2").is_err());
        assert!(parse_color("1,2,3,4").is_err());
        assert!(parse_color("300,0,0").is_err());
    }

    #[test]
    fn bad_colors_are_errors_not_panics() {
        /* Six *bytes* but four characters, with byte 2 inside the multi-byte character: the
         * previous byte-slicing implementation panicked here. */
        assert!(parse_color("a€bc").is_err());
        assert!(parse_color("").is_err());
        assert!(parse_color("#ff00").is_err());
        assert!(parse_color("nosuchcolour").is_err());
        assert!(parse_color("zzzzzz").is_err());
    }

    #[test]
    fn color_display_is_lowercase_hex() {
        assert_eq!(Rgb::new(255, 0, 128).to_string(), "#ff0080");
        assert_eq!(Rgb::from_wire(300, 0, 1).to_string(), "#ff0001");
    }

    #[test]
    fn keys_by_name_and_code() {
        assert_eq!(parse_key("a"), Ok(30));
        assert_eq!(parse_key("KEY_A"), Ok(30));
        assert_eq!(parse_key("ctrl"), Ok(29));
        /* Not a key name, so it falls through to the raw code — old invocations still work. */
        assert_eq!(parse_key("30"), Ok(30));
        /* A digit is the digit key. */
        assert_eq!(parse_key("5"), Ok(6));
        assert_eq!(parse_key("code:5"), Ok(5));
        assert!(parse_key("nosuchkey").is_err());
        assert!(parse_key("code:99999").is_err());
    }

    #[test]
    fn clicks_by_name_and_number() {
        assert_eq!(parse_click("left"), Ok(1));
        assert_eq!(parse_click("MIDDLE"), Ok(3));
        assert_eq!(parse_click("7"), Ok(7));
        assert!(parse_click("nope").is_err());
    }

    #[test]
    fn macro_steps() {
        assert_eq!(parse_macro_step("a"), Ok(MacroStep::Tap(30)));
        assert_eq!(parse_macro_step("+ctrl"), Ok(MacroStep::Press(29)));
        assert_eq!(parse_macro_step("-ctrl"), Ok(MacroStep::Release(29)));
        assert_eq!(parse_macro_step("30:1"), Ok(MacroStep::Press(30)));
        assert_eq!(parse_macro_step("30:0"), Ok(MacroStep::Release(30)));
        assert_eq!(parse_macro_step("a:1"), Ok(MacroStep::Press(30)));
        assert_eq!(parse_macro_step("code:30"), Ok(MacroStep::Tap(30)));
        assert!(parse_macro_step("30:2").is_err());
        assert!(parse_macro_step("").is_err());
    }

    #[test]
    fn macro_expansion_matches_the_legacy_form() {
        let legacy = [parse_macro_step("30:1").unwrap_or(MacroStep::Tap(0)),
                      parse_macro_step("30:0").unwrap_or(MacroStep::Tap(0))];
        let friendly = [MacroStep::Tap(30)];
        assert_eq!(expand_macro(&legacy), expand_macro(&friendly));
        assert_eq!(expand_macro(&friendly), vec![(30, 1), (30, 0)]);
    }

    #[test]
    fn macro_formatting_round_trips() {
        let steps = [MacroStep::Press(29), MacroStep::Tap(46), MacroStep::Release(29)];
        let events = expand_macro(&steps);
        assert_eq!(format_macro(&events), "+leftctrl c -leftctrl");
        assert_eq!(format_macro(&[]), "(empty)");
        assert_eq!(format_macro(&[(60000, 1)]), "+code:60000");
    }

    #[test]
    fn on_off_spellings() {
        for text in ["on", "true", "yes", "1", "enable"] {
            assert_eq!(OnOff::from_str(text, true), Ok(OnOff::On), "{text}");
        }
        for text in ["off", "false", "no", "0", "disable"] {
            assert_eq!(OnOff::from_str(text, true), Ok(OnOff::Off), "{text}");
        }
        assert!(OnOff::from_str("maybe", true).is_err());
        assert!(OnOff::from_bool(true).as_bool());
    }
}
