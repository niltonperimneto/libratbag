/* User-facing reporting: what a write changed, and what went wrong.
 *
 * Two jobs. `Change` renders the single confirmation line a write command prints, showing
 * `old → new` so the effect is visible without a follow-up read. `render_error` replaces Rust's
 * default `Error: <debug chain>` with a red header, an indented cause chain, and hints — either
 * static ones attached at the call site with `.hint(…)`, or the data-carrying ones on CliError. */

use std::error::Error as StdError;
use std::fmt;

use crate::errors::CliError;
use crate::style::Style;
use crate::values::Rgb;

// ---------------------------------------------------------------------------
// Write confirmations
// ---------------------------------------------------------------------------

/// A property that a write command just changed.
pub struct Change {
    /// What was changed, e.g. `LED 0` or `Profile 1`.
    pub object: String,
    /// Which property of it, e.g. `mode`.
    pub property: &'static str,
    /// The previous value, where reading it first was a single cheap property get.
    pub from: Option<String>,
    /// The value now in effect.
    pub to: String,
    /// A colour to show as a swatch beside the new value.
    pub swatch: Option<Rgb>,
}

impl Change {
    pub fn new(object: impl Into<String>, property: &'static str, to: impl Into<String>) -> Self {
        Self {
            object: object.into(),
            property,
            from: None,
            to: to.into(),
            swatch: None,
        }
    }

    /// Record the previous value, so the confirmation can show `old → new`.
    pub fn from(mut self, from: impl Into<String>) -> Self {
        self.from = Some(from.into());
        self
    }

    pub const fn swatch(mut self, rgb: Rgb) -> Self {
        self.swatch = Some(rgb);
        self
    }

    /// `✓ LED 0 mode: solid → breathing`
    pub fn render(&self, style: Style) -> String {
        let swatch = match self.swatch {
            Some(rgb) if style.is_color() => format!("{} ", style.swatch(rgb.r, rgb.g, rgb.b)),
            _ => String::new(),
        };
        let head = format!(
            "{} {} {}",
            style.ok_mark(),
            style.bold(&self.object),
            style.dim(&format!("{}:", self.property)),
        );
        match &self.from {
            Some(from) => format!(
                "{head} {} {} {swatch}{}",
                style.dim(from),
                style.arrow(),
                style.accent(&self.to),
            ),
            None => format!("{head} {swatch}{}", style.accent(&self.to)),
        }
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "ok": true,
            "set": {
                "object": self.object,
                "property": self.property,
                "from": self.from,
                "to": self.to,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Hints
// ---------------------------------------------------------------------------

/// An error with a line of advice attached.
///
/// Display is transparent — it shows the wrapped error's message — and `source` skips straight to
/// the wrapped error's own cause, so wrapping does not duplicate a line in the printed chain.
#[derive(Debug)]
pub struct Hinted {
    hint: String,
    source: anyhow::Error,
}

impl fmt::Display for Hinted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.source)
    }
}

impl StdError for Hinted {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.source()
    }
}

/// Attach a hint to a fallible call.
pub trait HintExt<T> {
    fn hint(self, text: impl Into<String>) -> anyhow::Result<T>;
    /// Attach a hint only when the condition holds — for advice that is wrong in other cases.
    fn hint_if(self, condition: bool, text: impl Into<String>) -> anyhow::Result<T>;
}

impl<T, E> HintExt<T> for Result<T, E>
where
    E: Into<anyhow::Error>,
{
    fn hint(self, text: impl Into<String>) -> anyhow::Result<T> {
        self.map_err(|err| {
            anyhow::Error::new(Hinted { hint: text.into(), source: err.into() })
        })
    }

    fn hint_if(self, condition: bool, text: impl Into<String>) -> anyhow::Result<T> {
        if condition { self.hint(text) } else { self.map_err(Into::into) }
    }
}

// ---------------------------------------------------------------------------
// Error rendering
// ---------------------------------------------------------------------------

/// Print an error as a header, its cause chain, and any hints.
pub fn render_error(style: Style, err: &anyhow::Error) {
    /* Each Hinted link displays as the error it wrapped and sources that error's own cause, so
     * the chain needs no special-casing here beyond harvesting the advice. */
    let mut hints: Vec<String> = err
        .chain()
        .filter_map(|link| link.downcast_ref::<Hinted>().map(|hinted| hinted.hint.clone()))
        .collect();

    /* A hint that needs values read off the device lives on CliError; anyhow's downcast searches
     * the whole chain, including through context wrappers. */
    if let Some(cli_error) = err.downcast_ref::<CliError>() {
        hints.extend(cli_error.hints(style));
    }

    if daemon_is_absent(err) {
        hints.insert(0, "ratbagd does not appear to be running on the bus".to_string());
        hints.insert(1, "start it with `systemctl start ratbagd`".to_string());
    }

    eprintln!("{} {}", style.bad("error:"), err);
    for cause in err.chain().skip(1) {
        eprintln!("  {} {cause}", style.dim("caused by:"));
    }
    for (index, hint) in hints.iter().enumerate() {
        let label = if index == 0 { style.warn("hint:") } else { style.warn("     ") };
        eprintln!("  {label} {hint}");
    }
}

