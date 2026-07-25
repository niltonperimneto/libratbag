/* Read views.
 *
 * Every view is a plain data struct that knows how to render itself two ways: `human()` for a
 * terminal — aligned columns, semantic colour, LED swatches — and serde for `--json`. Keeping both
 * behind one struct is what stops the two from drifting, and keeps the command modules free of
 * formatting.
 *
 * Colour is always applied through `Style`, so a plain Style yields exactly the same text a pipe
 * would receive. */

use serde::Serialize;

use crate::codes::{self, LedModeArg};
use crate::style::{Cell, Ink, Style, Table, Tag};

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// A label/value block, e.g. the body of `led show`. Labels are dimmed and right-padded so the
/// values line up.
#[derive(Default)]
pub struct Fields {
    rows: Vec<(String, String)>,
}

impl Fields {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, label: &str, value: impl AsRef<str>) {
        self.rows.push((label.to_string(), value.as_ref().to_string()));
    }

    /// Add only when there is something to say.
    pub fn add_if(&mut self, condition: bool, label: &str, value: impl AsRef<str>) {
        if condition {
            self.add(label, value);
        }
    }

    pub fn render(&self, style: Style, indent: &str) -> String {
        let width = self.rows.iter().map(|(label, _)| label.chars().count()).max().unwrap_or(0);
        self.rows
            .iter()
            .map(|(label, value)| {
                let pad = width.saturating_sub(label.chars().count());
                let padded = format!("{label}:{:pad$}", "", pad = pad);
                format!("{indent}{} {value}", style.dim(&padded))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// `400, 800, 1600` — a list of supported values, or nothing at all when the device is silent.
fn join_numbers(values: &[u32]) -> String {
    values.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
}

/// The same list, condensed when it is long.
///
/// Mice routinely advertise every 100-DPI step from 100 to 16000. Printing 160 numbers buries
/// whatever the user was actually looking at, so an evenly spaced run is described by its range
/// and step instead, and any other long list is truncated.
pub fn summarize_numbers(values: &[u32]) -> String {
    let (Some(first), Some(last)) = (values.first(), values.last()) else {
        return String::new();
    };
    if values.len() <= 12 {
        return join_numbers(values);
    }

    let step = values.get(1).map_or(0, |second| second.saturating_sub(*first));
    let uniform =
        step > 0 && values.windows(2).all(|pair| pair[1].saturating_sub(pair[0]) == step);
    if uniform {
        return format!("{first}-{last} in steps of {step} ({} values)", values.len());
    }
    let head = join_numbers(&values[..6]);
    format!("{head}, … {last} ({} values)", values.len())
}

/// The `[active] [dirty]` suffix for a list row, as a single cell.
fn tags_cell(style: Style, tags: &[Tag]) -> Cell {
    /* Each tag carries its own colour, so the cell is measured on the plain spelling and
     * rendered from the painted one. */
    let plain = tags.iter().map(|tag| tag.label()).collect::<Vec<_>>().join(" ");
    let painted = tags.iter().map(|tag| style.tag(*tag)).collect::<Vec<_>>().join(" ");
    Cell::painted(plain, painted)
}

/// A colour as a swatch cell followed by its hex value.
fn color_cells(style: Style, color: &str, rgb: [u8; 3]) -> Vec<Cell> {
    vec![
        Cell::raw(style.swatch(rgb[0], rgb[1], rgb[2]), style.swatch_width()),
        Cell::new(color),
    ]
}

// ---------------------------------------------------------------------------
// Device list
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct DeviceRow {
    pub index: usize,
    pub name: String,
    pub model: String,
    pub sysname: String,
    pub profiles: usize,
}

#[derive(Serialize)]
pub struct DeviceList {
    pub version: u32,
    pub api_version: i32,
    pub devices: Vec<DeviceRow>,
}

impl DeviceList {
    pub fn new(api_version: i32, devices: Vec<DeviceRow>) -> Self {
        Self { version: 1, api_version, devices }
    }

    pub fn human(&self, style: Style) -> String {
        if self.devices.is_empty() {
            return format!(
                "{}\n{}",
                style.dim("No devices found."),
                style.dim("Is ratbagd running, and is a supported mouse connected?"),
            );
        }

        let mut table = Table::new().headers(["", "NAME", "MODEL", "PROFILES"]);
        for device in &self.devices {
            table.row(vec![
                Cell::with(device.index.to_string(), Ink::Bold),
                Cell::new(&device.name),
                Cell::with(&device.model, Ink::Dim),
                Cell::with(device.profiles.to_string(), Ink::Dim),
            ]);
        }
        table.render(style)
    }
}

// ---------------------------------------------------------------------------
// Device detail
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct DeviceView {
    pub version: u32,
    pub index: usize,
    pub name: String,
    pub model: String,
    pub sysname: String,
    pub firmware_version: String,
    pub profiles: Vec<ProfileRow>,
}

impl DeviceView {
    pub fn human(&self, style: Style) -> String {
        let mut out = vec![style.bold(&self.name)];

        let mut fields = Fields::new();
        fields.add("Model", &self.model);
        fields.add("Sysname", &self.sysname);
        fields.add_if(!self.firmware_version.is_empty(), "Firmware", &self.firmware_version);
        fields.add("Profiles", self.profiles.len().to_string());
        out.push(fields.render(style, "  "));

        if !self.profiles.is_empty() {
            out.push(String::new());
            out.push(ProfileList::new(self.profiles.clone()).human(style));
        }
        out.join("\n")
    }
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct ProfileRow {
    pub index: u32,
    pub name: String,
    pub report_rate: u32,
    pub active: bool,
    pub enabled: bool,
    pub dirty: bool,
}

impl ProfileRow {
    fn tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        if self.active {
            tags.push(Tag::Active);
        }
        if !self.enabled {
            tags.push(Tag::Disabled);
        }
        if self.dirty {
            tags.push(Tag::Dirty);
        }
        tags
    }
}

#[derive(Serialize)]
pub struct ProfileList {
    pub version: u32,
    pub profiles: Vec<ProfileRow>,
}

impl ProfileList {
    pub fn new(profiles: Vec<ProfileRow>) -> Self {
        Self { version: 1, profiles }
    }

    pub fn human(&self, style: Style) -> String {
        if self.profiles.is_empty() {
            return style.dim("This device has no profiles.").to_string();
        }
        let mut table = Table::new().headers(["PROFILE", "NAME", "RATE", ""]);
        for profile in &self.profiles {
            table.row(vec![
                Cell::with(profile.index.to_string(), Ink::Bold),
                Cell::new(&profile.name),
                Cell::new(rate_text(profile.report_rate)),
                tags_cell(style, &profile.tags()),
            ]);
        }
        table.render(style)
    }
}

fn rate_text(rate: u32) -> String {
    if rate == 0 { "-".to_string() } else { format!("{rate} Hz") }
}

#[derive(Serialize)]
pub struct ProfileView {
    pub version: u32,
    #[serde(flatten)]
    pub profile: ProfileRow,
    pub supported_rates: Vec<u32>,
    pub angle_snapping: Option<bool>,
    pub debounce_ms: Option<i32>,
    pub supported_debounces: Vec<u32>,
    pub resolutions: Vec<ResolutionRow>,
    pub buttons: Vec<ButtonRow>,
    pub leds: Vec<LedRow>,
}

impl ProfileView {
    pub fn human(&self, style: Style) -> String {
        let heading = match self.profile.name.is_empty() {
            true => format!("Profile {}", self.profile.index),
            false => format!("Profile {} \"{}\"", self.profile.index, self.profile.name),
        };
        let tags = self.profile.tags();
        let mut title = style.bold(&heading);
        if !tags.is_empty() {
            let painted: Vec<String> = tags.iter().map(|tag| style.tag(*tag)).collect();
            title = format!("{title} {}", painted.join(" "));
        }
        let mut out = vec![title];

        let mut fields = Fields::new();
        fields.add("Report rate", rate_text(self.profile.report_rate));
        fields.add_if(
            !self.supported_rates.is_empty(),
            "Supported rates",
            summarize_numbers(&self.supported_rates),
        );
        if let Some(snapping) = self.angle_snapping {
            fields.add("Angle snapping", if snapping { "on" } else { "off" });
        }
        if let Some(debounce) = self.debounce_ms {
            fields.add("Debounce", format!("{debounce} ms"));
        }
        fields.add_if(
            !self.supported_debounces.is_empty(),
            "Supported debounces",
            summarize_numbers(&self.supported_debounces),
        );
        out.push(fields.render(style, "  "));

        for (label, body) in [
            ("Resolutions", ResolutionList::new(self.resolutions.clone()).human(style)),
            ("Buttons", ButtonList::new(self.buttons.clone()).human(style)),
            ("LEDs", LedList::new(self.leds.clone()).human(style)),
        ] {
            if body.is_empty() {
                continue;
            }
            out.push(String::new());
            out.push(style.bold(label));
            out.push(indent(&body, "  "));
        }
        out.join("\n")
    }
}

fn indent(text: &str, prefix: &str) -> String {
    text.lines()
        .map(|line| if line.is_empty() { line.to_string() } else { format!("{prefix}{line}") })
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Resolutions
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct ResolutionRow {
    pub index: u32,
    pub dpi: String,
    pub active: bool,
    pub default: bool,
    pub disabled: bool,
    pub supported_dpi: Vec<u32>,
    pub capabilities: Vec<&'static str>,
}

impl ResolutionRow {
    fn tags(&self) -> Vec<Tag> {
        let mut tags = Vec::new();
        if self.active {
            tags.push(Tag::Active);
        }
        if self.default {
            tags.push(Tag::Default);
        }
        if self.disabled {
            tags.push(Tag::Disabled);
        }
        tags
    }
}

#[derive(Serialize)]
pub struct ResolutionList {
    pub version: u32,
    pub resolutions: Vec<ResolutionRow>,
}

impl ResolutionList {
    pub fn new(resolutions: Vec<ResolutionRow>) -> Self {
        Self { version: 1, resolutions }
    }

    pub fn human(&self, style: Style) -> String {
        if self.resolutions.is_empty() {
            return String::new();
        }
        let mut table = Table::new().headers(["RES", "DPI", ""]);
        for resolution in &self.resolutions {
            table.row(vec![
                Cell::with(resolution.index.to_string(), Ink::Bold),
                Cell::new(&resolution.dpi),
                tags_cell(style, &resolution.tags()),
            ]);
        }
        table.render(style)
    }
}

#[derive(Serialize)]
pub struct ResolutionView {
    pub version: u32,
    #[serde(flatten)]
    pub resolution: ResolutionRow,
}

impl ResolutionView {
    pub fn human(&self, style: Style) -> String {
        let tags: Vec<String> =
            self.resolution.tags().iter().map(|tag| style.tag(*tag)).collect();
        let mut title = style.bold(&format!("Resolution {}", self.resolution.index));
        if !tags.is_empty() {
            title = format!("{title} {}", tags.join(" "));
        }

        let mut fields = Fields::new();
        fields.add("DPI", &self.resolution.dpi);
        fields.add_if(
            !self.resolution.supported_dpi.is_empty(),
            "Supported DPI",
            summarize_numbers(&self.resolution.supported_dpi),
        );
        fields.add_if(
            !self.resolution.capabilities.is_empty(),
            "Capabilities",
            self.resolution.capabilities.join(", "),
        );
        format!("{title}\n{}", fields.render(style, "  "))
    }
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct ButtonRow {
    pub index: u32,
    /// `key`, `button`, `special`, `macro`, `disabled`.
    pub action_type: &'static str,
    /// What it does, in words: `a (KEY_A, 30)`, `wheel-up`, `+leftctrl c -leftctrl`.
    pub action: String,
}

#[derive(Serialize)]
pub struct ButtonList {
    pub version: u32,
    pub buttons: Vec<ButtonRow>,
}

impl ButtonList {
    pub fn new(buttons: Vec<ButtonRow>) -> Self {
        Self { version: 1, buttons }
    }

    pub fn human(&self, style: Style) -> String {
        if self.buttons.is_empty() {
            return String::new();
        }
        let mut table = Table::new().headers(["BUTTON", "TYPE", "ACTION"]);
        for button in &self.buttons {
            let disabled = button.action_type == codes::action_type_name(codes::ACTION_NONE);
            table.row(vec![
                Cell::with(button.index.to_string(), Ink::Bold),
                Cell::with(button.action_type, if disabled { Ink::Dim } else { Ink::Plain }),
                Cell::with(&button.action, if disabled { Ink::Dim } else { Ink::Plain }),
            ]);
        }
        table.render(style)
    }
}

#[derive(Serialize)]
pub struct ButtonView {
    pub version: u32,
    #[serde(flatten)]
    pub button: ButtonRow,
    pub supported_types: Vec<&'static str>,
}

impl ButtonView {
    pub fn human(&self, style: Style) -> String {
        let title = style.bold(&format!("Button {}", self.button.index));
        let mut fields = Fields::new();
        fields.add("Type", self.button.action_type);
        fields.add("Action", &self.button.action);
        fields.add_if(
            !self.supported_types.is_empty(),
            "Supported types",
            self.supported_types.join(", "),
        );
        format!("{title}\n{}", fields.render(style, "  "))
    }
}

// ---------------------------------------------------------------------------
// LEDs
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct LedRow {
    pub index: u32,
    pub mode: &'static str,
    pub mode_code: u32,
    pub color: String,
    pub rgb: [u8; 3],
    pub brightness: u32,
    pub duration_ms: u32,
}

impl LedRow {
    /// Whether the effect actually shows the configured colour.
    fn is_lit(&self) -> bool {
        !matches!(LedModeArg::from_wire(self.mode_code), None | Some(LedModeArg::Off))
    }
}

#[derive(Serialize)]
pub struct LedList {
    pub version: u32,
    pub leds: Vec<LedRow>,
}

impl LedList {
    pub fn new(leds: Vec<LedRow>) -> Self {
        Self { version: 1, leds }
    }

    pub fn human(&self, style: Style) -> String {
        if self.leds.is_empty() {
            return String::new();
        }
        let mut table = Table::new().headers(["LED", "MODE", "", "COLOR", "BRIGHTNESS"]);
        for led in &self.leds {
            let mut row = vec![
                Cell::with(led.index.to_string(), Ink::Bold),
                Cell::new(led.mode),
            ];
            /* An unlit LED's colour is not what you see, so it is dimmed and shown without a
             * swatch that would suggest otherwise. */
            if led.is_lit() {
                row.extend(color_cells(style, &led.color, led.rgb));
            } else {
                row.push(Cell::raw(String::new(), style.swatch_width()));
                row.push(Cell::with(&led.color, Ink::Dim));
            }
            row.push(Cell::new(led.brightness.to_string()));
            table.row(row);
        }
        table.render(style)
    }
}

#[derive(Serialize)]
pub struct LedView {
    pub version: u32,
    #[serde(flatten)]
    pub led: LedRow,
    pub secondary_color: String,
    pub secondary_rgb: [u8; 3],
    pub tertiary_color: String,
    pub tertiary_rgb: [u8; 3],
    pub color_depth: &'static str,
    pub supported_modes: Vec<&'static str>,
}

impl LedView {
    pub fn human(&self, style: Style) -> String {
        let title = style.bold(&format!("LED {}", self.led.index));
        let swatch = |rgb: [u8; 3], text: &str| -> String {
            let swatch = style.swatch(rgb[0], rgb[1], rgb[2]);
            if swatch.is_empty() { text.to_string() } else { format!("{swatch} {text}") }
        };

        let mut fields = Fields::new();
        fields.add("Mode", self.led.mode);
        fields.add("Color", swatch(self.led.rgb, &self.led.color));
        /* Secondary and tertiary colours only mean something to the multi-colour effects. */
        let mode = LedModeArg::from_wire(self.led.mode_code);
        let multi = matches!(mode, Some(LedModeArg::Starlight | LedModeArg::TriColor));
        fields.add_if(multi, "Secondary", swatch(self.secondary_rgb, &self.secondary_color));
        fields.add_if(
            matches!(mode, Some(LedModeArg::TriColor)),
            "Tertiary",
            swatch(self.tertiary_rgb, &self.tertiary_color),
        );
        fields.add("Brightness", self.led.brightness.to_string());
        fields.add("Duration", format!("{} ms", self.led.duration_ms));
        fields.add("Color depth", self.color_depth);
        fields.add_if(
            !self.supported_modes.is_empty(),
            "Supported modes",
            self.supported_modes.join(", "),
        );
        format!("{title}\n{}", fields.render(style, "  "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(index: u32, active: bool) -> ProfileRow {
        ProfileRow {
            index,
            name: format!("Profile {index}"),
            report_rate: 1000,
            active,
            enabled: true,
            dirty: false,
        }
    }

    #[test]
    fn empty_device_list_explains_itself() {
        let list = DeviceList::new(1, Vec::new());
        let out = list.human(Style::plain());
        assert!(out.contains("No devices found."));
        assert!(out.contains("ratbagd"));
    }

    #[test]
    fn device_list_is_a_plain_table_without_color() {
        let list = DeviceList::new(1, vec![DeviceRow {
            index: 0,
            name: "Logitech G502".to_string(),
            model: "usb:046d:c08b".to_string(),
            sysname: "event3".to_string(),
            profiles: 3,
        }]);
        let out = list.human(Style::plain());
        assert!(!out.contains('\x1b'), "plain output must carry no escapes: {out:?}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        /* Each value sits under its own header. */
        for (header, value) in [("NAME", "Logitech G502"), ("MODEL", "usb:046d:c08b")] {
            assert_eq!(
                lines[0].find(header),
                lines[1].find(value),
                "{value} should sit under {header}:\n{out}",
            );
        }
        assert!(lines[1].starts_with('0'));
        assert!(lines[1].ends_with('3'), "the profile count ends the row: {out:?}");
    }

    #[test]
    fn profile_tags_appear_in_the_last_column() {
        let list = ProfileList::new(vec![profile(0, true), profile(1, false)]);
        let out = list.human(Style::plain());
        assert!(out.lines().nth(1).is_some_and(|line| line.ends_with("[active]")));
        assert!(out.lines().nth(2).is_some_and(|line| !line.contains("[active]")));
    }

    #[test]
    fn disabled_and_dirty_profiles_are_tagged() {
        let mut row = profile(0, false);
        row.enabled = false;
        row.dirty = true;
        let out = ProfileList::new(vec![row]).human(Style::plain());
        assert!(out.contains("[disabled]"));
        assert!(out.contains("[dirty]"));
    }

    fn led_row(mode: LedModeArg) -> LedRow {
        LedRow {
            index: 0,
            mode: mode.label(),
            mode_code: mode.wire(),
            color: "#ff0000".to_string(),
            rgb: [255, 0, 0],
            brightness: 200,
            duration_ms: 0,
        }
    }

    #[test]
    fn led_rows_keep_the_swatch_column_when_color_is_off() {
        let out = LedList::new(vec![led_row(LedModeArg::Solid)]).human(Style::plain());
        assert!(!out.contains('\x1b'));
        assert!(out.contains("#ff0000"));
    }

    #[test]
    fn an_unlit_led_gets_no_swatch() {
        let styled = LedList::new(vec![led_row(LedModeArg::Off)]).human(Style::plain());
        assert!(styled.contains("off"));
        assert!(!led_row(LedModeArg::Off).is_lit());
        assert!(led_row(LedModeArg::Breathing).is_lit());
    }

    #[test]
    fn led_detail_hides_colors_the_mode_ignores() {
        let view = LedView {
            version: 1,
            led: led_row(LedModeArg::Solid),
            secondary_color: "#00ff00".to_string(),
            secondary_rgb: [0, 255, 0],
            tertiary_color: "#0000ff".to_string(),
            tertiary_rgb: [0, 0, 255],
            color_depth: "rgb",
            supported_modes: vec!["off", "solid"],
        };
        let solid = view.human(Style::plain());
        assert!(!solid.contains("Secondary"), "solid mode has no secondary colour");

        let tricolor = LedView { led: led_row(LedModeArg::TriColor), ..view };
        let out = tricolor.human(Style::plain());
        assert!(out.contains("Secondary"));
        assert!(out.contains("Tertiary"));
    }

    #[test]
    fn fields_align_on_the_value_column() {
        let mut fields = Fields::new();
        fields.add("Mode", "solid");
        fields.add("Brightness", "200");
        let out = fields.render(Style::plain(), "  ");
        let columns: Vec<Option<usize>> =
            out.lines().map(|line| line.rfind(' ').map(|index| index + 1)).collect();
        assert_eq!(columns[0], columns[1]);
    }

    #[test]
    fn json_views_carry_a_version_and_flatten_their_row() {
        let view = ButtonView {
            version: 1,
            button: ButtonRow { index: 4, action_type: "key", action: "a (KEY_A, 30)".to_string() },
            supported_types: vec!["key", "button"],
        };
        let json = serde_json::to_value(&view).unwrap_or_default();
        assert_eq!(json["version"], serde_json::json!(1));
        assert_eq!(json["index"], serde_json::json!(4));
        assert_eq!(json["action_type"], serde_json::json!("key"));
    }

    #[test]
    fn supported_value_lists_are_readable_not_debug_printed() {
        assert_eq!(join_numbers(&[400, 800, 1600]), "400, 800, 1600");
        assert_eq!(join_numbers(&[]), "");
    }

    #[test]
    fn short_value_lists_are_shown_in_full() {
        assert_eq!(summarize_numbers(&[]), "");
        assert_eq!(summarize_numbers(&[1000]), "1000");
        assert_eq!(summarize_numbers(&[125, 250, 500, 1000]), "125, 250, 500, 1000");
    }

    #[test]
    fn a_long_even_range_is_described_by_its_step() {
        /* What a real mouse reports: every 100 DPI from 100 to 16000. */
        let dpis: Vec<u32> = (1..=160).map(|step| step * 100).collect();
        assert_eq!(summarize_numbers(&dpis), "100-16000 in steps of 100 (160 values)");
    }

    #[test]
    fn a_long_uneven_list_is_truncated() {
        let mut values: Vec<u32> = (1..=20).map(|step| step * 100).collect();
        values.push(50_000);
        let summary = summarize_numbers(&values);
        assert!(summary.starts_with("100, 200, 300, 400, 500, 600, … 50000"), "{summary}");
        assert!(summary.ends_with("(21 values)"), "{summary}");
    }
}
