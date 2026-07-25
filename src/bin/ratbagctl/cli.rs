/* The ratbagctl command line.
 *
 * Shape: `ratbagctl [SELECTORS] <group> [INDEX] <verb> [VALUE]`. Selectors say *which* object to
 * act on and default to the obvious one — the only connected device, the active profile, the
 * active resolution — so the common commands carry no indices at all. Omitting a verb's value
 * reads the current setting instead of writing one.
 *
 * Values are typed here rather than in the command bodies: every ValueEnum and value_parser below
 * means clap rejects bad input, lists the accepted spellings in --help, and suggests near misses,
 * all before the CLI has opened a DBus connection. */

use clap::{Args, Parser, Subcommand};

use crate::codes::{LedModeArg, SpecialArg};
use crate::dbus_client::Dpi;
use crate::style::ColorWhen;
use crate::values::{self, MacroStep, OnOff, Rgb};

const SELECTORS: &str = "Selectors";

const EXAMPLES: &str = "\
Examples:
  ratbagctl                          list the connected devices
  ratbagctl show                     everything about the default device
  ratbagctl dpi 1600                 set the active resolution to 1600 DPI
  ratbagctl dpi                      print the active resolution's DPI
  ratbagctl rate 1000                set the active profile's report rate
  ratbagctl led color red            paint LED 0 red
  ratbagctl led mode breathing       set LED 0 to the breathing effect
  ratbagctl button 4 key a           map button 4 to the A key
  ratbagctl button 4 special wheel-up
  ratbagctl button 4 macro +ctrl c -ctrl
  ratbagctl profile 1 activate       switch to profile 1
  ratbagctl -d g502 -p 1 dpi 800     act on a named device and an explicit profile

Selectors default to the only connected device, its active profile, that profile's active
resolution, and LED 0. Every write is committed to the device immediately.";

/// Configure gaming mice through the ratbagd daemon.
///
/// Reading a setting and writing it are the same command: supply a value to set it, leave it out
/// to print it.
#[derive(Parser, Debug)]
#[command(name = "ratbagctl", version, after_help = EXAMPLES)]
pub struct Cli {
    /// Device to act on: list index, sysname, or part of its name.
    #[arg(short, long, global = true, value_name = "DEVICE", help_heading = SELECTORS)]
    pub device: Option<String>,

    /// Profile to act on [default: the active profile].
    #[arg(short, long, global = true, value_name = "N", help_heading = SELECTORS)]
    pub profile: Option<u32>,

    /// Resolution to act on [default: the active resolution].
    #[arg(short, long, global = true, value_name = "N", help_heading = SELECTORS)]
    pub resolution: Option<u32>,

    /// LED to act on [default: 0].
    #[arg(short, long, global = true, value_name = "N", help_heading = SELECTORS)]
    pub led: Option<u32>,

    /// Button to act on.
    #[arg(short, long, global = true, value_name = "N", help_heading = SELECTORS)]
    pub button: Option<u32>,

    /// When to colour the output.
    #[arg(long, global = true, value_name = "WHEN", default_value_t = ColorWhen::Auto)]
    pub color: ColorWhen,

    /// Print machine-readable JSON instead of a human-readable report.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List the connected devices.
    #[command(alias = "ls", alias = "devices")]
    List,

    /// Show a device's profiles, resolutions, buttons and LEDs.
    #[command(alias = "info")]
    Show,

    /// Write any staged changes to the device.
    Commit,

    /// Get or set the DPI of the active resolution.
    Dpi {
        /// New DPI, as `1600` or `800x1600` for separate X and Y.
        #[arg(value_name = "DPI", value_parser = values::parse_dpi)]
        value: Option<Dpi>,
    },

    /// Get or set the active profile's report rate, in Hz.
    Rate {
        #[arg(value_name = "HZ")]
        value: Option<u32>,
    },

    /// Profile commands.
    #[command(alias = "prof")]
    Profile(ProfileGroup),

    /// Resolution (DPI) commands.
    #[command(alias = "res")]
    Resolution(ResolutionGroup),

    /// Button mapping commands.
    #[command(alias = "btn")]
    Button(ButtonGroup),

    /// LED commands.
    Led(LedGroup),

