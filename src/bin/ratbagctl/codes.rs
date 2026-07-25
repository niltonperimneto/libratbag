/* Wire-code tables shared by the CLI's argument parsers and its renderers: LED modes, button
 * action types, special actions, logical mouse buttons, resolution capabilities and LED colour
 * depth.
 *
 * These values are duplicated from src/engine/device.rs. ratbagctl is a separate [[bin]] in the
 * ratbagd package and the package has no [lib] target, so the CLI cannot `use crate::engine::…`.
 * The duplication is exactly how the LED mode codes drifted (`breathing` used to be sent as 10,
 * which the daemon rejects), so the drift tests at the bottom of this file re-read
 * src/engine/device.rs at compile time and fail if the canonical definitions move. */

use clap::ValueEnum;

// ---------------------------------------------------------------------------
// LED modes
// ---------------------------------------------------------------------------

/// LED effect mode.
///
/// Discriminants mirror `LedMode` in src/engine/device.rs. The daemon validates the value it
/// receives against `LedMode::from_u32` (src/ipc/led.rs), so a mismatch here is either a hard
/// `InvalidArgs` failure or — worse — a silently wrong effect.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum LedModeArg {
    /// Lighting off.
    Off,
    /// A single steady colour.
    #[value(alias = "on")]
    Solid,
    /// Cycle through the colour spectrum.
    #[value(alias = "colorcycle")]
    Cycle,
    /// Fade the primary colour in and out.
    #[value(alias = "breathe")]
    Breathing,
    /// Colour wave across the lighting zones.
    #[value(name = "wave", alias = "colorwave", alias = "color-wave")]
    Wave,
    /// Twinkling effect using the primary and secondary colours.
    Starlight,
    /// Three fixed zones using the primary, secondary and tertiary colours.
    #[value(name = "tricolor", alias = "tri-color")]
    TriColor,
}

impl LedModeArg {
    pub const fn wire(self) -> u32 {
        match self {
            Self::Off => 0,
            Self::Solid => 1,
            Self::Cycle => 2,
            Self::Breathing => 3,
            Self::Wave => 4,
            Self::Starlight => 5,
            Self::TriColor => 6,
        }
    }

    pub const fn from_wire(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Off),
            1 => Some(Self::Solid),
            2 => Some(Self::Cycle),
            3 => Some(Self::Breathing),
            4 => Some(Self::Wave),
            5 => Some(Self::Starlight),
            6 => Some(Self::TriColor),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Solid => "solid",
            Self::Cycle => "cycle",
            Self::Breathing => "breathing",
            Self::Wave => "wave",
            Self::Starlight => "starlight",
            Self::TriColor => "tricolor",
        }
    }

    /// Name for a mode read off the device, which may be a code we do not know.
    pub const fn label_of(value: u32) -> &'static str {
        match Self::from_wire(value) {
            Some(mode) => mode.label(),
            None => "unknown",
        }
    }

    /// Whether the mode paints a fixed colour the user can choose.
    pub const fn uses_color(self) -> bool {
        matches!(self, Self::Solid | Self::Breathing | Self::Starlight | Self::TriColor)
    }
}

// ---------------------------------------------------------------------------
// Button action types
// ---------------------------------------------------------------------------

/* Mirrors ActionType in src/engine/device.rs. */
pub const ACTION_NONE: u32 = 0;
pub const ACTION_BUTTON: u32 = 1;
pub const ACTION_SPECIAL: u32 = 2;
pub const ACTION_KEY: u32 = 3;
pub const ACTION_MACRO: u32 = 4;

