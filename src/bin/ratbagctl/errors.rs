/* Domain errors for ratbagctl.
 *
 * These are the failures where a bare message is not enough: the useful part is the *data* the
 * user needs next — which devices are connected, which LED modes this device supports, which DPI
 * steps are valid. Each variant carries that data and renders it through `hints()`, which
 * report::render_error prints under the error itself.
 *
 * Everything else stays plain anyhow with a `Hint` attached at the call site (see report::Hint);
 * a variant here is only warranted when the hint needs values read off the device. */

use crate::style::Style;

/// One connected device, as shown in a candidate list.
#[derive(Clone, Debug)]
pub struct DeviceChoice {
    pub index: usize,
    pub name: String,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("no devices are connected")]
    NoDevices,

    #[error("more than one device is connected, so there is no obvious one to act on")]
    AmbiguousDefault { candidates: Vec<DeviceChoice> },

    #[error("no connected device matches '{spec}'")]
    NoSuchDevice { spec: String, candidates: Vec<DeviceChoice> },

    #[error("'{spec}' matches {} devices", candidates.len())]
    AmbiguousDevice { spec: String, candidates: Vec<DeviceChoice> },

    #[error("device index {index} is out of range (this system has {count})")]
    DeviceIndexOutOfRange { index: usize, count: usize },

    #[error("profile {index} does not exist on this device")]
    NoSuchProfile { index: u32, count: usize },

    #[error("this command needs to know which button to act on")]
    NoButtonSelected,

    #[error("LED {index} does not support {requested} mode")]
    UnsupportedLedMode { index: u32, requested: &'static str, supported: Vec<&'static str> },

    #[error("{what} is not supported on this device")]
    NotSupported { what: String },

    /// `supported` is already formatted for display: a long evenly spaced range is condensed by
    /// [`crate::render::summarize_numbers`] rather than printed in full.
    #[error("{value} is not a supported {what} on this device")]
    UnsupportedValue { what: String, value: String, supported: String },

    #[error("the daemon rejected the commit (error code {code})")]
    CommitFailed { code: i32 },
}

impl CliError {
    /// Advice printed under the error, most useful first.
    pub fn hints(&self, style: Style) -> Vec<String> {
        match self {
            Self::NoDevices => vec![
                "is ratbagd running? check with `systemctl status ratbagd`".to_string(),
                "a supported mouse also has to be plugged in".to_string(),
            ],
            Self::AmbiguousDefault { candidates } => {
                let mut hints =
                    vec!["choose one with -d/--device, by index, name or sysname:".to_string()];
                hints.extend(list_devices(style, candidates));
                hints
            }
            Self::NoSuchDevice { candidates, .. } => {
                if candidates.is_empty() {
                    vec!["no devices are connected at all".to_string()]
                } else {
                    let mut hints = vec!["these devices are connected:".to_string()];
                    hints.extend(list_devices(style, candidates));
                    hints
                }
            }
            Self::AmbiguousDevice { candidates, .. } => {
                let mut hints = vec!["be more specific, or use the index:".to_string()];
                hints.extend(list_devices(style, candidates));
                hints
            }
            Self::DeviceIndexOutOfRange { count, .. } => {
                vec![format!("valid indices are 0-{}; run `ratbagctl list` to see them", count.saturating_sub(1))]
            }
            Self::NoSuchProfile { count, .. } => vec![
                format!("this device has {count} profile(s), numbered 0-{}", count.saturating_sub(1)),
                "run `ratbagctl profile list` to see them".to_string(),
            ],
            Self::NoButtonSelected => vec![
                "name the button first, as in `ratbagctl button 4 key a`".to_string(),
                "run `ratbagctl button list` to see the buttons on this device".to_string(),
            ],
            Self::UnsupportedLedMode { supported, .. } => vec![
                format!("this LED supports: {}", supported.join(", ")),
                "run `ratbagctl led show` for its full capabilities".to_string(),
            ],
            Self::NotSupported { .. } => {
                vec!["run `ratbagctl show` to see what this device exposes".to_string()]
            }
            Self::UnsupportedValue { what, supported, .. } => {
                if supported.is_empty() {
                    vec![format!("this device does not advertise the {what} values it accepts")]
                } else {
                    vec![format!("supported: {supported}")]
                }
            }
            Self::CommitFailed { .. } => vec![
                "the change was staged but not written to the device".to_string(),
                "unplugging and reconnecting the device usually clears this".to_string(),
            ],
        }
    }