    /// Dev-hooks test commands (requires a daemon built with dev-hooks).
    Test(TestGroup),
}

/* Each group takes an optional leading index, so `led 1 color red` and `-l 1 led color red` are
 * the same command. clap matches a subcommand name before it offers the token to a positional, so
 * the verb is never mistaken for the index. */

#[derive(Args, Debug)]
pub struct ProfileGroup {
    /// Profile index [default: -p, else the active profile].
    #[arg(value_name = "N")]
    pub index: Option<u32>,

    #[command(subcommand)]
    pub action: Option<ProfileCmd>,
}

#[derive(Subcommand, Debug)]
pub enum ProfileCmd {
    /// List every profile on the device.
    #[command(alias = "ls")]
    List,

    /// Show one profile in full.
    #[command(alias = "info")]
    Show,

    /// Make this profile the active one.
    #[command(alias = "use", alias = "switch")]
    Activate,

    /// Get or set the profile's name.
    Name {
        /// New name; omit to read the current one.
        #[arg(value_name = "NAME", allow_hyphen_values = true)]
        name: Option<String>,
    },

    /// Enable the profile.
    Enable,

    /// Disable the profile.
    Disable,

    /// Get or set the report rate, in Hz.
    Rate {
        #[arg(value_name = "HZ")]
        value: Option<u32>,
    },

    /// Get or set angle snapping.
    #[command(name = "angle-snapping", alias = "snapping")]
    AngleSnapping {
        #[arg(value_name = "ON_OFF")]
        value: Option<OnOff>,
    },

    /// Get or set the debounce time, in milliseconds.
    Debounce {
        #[arg(value_name = "MS")]
        value: Option<i32>,
    },
}

#[derive(Args, Debug)]
pub struct ResolutionGroup {
    /// Resolution index [default: -r, else the active resolution].
    #[arg(value_name = "N")]
    pub index: Option<u32>,

    #[command(subcommand)]
    pub action: Option<ResolutionCmd>,
}

#[derive(Subcommand, Debug)]
pub enum ResolutionCmd {
    /// List the profile's resolutions.
    #[command(alias = "ls")]
    List,

    /// Show one resolution in full.
    #[command(alias = "info")]
    Show,

    /// Get or set the DPI.
    Dpi {
        /// New DPI, as `1600` or `800x1600` for separate X and Y.
        #[arg(value_name = "DPI", value_parser = values::parse_dpi)]
        value: Option<Dpi>,
    },

    /// Make this resolution the active one.
    #[command(alias = "use")]
    Activate,

    /// Make this resolution the profile's default.
    Default,

    /// Enable this resolution slot.
    Enable,

    /// Disable this resolution slot.
    Disable,
}

#[derive(Args, Debug)]
pub struct ButtonGroup {
    /// Button index [default: -b].
    #[arg(value_name = "N")]
    pub index: Option<u32>,

    #[command(subcommand)]
    pub action: Option<ButtonCmd>,
}

#[derive(Subcommand, Debug)]
pub enum ButtonCmd {
    /// List the profile's buttons and what they do.
    #[command(alias = "ls")]
    List,

    /// Show one button's mapping and capabilities.
    #[command(alias = "info", alias = "get")]
    Show,

    /// Send a key press.
    Key {
        /// Key name (`a`, `enter`, `KEY_A`, `volumeup`) or `code:N` for a raw evdev code.
        #[arg(value_name = "KEY", value_parser = values::parse_key)]
        key: u32,
    },

    /// Act as another mouse button.
    #[command(alias = "button")]
    Click {
        /// `left`, `right`, `middle`, `back`, `forward`, or a logical button number.
        ///
        /// Named `target` rather than `button` because the global -b/--button selector already
        /// claims that argument id in every subcommand.
        #[arg(value_name = "BUTTON", value_parser = values::parse_click)]
        target: u32,
    },

    /// Perform a device-handled special action.
    Special {
        #[arg(value_name = "ACTION")]
        action: SpecialArg,
    },

