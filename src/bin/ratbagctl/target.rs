/* Selector resolution and output plumbing.
 *
 * `Cx` turns the command line's selectors (-d/-p/-r/-l/-b, each with a sensible default) into DBus
 * object paths, and is the single place that decides how a line reaches the user: styled text, a
 * bare value, or JSON. Command modules take a `&Cx` and never format escapes or build paths
 * themselves.
 *
 * Resolution is lazy and memoised: `ratbagctl list` never resolves a device, and a command that
 * touches an LED resolves the device and profile exactly once. */

use anyhow::Result;
use tokio::sync::OnceCell;

use crate::dbus_client::{RatbagClient, ResolvedDevice};
use crate::errors::CliError;
use crate::render;
use crate::report::Change;
use crate::style::Style;

/// Which objects to act on, after merging a group's leading index over the global flags.
#[derive(Clone, Debug, Default)]
pub struct Selectors {
    pub device: Option<String>,
    pub profile: Option<u32>,
    pub resolution: Option<u32>,
    pub led: Option<u32>,
    pub button: Option<u32>,
}

/// A resolved object: its DBus path and its index.
#[derive(Clone, Debug)]
pub struct Target {
    pub path: String,
    pub index: u32,
}

pub struct Cx {
    pub client: RatbagClient,
    /// Style for stdout.
    pub out: Style,
    pub json: bool,
    selectors: Selectors,
    device: OnceCell<ResolvedDevice>,
    profile: OnceCell<Target>,
}