    /// Distinct exit codes so scripts can tell the classes of failure apart.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::NoDevices
            | Self::NoSuchDevice { .. }
            | Self::AmbiguousDevice { .. }
            | Self::AmbiguousDefault { .. }
            | Self::DeviceIndexOutOfRange { .. }
            | Self::NoSuchProfile { .. } => 3,
            Self::NoButtonSelected => 2,
            Self::UnsupportedLedMode { .. }
            | Self::NotSupported { .. }
            | Self::UnsupportedValue { .. } => 4,
            Self::CommitFailed { .. } => 5,
        }
    }
}

/* Candidate lists are indented under the hint and aligned on the name column. */
fn list_devices(style: Style, candidates: &[DeviceChoice]) -> Vec<String> {
    let width = candidates.iter().map(|choice| choice.name.chars().count()).max().unwrap_or(0);
    candidates
        .iter()
        .map(|choice| {
            format!(
                "  {}  {:width$}  {}",
                style.bold(&choice.index.to_string()),
                choice.name,
                style.dim(&choice.model),
                width = width,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> Vec<DeviceChoice> {
        vec![
            DeviceChoice { index: 0, name: "ROCCAT Kone Pro".into(), model: "usb:1e7d:2c8e".into() },
            DeviceChoice { index: 1, name: "Logitech G502".into(), model: "usb:046d:c08b".into() },
        ]
    }

    #[test]
    fn messages_name_the_problem() {
        let err = CliError::NoSuchDevice { spec: "g9".into(), candidates: choices() };
        assert_eq!(err.to_string(), "no connected device matches 'g9'");

        let err = CliError::AmbiguousDevice { spec: "usb".into(), candidates: choices() };
        assert_eq!(err.to_string(), "'usb' matches 2 devices");

        let err = CliError::UnsupportedLedMode {
            index: 0,
            requested: "tricolor",
            supported: vec!["off", "solid"],
        };
        assert_eq!(err.to_string(), "LED 0 does not support tricolor mode");
    }

    #[test]
    fn hints_list_the_candidates_aligned() {
        let err = CliError::AmbiguousDefault { candidates: choices() };
        let hints = err.hints(Style::plain());
        assert_eq!(hints[0], "choose one with -d/--device, by index, name or sysname:");
        assert_eq!(hints[1], "  0  ROCCAT Kone Pro  usb:1e7d:2c8e");
        assert_eq!(hints[2], "  1  Logitech G502    usb:046d:c08b");
    }

    #[test]
    fn hints_report_supported_values() {
        let err = CliError::UnsupportedLedMode {
            index: 1,
            requested: "starlight",
            supported: vec!["off", "solid", "cycle"],
        };
        assert!(err.hints(Style::plain())[0].contains("off, solid, cycle"));

        let err = CliError::UnsupportedValue {
            what: "DPI".into(),
            value: "1234".into(),
            supported: "400, 800".into(),
        };
        assert_eq!(err.hints(Style::plain())[0], "supported: 400, 800");

        let err = CliError::UnsupportedValue {
            what: "report rate".into(),
            value: "2000".into(),
            supported: String::new(),
        };
        assert!(err.hints(Style::plain())[0].contains("does not advertise"));
    }

    #[test]
    fn failure_classes_have_distinct_exit_codes() {
        assert_eq!(CliError::NoDevices.exit_code(), 3);
        assert_eq!(CliError::NoButtonSelected.exit_code(), 2);
        assert_eq!(CliError::NotSupported { what: "debounce".into() }.exit_code(), 4);
        assert_eq!(CliError::CommitFailed { code: -1 }.exit_code(), 5);
    }
}