    /// Play a key macro.
    ///
    /// Each step is a key name to press and release, `+key` to press, or `-key` to release. The
    /// raw `CODE:DIRECTION` form still works. Put `--` first if a step would look like an option.
    #[command(name = "macro")]
    Macro {
        /// Macro steps, e.g. `a`, `+ctrl c -ctrl`, `30:1 30:0`.
        #[arg(
            value_name = "STEP",
            required = true,
            num_args = 1..,
            allow_hyphen_values = true,
            value_parser = values::parse_macro_step,
        )]
        steps: Vec<MacroStep>,
    },

    /// Do nothing when pressed.
    #[command(alias = "off", alias = "none")]
    Disable,
}

#[derive(Args, Debug)]
pub struct LedGroup {
    /// LED index [default: -l, else 0].
    #[arg(value_name = "N")]
    pub index: Option<u32>,

    #[command(subcommand)]
    pub action: Option<LedCmd>,
}

#[derive(Subcommand, Debug)]
pub enum LedCmd {
    /// List the profile's LEDs.
    #[command(alias = "ls")]
    List,

    /// Show one LED in full.
    #[command(alias = "info", alias = "get")]
    Show,

    /// Get or set the lighting effect.
    Mode {
        #[arg(value_name = "MODE")]
        mode: Option<LedModeArg>,
    },

    /// Get or set the primary colour.
    Color {
        /// `red`, `#ff0000`, `ff0000`, `#f00` or `255,0,0`.
        #[arg(value_name = "COLOR", value_parser = values::parse_color)]
        value: Option<Rgb>,
    },

    /// Get or set the secondary colour, used by starlight and tricolor.
    #[command(name = "secondary-color", alias = "secondary")]
    SecondaryColor {
        #[arg(value_name = "COLOR", value_parser = values::parse_color)]
        value: Option<Rgb>,
    },

    /// Get or set the tertiary colour, used by tricolor.
    #[command(name = "tertiary-color", alias = "tertiary")]
    TertiaryColor {
        #[arg(value_name = "COLOR", value_parser = values::parse_color)]
        value: Option<Rgb>,
    },

    /// Get or set the brightness.
    Brightness {
        #[arg(value_name = "0-255", value_parser = clap::value_parser!(u32).range(0..=255))]
        value: Option<u32>,
    },

    /// Get or set the effect duration, in milliseconds.
    Duration {
        #[arg(value_name = "MS", value_parser = clap::value_parser!(u32).range(0..=10_000))]
        ms: Option<u32>,
    },
}

#[derive(Args, Debug)]
pub struct TestGroup {
    #[command(subcommand)]
    pub action: TestCmd,
}

#[derive(Subcommand, Debug)]
pub enum TestCmd {
    /// Load a synthetic test device from a JSON file.
    #[command(name = "load-device")]
    LoadDevice {
        #[arg(value_name = "FILE")]
        json_file: String,
    },