/// Whether the failure was simply that nothing is serving `org.freedesktop.ratbag1`.
///
/// Connecting to the bus succeeds whether or not the daemon is there, so this only surfaces on the
/// first method call — as `ServiceUnknown`, whose stock text ("The name … was not provided by any
/// .service files") never mentions ratbagd.
fn daemon_is_absent(err: &anyhow::Error) -> bool {
    err.chain().any(|link| match link.downcast_ref::<zbus::Error>() {
        Some(zbus::Error::MethodError(name, _, _)) => {
            name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown"
        }
        _ => false,
    })
}

/// Exit status for a failure: 2 usage, 3 device selection, 4 unsupported, 5 device I/O, 1 other.
pub fn exit_code(err: &anyhow::Error) -> u8 {
    if err.downcast_ref::<CliError>().is_none() && daemon_is_absent(err) {
        /* Same class as "no devices": the CLI could not find anything to talk to. */
        return 3;
    }
    err.downcast_ref::<CliError>().map_or(1, CliError::exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn change_shows_old_and_new() {
        let change = Change::new("LED 0", "mode", "breathing").from("solid");
        assert_eq!(change.render(Style::plain()), "[ok] LED 0 mode: solid -> breathing");
    }

    #[test]
    fn change_without_previous_value_is_one_sided() {
        let change = Change::new("Button 4", "mapping", "key a (KEY_A, 30)");
        assert_eq!(change.render(Style::plain()), "[ok] Button 4 mapping: key a (KEY_A, 30)");
    }

    #[test]
    fn change_json_is_machine_readable() {
        let change = Change::new("LED 0", "brightness", "200").from("120");
        let json = change.to_json();
        assert_eq!(json["ok"], serde_json::json!(true));
        assert_eq!(json["set"]["property"], serde_json::json!("brightness"));
        assert_eq!(json["set"]["from"], serde_json::json!("120"));
        assert_eq!(json["set"]["to"], serde_json::json!("200"));
    }

    #[test]
    fn swatch_is_dropped_when_color_is_off() {
        let change = Change::new("LED 0", "color", "#ff0000").swatch(Rgb::new(255, 0, 0));
        assert_eq!(change.render(Style::plain()), "[ok] LED 0 color: #ff0000");
    }

    #[test]
    fn hint_wrapping_keeps_the_message_and_the_cause() {
        let root = anyhow::anyhow!("Set Mode failed").context("cannot set LED 0 mode");
        let err: anyhow::Error = Err::<(), _>(root)
            .hint("run `ratbagctl led show`")
            .unwrap_err();

        /* Transparent Display: the wrapper shows the message it wrapped. */
        assert_eq!(err.to_string(), "cannot set LED 0 mode");

        let chain: Vec<String> = err.chain().map(ToString::to_string).collect();
        assert_eq!(chain[0], "cannot set LED 0 mode");
        /* The wrapped error is not repeated: the next link is its own cause. */
        assert_eq!(chain[1], "Set Mode failed");
        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn hint_if_only_attaches_when_asked() {
        let quiet: anyhow::Error =
            Err::<(), _>(anyhow::anyhow!("boom")).hint_if(false, "advice").unwrap_err();
        assert!(quiet.chain().all(|link| link.downcast_ref::<Hinted>().is_none()));

        let loud: anyhow::Error =
            Err::<(), _>(anyhow::anyhow!("boom")).hint_if(true, "advice").unwrap_err();
        assert!(loud.chain().any(|link| link.downcast_ref::<Hinted>().is_some()));
    }

    /// A `MethodError` as zbus delivers it when nothing owns the bus name.
    fn service_unknown() -> anyhow::Result<zbus::Error> {
        let message = zbus::message::Message::method_call("/", "Whatever")?.build(&())?;
        Ok(zbus::Error::MethodError(
            "org.freedesktop.DBus.Error.ServiceUnknown".try_into()?,
            Some("The name org.freedesktop.ratbag1 was not provided by any .service files".into()),
            message,
        ))
    }

    #[test]
    fn a_missing_daemon_is_recognised_and_explained() {
        let Ok(dbus) = service_unknown() else { panic!("the fixture error should build") };
        let err = anyhow::Error::new(dbus).context("Get Manager.Devices failed");

        assert!(daemon_is_absent(&err));
        assert_eq!(exit_code(&err), 3);
        assert!(!daemon_is_absent(&anyhow::anyhow!("unrelated")));
    }

    #[test]
    fn exit_codes_come_from_the_domain_error_through_context() {
        let err = anyhow::Error::new(CliError::NoDevices).context("cannot list devices");
        assert_eq!(exit_code(&err), 3);
        assert_eq!(exit_code(&anyhow::anyhow!("something else")), 1);
    }
}
