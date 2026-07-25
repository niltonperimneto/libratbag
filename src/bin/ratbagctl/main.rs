/* ratbagctl: command-line client for the ratbagd DBus daemon.
 *
 * This file is the root: it resolves how output should look, parses the command line, merges the
 * selectors, and dispatches to one function per command. Everything else lives in a sibling
 * module — cli for the argument grammar, values/codes/keys for turning human words into wire
 * values, target for selector resolution, render and report for output, commands for the work. */

mod cli;
mod codes;
mod commands;
mod dbus_client;
mod errors;
mod keys;
mod render;
mod report;
mod style;
mod target;
mod values;

use std::process::ExitCode;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches};

use cli::{
    ButtonCmd, ButtonGroup, Cli, Command, LedCmd, LedGroup, ProfileCmd, ProfileGroup,
    ResolutionCmd, ResolutionGroup, TestCmd,
};
use commands::ColorSlot;
use dbus_client::RatbagClient;
use report::HintExt;
use style::Style;
use target::{Cx, Selectors};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    /* Resolve colour before parsing so that clap's own usage errors obey --color too. */
    let when = style::prescan_color(std::env::args_os());
    let matches = match Cli::command().color(style::clap_color(when)).try_get_matches() {
        Ok(matches) => matches,
        Err(err) => err.exit(),
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(err) => err.exit(),
    };

    match run(&cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report::render_error(Style::for_stderr(cli.color), &err);
            ExitCode::from(report::exit_code(&err))
        }
    }
}

async fn run(cli: &Cli) -> Result<()> {
    let client = RatbagClient::connect()
        .await
        .hint("is ratbagd running? check with `systemctl status ratbagd`")
        .map_err(|err| err.context("cannot reach ratbagd on org.freedesktop.ratbag1"))?;

    let cx = Cx::new(client, Style::for_stdout(cli.color, cli.json), cli.json, selectors(cli));

    match &cli.command {
        /* A bare `ratbagctl` shows what is connected: the most useful thing to say to someone who
         * does not yet know what to type. */
        None | Some(Command::List) => commands::list(&cx).await,
        Some(Command::Show) => commands::show(&cx).await,
        Some(Command::Commit) => commands::commit(&cx).await,
        Some(Command::Dpi { value }) => commands::resolution_dpi(&cx, *value).await,
        Some(Command::Rate { value }) => commands::profile_rate(&cx, *value).await,
        Some(Command::Profile(group)) => profile(&cx, group).await,
        Some(Command::Resolution(group)) => resolution(&cx, group).await,
        Some(Command::Button(group)) => button(&cx, group).await,
        Some(Command::Led(group)) => led(&cx, group).await,
        Some(Command::Test(group)) => match &group.action {
            TestCmd::LoadDevice { json_file } => commands::test_load_device(&cx, json_file).await,
            TestCmd::Reset => commands::test_reset(&cx).await,
        },
    }
}

/// Merge each group's leading index over the global selector flags.
///
/// The index sits next to the verb it qualifies, so it wins: in `ratbagctl -l 0 led 1 color red`,
/// LED 1 is the one the user meant.
fn selectors(cli: &Cli) -> Selectors {
    let mut selectors = Selectors {
        device: cli.device.clone(),
        profile: cli.profile,
        resolution: cli.resolution,
        led: cli.led,
        button: cli.button,
    };
    match &cli.command {
        Some(Command::Profile(group)) => selectors.profile = group.index.or(selectors.profile),
        Some(Command::Resolution(group)) => {
            selectors.resolution = group.index.or(selectors.resolution);
        }
        Some(Command::Button(group)) => selectors.button = group.index.or(selectors.button),
        Some(Command::Led(group)) => selectors.led = group.index.or(selectors.led),
        _ => {}
    }
    selectors
}

async fn profile(cx: &Cx, group: &ProfileGroup) -> Result<()> {
    match &group.action {
        /* `ratbagctl profile` lists them; `ratbagctl profile 1` shows that one. */
        None if group.index.is_none() => commands::profile_list(cx).await,
        None | Some(ProfileCmd::Show) => commands::profile_show(cx).await,
        Some(ProfileCmd::List) => commands::profile_list(cx).await,
        Some(ProfileCmd::Activate) => commands::profile_activate(cx).await,
        Some(ProfileCmd::Name { name }) => commands::profile_name(cx, name.clone()).await,
        Some(ProfileCmd::Enable) => commands::profile_set_enabled(cx, true).await,
        Some(ProfileCmd::Disable) => commands::profile_set_enabled(cx, false).await,
        Some(ProfileCmd::Rate { value }) => commands::profile_rate(cx, *value).await,
        Some(ProfileCmd::AngleSnapping { value }) => {
            commands::profile_angle_snapping(cx, *value).await
        }
        Some(ProfileCmd::Debounce { value }) => commands::profile_debounce(cx, *value).await,
    }
}