    /// Remove all test devices.
    Reset,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Runs clap's own assertion suite over the whole tree: duplicate flags or aliases,
    /// globals that are also required, illegal positional/subcommand combinations.
    #[test]
    fn command_tree_is_well_formed() {
        Cli::command().debug_assert();
    }

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("ratbagctl").chain(args.iter().copied()))
    }

    #[test]
    fn bare_invocation_has_no_command() {
        let cli = parse(&[]);
        assert!(matches!(cli, Ok(Cli { command: None, .. })));
    }

    #[test]
    fn group_verb_is_not_swallowed_by_the_index_positional() {
        /* The load-bearing parse: `color` must be read as the verb, not as the LED index. */
        let Ok(cli) = parse(&["led", "color", "red"]) else {
            panic!("`led color red` should parse");
        };
        match cli.command {
            Some(Command::Led(group)) => {
                assert_eq!(group.index, None);
                assert!(matches!(group.action, Some(LedCmd::Color { value: Some(_) })));
            }
            other => panic!("expected an led command, got {other:?}"),
        }
    }

    #[test]
    fn group_index_before_the_verb_is_accepted() {
        let Ok(cli) = parse(&["led", "1", "color", "red"]) else {
            panic!("`led 1 color red` should parse");
        };
        match cli.command {
            Some(Command::Led(group)) => {
                assert_eq!(group.index, Some(1));
                assert!(matches!(group.action, Some(LedCmd::Color { value: Some(_) })));
            }
            other => panic!("expected an led command, got {other:?}"),
        }
    }

    #[test]
    fn a_group_with_no_verb_is_allowed() {
        assert!(matches!(
            parse(&["button"]),
            Ok(Cli { command: Some(Command::Button(ButtonGroup { index: None, action: None })), .. })
        ));
        assert!(matches!(
            parse(&["button", "4"]),
            Ok(Cli {
                command: Some(Command::Button(ButtonGroup { index: Some(4), action: None })),
                ..
            })
        ));
    }

    #[test]
    fn selectors_are_global_and_may_follow_the_verb() {
        let Ok(cli) = parse(&["dpi", "800", "-d", "g502", "-p", "1", "-r", "2"]) else {
            panic!("selectors should be accepted after the verb");
        };
        assert_eq!(cli.device.as_deref(), Some("g502"));
        assert_eq!(cli.profile, Some(1));
        assert_eq!(cli.resolution, Some(2));
    }

    #[test]
    fn named_values_are_parsed_into_wire_codes() {
        let Ok(cli) = parse(&["button", "4", "key", "a"]) else { panic!("key a should parse") };
        match cli.command {
            Some(Command::Button(group)) => {
                assert!(matches!(group.action, Some(ButtonCmd::Key { key: 30 })));
            }
            other => panic!("expected a button command, got {other:?}"),
        }

        let Ok(cli) = parse(&["led", "mode", "breathing"]) else { panic!("mode should parse") };
        match cli.command {
            Some(Command::Led(group)) => {
                assert!(matches!(
                    group.action,
                    Some(LedCmd::Mode { mode: Some(LedModeArg::Breathing) }),
                ));
            }
            other => panic!("expected an led command, got {other:?}"),
        }
    }

    #[test]
    fn macro_steps_accept_leading_hyphens() {
        let Ok(cli) = parse(&["button", "4", "macro", "+ctrl", "c", "-ctrl"]) else {
            panic!("`-ctrl` should be read as a macro step, not an option");
        };
        match cli.command {
            Some(Command::Button(group)) => match group.action {
                Some(ButtonCmd::Macro { steps }) => assert_eq!(steps.len(), 3),
                other => panic!("expected a macro, got {other:?}"),
            },
            other => panic!("expected a button command, got {other:?}"),
        }
    }

    #[test]
    fn out_of_range_values_are_rejected_before_any_dbus_call() {
        assert!(parse(&["led", "brightness", "256"]).is_err());
        assert!(parse(&["led", "duration", "10001"]).is_err());
        assert!(parse(&["led", "mode", "nosuchmode"]).is_err());
        assert!(parse(&["button", "4", "key", "nosuchkey"]).is_err());
        assert!(parse(&["led", "color", "nosuchcolour"]).is_err());
        assert!(parse(&["button", "4", "special", "nosuchaction"]).is_err());
    }

    #[test]
    fn aliases_reach_the_same_commands() {
        assert!(matches!(parse(&["ls"]), Ok(Cli { command: Some(Command::List), .. })));
        assert!(matches!(parse(&["info"]), Ok(Cli { command: Some(Command::Show), .. })));
        assert!(matches!(parse(&["res", "list"]), Ok(Cli { command: Some(Command::Resolution(_)), .. })));
        assert!(matches!(parse(&["btn", "list"]), Ok(Cli { command: Some(Command::Button(_)), .. })));
        assert!(matches!(parse(&["prof", "ls"]), Ok(Cli { command: Some(Command::Profile(_)), .. })));
    }

    #[test]
    fn a_profile_name_may_start_with_a_hyphen() {
        let Ok(cli) = parse(&["profile", "name", "-Gaming"]) else {
            panic!("a hyphenated name should be accepted");
        };
        match cli.command {
            Some(Command::Profile(group)) => {
                assert!(matches!(group.action, Some(ProfileCmd::Name { name: Some(name) }) if name == "-Gaming"));
            }
            other => panic!("expected a profile command, got {other:?}"),
        }
    }

    #[test]
    fn separate_xy_dpi_is_accepted() {
        let Ok(cli) = parse(&["dpi", "800x1600"]) else { panic!("800x1600 should parse") };
        assert!(matches!(
            cli.command,
            Some(Command::Dpi { value: Some(Dpi::Separate { x: 800, y: 1600 }) }),
        ));
    }
}