pub const fn action_type_name(action_type: u32) -> &'static str {
    match action_type {
        ACTION_NONE => "disabled",
        ACTION_BUTTON => "button",
        ACTION_SPECIAL => "special",
        ACTION_KEY => "key",
        ACTION_MACRO => "macro",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Special actions
// ---------------------------------------------------------------------------

/* Mirrors special_action in src/engine/device.rs, which in turn matches the C libratbag
 * `ratbag_button_action_special` enum. */
const SPECIAL_BASE: u32 = 1 << 30;

/// A device-handled action that is not a key or a plain button press.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum SpecialArg {
    /// Emit two left clicks.
    #[value(alias = "double-click")]
    Doubleclick,
    /// Tilt the wheel left.
    WheelLeft,
    /// Tilt the wheel right.
    WheelRight,
    /// Scroll up.
    WheelUp,
    /// Scroll down.
    WheelDown,
    /// Step to the next resolution, wrapping around.
    #[value(alias = "dpi-cycle-up")]
    ResolutionCycleUp,
    /// Step to the previous resolution, wrapping around.
    #[value(alias = "dpi-cycle-down")]
    ResolutionCycleDown,
    /// Step to the next resolution.
    #[value(alias = "dpi-up")]
    ResolutionUp,
    /// Step to the previous resolution.
    #[value(alias = "dpi-down")]
    ResolutionDown,
    /// Toggle between the current and the alternate resolution while held.
    #[value(alias = "dpi-alternate", alias = "sniper")]
    ResolutionAlternate,
    /// Return to the profile's default resolution.
    #[value(alias = "dpi-default")]
    ResolutionDefault,
    /// Step to the next profile, wrapping around.
    ProfileCycleUp,
    /// Step to the previous profile, wrapping around.
    ProfileCycleDown,
    /// Step to the next profile.
    ProfileUp,
    /// Step to the previous profile.
    ProfileDown,
    /// Hold to shift the whole device into its secondary mapping layer.
    #[value(alias = "shift")]
    SecondMode,
    /// Report the battery level, usually through the LEDs.
    BatteryLevel,
}

impl SpecialArg {
    pub const fn wire(self) -> u32 {
        SPECIAL_BASE + self.offset()
    }

    const fn offset(self) -> u32 {
        match self {
            Self::Doubleclick => 1,
            Self::WheelLeft => 2,
            Self::WheelRight => 3,
            Self::WheelUp => 4,
            Self::WheelDown => 5,
            Self::ResolutionCycleUp => 7,
            Self::ResolutionCycleDown => 8,
            Self::ResolutionUp => 9,
            Self::ResolutionDown => 10,
            Self::ResolutionAlternate => 11,
            Self::ResolutionDefault => 12,
            Self::ProfileCycleUp => 13,
            Self::ProfileCycleDown => 14,
            Self::ProfileUp => 15,
            Self::ProfileDown => 16,
            Self::SecondMode => 17,
            Self::BatteryLevel => 18,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Doubleclick => "doubleclick",
            Self::WheelLeft => "wheel-left",
            Self::WheelRight => "wheel-right",
            Self::WheelUp => "wheel-up",
            Self::WheelDown => "wheel-down",
            Self::ResolutionCycleUp => "resolution-cycle-up",
            Self::ResolutionCycleDown => "resolution-cycle-down",
            Self::ResolutionUp => "resolution-up",
            Self::ResolutionDown => "resolution-down",
            Self::ResolutionAlternate => "resolution-alternate",
            Self::ResolutionDefault => "resolution-default",
            Self::ProfileCycleUp => "profile-cycle-up",
            Self::ProfileCycleDown => "profile-cycle-down",
            Self::ProfileUp => "profile-up",
            Self::ProfileDown => "profile-down",
            Self::SecondMode => "second-mode",
            Self::BatteryLevel => "battery-level",
        }
    }

    pub fn from_wire(value: u32) -> Option<Self> {
        Self::value_variants().iter().copied().find(|action| action.wire() == value)
    }