async fn resolution(cx: &Cx, group: &ResolutionGroup) -> Result<()> {
    match &group.action {
        None if group.index.is_none() => commands::resolution_list(cx).await,
        None | Some(ResolutionCmd::Show) => commands::resolution_show(cx).await,
        Some(ResolutionCmd::List) => commands::resolution_list(cx).await,
        Some(ResolutionCmd::Dpi { value }) => commands::resolution_dpi(cx, *value).await,
        Some(ResolutionCmd::Activate) => commands::resolution_activate(cx).await,
        Some(ResolutionCmd::Default) => commands::resolution_default(cx).await,
        Some(ResolutionCmd::Enable) => commands::resolution_set_enabled(cx, true).await,
        Some(ResolutionCmd::Disable) => commands::resolution_set_enabled(cx, false).await,
    }
}

async fn button(cx: &Cx, group: &ButtonGroup) -> Result<()> {
    match &group.action {
        None if group.index.is_none() => commands::button_list(cx).await,
        None | Some(ButtonCmd::Show) => commands::button_show(cx).await,
        Some(ButtonCmd::List) => commands::button_list(cx).await,
        Some(ButtonCmd::Key { key }) => commands::button_set(cx, codes::ACTION_KEY, *key).await,
        Some(ButtonCmd::Click { target }) => {
            commands::button_set(cx, codes::ACTION_BUTTON, *target).await
        }
        Some(ButtonCmd::Special { action }) => {
            commands::button_set(cx, codes::ACTION_SPECIAL, action.wire()).await
        }
        Some(ButtonCmd::Macro { steps }) => commands::button_macro(cx, steps).await,
        Some(ButtonCmd::Disable) => commands::button_set(cx, codes::ACTION_NONE, 0).await,
    }
}

async fn led(cx: &Cx, group: &LedGroup) -> Result<()> {
    match &group.action {
        None if group.index.is_none() => commands::led_list(cx).await,
        None | Some(LedCmd::Show) => commands::led_show(cx).await,
        Some(LedCmd::List) => commands::led_list(cx).await,
        Some(LedCmd::Mode { mode }) => commands::led_mode(cx, *mode).await,
        Some(LedCmd::Color { value }) => commands::led_color(cx, ColorSlot::Primary, *value).await,
        Some(LedCmd::SecondaryColor { value }) => {
            commands::led_color(cx, ColorSlot::Secondary, *value).await
        }
        Some(LedCmd::TertiaryColor { value }) => {
            commands::led_color(cx, ColorSlot::Tertiary, *value).await
        }
        Some(LedCmd::Brightness { value }) => commands::led_brightness(cx, *value).await,
        Some(LedCmd::Duration { ms }) => commands::led_duration(cx, *ms).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        match Cli::try_parse_from(std::iter::once("ratbagctl").chain(args.iter().copied())) {
            Ok(cli) => cli,
            Err(err) => panic!("{args:?} should parse: {err}"),
        }
    }

    #[test]
    fn a_group_index_overrides_the_global_flag() {
        let selectors = selectors(&parse(&["-l", "0", "led", "1", "color", "red"]));
        assert_eq!(selectors.led, Some(1));
    }

    #[test]
    fn the_global_flag_applies_when_no_group_index_is_given() {
        let selectors = selectors(&parse(&["-l", "2", "led", "color", "red"]));
        assert_eq!(selectors.led, Some(2));
    }

    #[test]
    fn selectors_are_empty_by_default_so_targets_resolve_themselves() {
        let selectors = selectors(&parse(&["dpi", "1600"]));
        assert_eq!(selectors.device, None);
        assert_eq!(selectors.profile, None);
        assert_eq!(selectors.resolution, None);
        assert_eq!(selectors.button, None);
    }

    #[test]
    fn each_group_merges_its_own_index_only() {
        let button = selectors(&parse(&["button", "4", "key", "a"]));
        assert_eq!(button.button, Some(4));
        assert_eq!(button.led, None);

        assert_eq!(selectors(&parse(&["profile", "1", "rate", "1000"])).profile, Some(1));
        assert_eq!(selectors(&parse(&["resolution", "2", "dpi"])).resolution, Some(2));
    }
}