impl Cx {
    pub fn new(client: RatbagClient, out: Style, json: bool, selectors: Selectors) -> Self {
        Self {
            client,
            out,
            json,
            selectors,
            device: OnceCell::new(),
            profile: OnceCell::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Selector resolution
    // -----------------------------------------------------------------------

    /// The device to act on, resolved once per run.
    pub async fn device(&self) -> Result<&ResolvedDevice> {
        self.device
            .get_or_try_init(|| self.client.resolve_device(self.selectors.device.as_deref()))
            .await
    }

    pub async fn device_path(&self) -> Result<&str> {
        Ok(self.device().await?.path.as_str())
    }

    /// The profile to act on: `-p`, else the profile the device reports as active.
    pub async fn profile(&self) -> Result<&Target> {
        self.profile.get_or_try_init(|| self.resolve_profile()).await
    }

    async fn resolve_profile(&self) -> Result<Target> {
        let device_path = self.device_path().await?;
        let profiles = self.client.get_device_profiles(device_path).await?;
        if profiles.is_empty() {
            return Err(CliError::NoSuchProfile { index: 0, count: 0 }.into());
        }

        if let Some(index) = self.selectors.profile {
            let wanted = format!("/p{index}");
            return match profiles.iter().find(|path| path.ends_with(&wanted)) {
                Some(path) => Ok(Target { path: path.clone(), index }),
                None => {
                    Err(CliError::NoSuchProfile { index, count: profiles.len() }.into())
                }
            };
        }

        /* Default to the active profile. */
        for path in &profiles {
            if self.client.get_profile_is_active(path).await.unwrap_or(false) {
                let index = self.client.get_profile_index(path).await?;
                return Ok(Target { path: path.clone(), index });
            }
        }

        /* No profile claims to be active: fall back to the first one rather than failing. */
        let index = self.client.get_profile_index(&profiles[0]).await?;
        Ok(Target { path: profiles[0].clone(), index })
    }

    /// The resolution to act on: `-r` or a group index, else the active resolution.
    pub async fn resolution(&self) -> Result<Target> {
        let profile = self.profile().await?;
        let resolutions = self.client.get_profile_resolutions(&profile.path).await?;
        if resolutions.is_empty() {
            return Err(CliError::NotSupported {
                what: format!("profile {} has no resolutions", profile.index),
            }
            .into());
        }

        if let Some(index) = self.selectors.resolution {
            return pick(&resolutions, 'r', index, "resolution");
        }

        for path in &resolutions {
            if self.client.get_resolution_is_active(path).await.unwrap_or(false) {
                let index = self.client.get_resolution_index(path).await?;
                return Ok(Target { path: path.clone(), index });
            }
        }

        let index = self.client.get_resolution_index(&resolutions[0]).await?;
        Ok(Target { path: resolutions[0].clone(), index })
    }

    /// The LED to act on: `-l` or a group index, else LED 0.
    pub async fn led(&self) -> Result<Target> {
        let profile = self.profile().await?;
        let leds = self.client.get_profile_leds(&profile.path).await?;
        if leds.is_empty() {
            return Err(CliError::NotSupported { what: "lighting".to_string() }.into());
        }
        pick(&leds, 'l', self.selectors.led.unwrap_or(0), "LED")
    }

    /// The button to act on. There is no sensible default, so this fails if none was named.
    pub async fn button(&self) -> Result<Target> {
        let Some(index) = self.selectors.button else {
            return Err(CliError::NoButtonSelected.into());
        };
        let profile = self.profile().await?;
        let buttons = self.client.get_profile_buttons(&profile.path).await?;
        if buttons.is_empty() {
            return Err(CliError::NotSupported { what: "button remapping".to_string() }.into());
        }
        pick(&buttons, 'b', index, "button")
    }

    /// Write staged changes to the device, as every write command does.
    pub async fn commit(&self) -> Result<()> {
        let device_path = self.device_path().await?;
        let code = self.client.commit_device(device_path).await?;
        if code != 0 {
            /* The daemon returns the errno-style code from the driver. */
            return Err(CliError::CommitFailed { code: code as i32 }.into());
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Output
    // -----------------------------------------------------------------------

    /// Report a write, showing `old → new` when the previous value was read.
    pub fn changed(&self, change: &Change) {
        if self.json {
            self.emit_json(&change.to_json());
        } else {
            println!("{}", change.render(self.out));
        }
    }

    /// Report that a write was not needed. Nothing was sent to the device.
    pub fn unchanged(&self, object: &str, property: &str, value: &str) {
        if self.json {
            self.emit_json(&serde_json::json!({
                "version": 1,
                "ok": true,
                "unchanged": { "object": object, "property": property, "value": value },
            }));
        } else {
            println!(
                "{} {} {} {} {}",
                self.out.info_mark(),
                self.out.bold(object),
                self.out.dim(property),
                self.out.dim("is already"),
                self.out.accent(value),
            );
        }
    }

    /// A single value in answer to a read, kept bare so `$(…)` capture stays useful.
    pub fn value(&self, value: &str) {
        self.reading(value, &[]);
    }

    /// A read, plus the numeric values the device would also accept.
    ///
    /// The value stays on a line of its own so it can be captured; the alternatives follow as a
    /// dimmed aside, condensed when the device advertises a long evenly spaced range. Under
    /// `--json` both arrive in one object, with the full list rather than the summary.
    pub fn reading_numbers(&self, value: &str, supported: &[u32]) {
        if self.json {
            self.emit_json(&serde_json::json!({
                "version": 1,
                "value": value,
                "supported": supported,
            }));
            return;
        }
        println!("{value}");
        if !supported.is_empty() {
            let summary = render::summarize_numbers(supported);
            println!("{}", self.out.dim(&format!("supported: {summary}")));
        }
    }

    /// A read whose alternatives are names rather than numbers.
    pub fn reading(&self, value: &str, supported: &[String]) {
        if self.json {
            self.emit_json(&serde_json::json!({
                "version": 1,
                "value": value,
                "supported": supported,
            }));
            return;
        }
        println!("{value}");
        if !supported.is_empty() {
            println!("{}", self.out.dim(&format!("supported: {}", supported.join(", "))));
        }
    }

    /// An informational aside, such as a setting the device does not implement.
    ///
    /// Not an error: reading an unsupported property is a legitimate question with a legitimate
    /// answer, so this leaves the exit status at 0.
    pub fn note(&self, text: &str) {
        if self.json {
            self.emit_json(&serde_json::json!({ "version": 1, "note": text }));
        } else {
            println!("{} {}", self.out.info_mark(), self.out.dim(text));
        }
    }

    /// A rendered multi-line view.
    pub fn block(&self, text: &str) {
        if !text.is_empty() {
            println!("{text}");
        }
    }

    /// A structured view under `--json`.
    pub fn emit_json(&self, value: &serde_json::Value) {
        match serde_json::to_string_pretty(value) {
            Ok(text) => println!("{text}"),
            /* Serialising our own view structs cannot realistically fail, but a panic here
             * would be indefensible. */
            Err(err) => eprintln!("{} {err}", self.out.bad("error: cannot encode JSON:")),
        }
    }
}

/* Pick the child whose path ends in the expected `/<letter><index>` segment. The daemon names
 * child objects after their index, so this needs no extra round trips. */
fn pick(paths: &[String], letter: char, index: u32, what: &str) -> Result<Target> {
    let wanted = format!("/{letter}{index}");
    match paths.iter().find(|path| path.ends_with(&wanted)) {
        Some(path) => Ok(Target { path: path.clone(), index }),
        None => Err(CliError::UnsupportedValue {
            what: format!("{what} index"),
            value: index.to_string(),
            supported: format!("0-{}", paths.len().saturating_sub(1)),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_matches_on_the_index_segment() {
        let paths = vec![
            "/org/freedesktop/ratbag1/device/event3/p0/r0".to_string(),
            "/org/freedesktop/ratbag1/device/event3/p0/r1".to_string(),
        ];
        let target = pick(&paths, 'r', 1, "resolution");
        assert!(matches!(target, Ok(Target { index: 1, .. })));
        assert!(pick(&paths, 'r', 2, "resolution").is_err());
    }

    #[test]
    fn pick_does_not_confuse_similar_indices() {
        /* `/r1` must not match `/r11`. */
        let paths = vec!["/dev/p0/r11".to_string()];
        assert!(pick(&paths, 'r', 1, "resolution").is_err());
        assert!(pick(&paths, 'r', 11, "resolution").is_ok());
    }

    #[test]
    fn pick_reports_the_valid_range() {
        let paths = vec!["/dev/p0/l0".to_string(), "/dev/p0/l1".to_string()];
        match pick(&paths, 'l', 5, "LED") {
            Err(err) => match err.downcast_ref::<CliError>() {
                Some(CliError::UnsupportedValue { supported, .. }) => {
                    assert_eq!(supported, "0-1");
                }
                other => panic!("expected UnsupportedValue, got {other:?}"),
            },
            Ok(_) => panic!("LED 5 should not resolve"),
        }
    }
}