    /// Human name for a special action read off the device.
    pub fn label_of(value: u32) -> String {
        match Self::from_wire(value) {
            Some(action) => action.label().to_string(),
            /* The daemon's UNKNOWN sentinel, and any code a newer driver invents. */
            None if value == SPECIAL_BASE => "unknown".to_string(),
            None => format!("special {value}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Logical mouse buttons
// ---------------------------------------------------------------------------

/* Logical button numbering is driver-defined, but every driver in src/hal follows the same
 * order for the first five (see logitech_g600.rs BUTTON_MAP and asus.rs ASUS_BUTTON_MAPPING).
 * Names are a convenience over those numbers; raw numbers stay first-class, and both are
 * always shown when printing a mapping. */
pub const LOGICAL_BUTTONS: &[(&str, u32)] = &[
    ("left", 1),
    ("right", 2),
    ("middle", 3),
    ("back", 4),
    ("forward", 5),
];

/// Resolve a logical button name to its number.
pub fn logical_button_code(name: &str) -> Option<u32> {
    let name = name.to_ascii_lowercase();
    LOGICAL_BUTTONS
        .iter()
        .find(|(label, _)| *label == name)
        .map(|(_, code)| *code)
        .or(match name.as_str() {
            /* Common synonyms for the side buttons. */
            "backward" | "side-back" | "button4" => Some(4),
            "side-forward" | "button5" => Some(5),
            "wheel" | "scroll" | "button3" => Some(3),
            "button1" => Some(1),
            "button2" => Some(2),
            _ => None,
        })
}

/// Name for a logical button number, if it has one.
pub fn logical_button_name(code: u32) -> Option<&'static str> {
    LOGICAL_BUTTONS
        .iter()
        .find(|(_, value)| *value == code)
        .map(|(label, _)| *label)
}

/// `right (2)`, or just `7` for a button with no conventional name.
pub fn logical_button_display(code: u32) -> String {
    match logical_button_name(code) {
        Some(name) => format!("{name} ({code})"),
        None => code.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Resolution capabilities and LED colour depth
// ---------------------------------------------------------------------------

/* Mirrors RATBAG_RESOLUTION_CAP_* in src/engine/device.rs. */
pub const fn resolution_cap_name(cap: u32) -> &'static str {
    match cap {
        1 => "individual-report-rate",
        2 => "separate-xy",
        3 => "disable",
        _ => "unknown",
    }
}

pub const fn color_depth_name(depth: u32) -> &'static str {
    match depth {
        0 => "monochrome",
        1 => "rgb",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* The canonical definitions the tables above copy. Read at compile time so that a change in
     * the daemon breaks `cargo test` instead of silently breaking `ratbagctl`. */
    const CANONICAL: &str = include_str!("../../engine/device.rs");

    /// Value of `Variant = N,` inside `pub enum <enum_name>`.
    fn canonical_variant(enum_name: &str, variant: &str) -> Option<u32> {
        let start = CANONICAL.find(&format!("pub enum {enum_name} {{"))?;
        let body = &CANONICAL[start..];
        let end = body.find("\n}")?;
        body[..end].lines().find_map(|line| {
            let rest = line.trim().strip_prefix(variant)?;
            let rest = rest.trim_start().strip_prefix('=')?;
            rest.trim().trim_end_matches(',').parse().ok()
        })
    }

    /// Right-hand side of `pub const <name>: u32 = …;`, with whitespace normalised.
    fn canonical_const(name: &str) -> Option<String> {
        CANONICAL.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(&format!("pub const {name}:"))?;
            let value = rest.split('=').nth(1)?.trim().trim_end_matches(';');
            Some(value.split_whitespace().collect::<Vec<_>>().join(" "))
        })
    }

    #[test]
    fn led_mode_wire_codes_match_the_daemon() {
        for (variant, mode) in [
            ("Off", LedModeArg::Off),
            ("Solid", LedModeArg::Solid),
            ("Cycle", LedModeArg::Cycle),
            ("Breathing", LedModeArg::Breathing),
            ("ColorWave", LedModeArg::Wave),
            ("Starlight", LedModeArg::Starlight),
            ("TriColor", LedModeArg::TriColor),
        ] {
            assert_eq!(
                canonical_variant("LedMode", variant),
                Some(mode.wire()),
                "LedMode::{variant} in src/engine/device.rs no longer matches {}",
                mode.label(),
            );
        }
    }

    #[test]
    fn action_type_wire_codes_match_the_daemon() {
        for (variant, code) in [
            ("None", ACTION_NONE),
            ("Button", ACTION_BUTTON),
            ("Special", ACTION_SPECIAL),
            ("Key", ACTION_KEY),
            ("Macro", ACTION_MACRO),
        ] {
            assert_eq!(canonical_variant("ActionType", variant), Some(code));
        }
    }

    #[test]
    fn special_action_base_and_offsets_match_the_daemon() {
        assert_eq!(canonical_const("BASE").as_deref(), Some("1 << 30"));
        for (name, action) in [
            ("DOUBLECLICK", SpecialArg::Doubleclick),
            ("WHEEL_LEFT", SpecialArg::WheelLeft),
            ("WHEEL_RIGHT", SpecialArg::WheelRight),
            ("WHEEL_UP", SpecialArg::WheelUp),
            ("WHEEL_DOWN", SpecialArg::WheelDown),
            ("RESOLUTION_CYCLE_UP", SpecialArg::ResolutionCycleUp),
            ("RESOLUTION_CYCLE_DOWN", SpecialArg::ResolutionCycleDown),
            ("RESOLUTION_UP", SpecialArg::ResolutionUp),
            ("RESOLUTION_DOWN", SpecialArg::ResolutionDown),
            ("RESOLUTION_ALTERNATE", SpecialArg::ResolutionAlternate),
            ("RESOLUTION_DEFAULT", SpecialArg::ResolutionDefault),
            ("PROFILE_CYCLE_UP", SpecialArg::ProfileCycleUp),
            ("PROFILE_CYCLE_DOWN", SpecialArg::ProfileCycleDown),
            ("PROFILE_UP", SpecialArg::ProfileUp),
            ("PROFILE_DOWN", SpecialArg::ProfileDown),
            ("SECOND_MODE", SpecialArg::SecondMode),
            ("BATTERY_LEVEL", SpecialArg::BatteryLevel),
        ] {
            assert_eq!(
                canonical_const(name).as_deref(),
                Some(format!("BASE + {}", action.offset()).as_str()),
                "special_action::{name} in src/engine/device.rs no longer matches {}",
                action.label(),
            );
        }
    }

    #[test]
    fn resolution_caps_match_the_daemon() {
        for (name, cap) in [
            ("RATBAG_RESOLUTION_CAP_INDIVIDUAL_REPORT_RATE", 1),
            ("RATBAG_RESOLUTION_CAP_SEPARATE_XY_RESOLUTION", 2),
            ("RATBAG_RESOLUTION_CAP_DISABLE", 3),
        ] {
            assert_eq!(canonical_const(name).as_deref(), Some(cap.to_string().as_str()));
            assert_ne!(resolution_cap_name(cap), "unknown");
        }
    }

    #[test]
    fn led_modes_round_trip() {
        for mode in LedModeArg::value_variants() {
            assert_eq!(LedModeArg::from_wire(mode.wire()), Some(*mode));
            assert_eq!(LedModeArg::label_of(mode.wire()), mode.label());
        }
        assert_eq!(LedModeArg::label_of(99), "unknown");
    }

    #[test]
    fn special_actions_round_trip() {
        for action in SpecialArg::value_variants() {
            assert_eq!(SpecialArg::from_wire(action.wire()), Some(*action));
            assert_eq!(SpecialArg::label_of(action.wire()), action.label());
        }
        /* The daemon's UNKNOWN sentinel is BASE with no offset. */
        assert_eq!(SpecialArg::label_of(1 << 30), "unknown");
        assert_eq!(SpecialArg::label_of(7), "special 7");
    }

    #[test]
    fn logical_buttons_round_trip() {
        for (name, code) in LOGICAL_BUTTONS {
            assert_eq!(logical_button_code(name), Some(*code));
            assert_eq!(logical_button_name(*code), Some(*name));
        }
        assert_eq!(logical_button_code("LEFT"), Some(1));
        assert_eq!(logical_button_code("backward"), Some(4));
        assert_eq!(logical_button_code("nope"), None);
        assert_eq!(logical_button_display(2), "right (2)");
        assert_eq!(logical_button_display(9), "9");
    }
}
