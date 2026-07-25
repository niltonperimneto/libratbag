/* Command implementations.
 *
 * Each function is one leaf of the command tree. The shape is the same throughout: resolve the
 * target through `Cx`, and either report the current value (no argument given) or write a new one
 * and confirm it.
 *
 * Writes read the old value first. That costs one property get and buys three things: the
 * confirmation can show `old → new`, a write that would change nothing is skipped entirely along
 * with its commit, and a value the device does not support becomes a real error listing what it
 * does support instead of a bare DBus InvalidArgs. */

use anyhow::Result;

use crate::codes::{self, LedModeArg, SpecialArg};
use crate::dbus_client::{ButtonMapping, Dpi, sysname_of};
use crate::errors::CliError;
use crate::keys;
use crate::render::{self,
    ButtonList, ButtonRow, ButtonView, DeviceList, DeviceRow, DeviceView, LedList, LedRow, LedView,
    ProfileList, ProfileRow, ProfileView, ResolutionList, ResolutionRow, ResolutionView,
};
use crate::report::{Change, HintExt};
use crate::target::Cx;
use crate::values::{self, MacroStep, OnOff, Rgb};

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

pub async fn list(cx: &Cx) -> Result<()> {
    let api_version = cx.client.get_api_version().await.unwrap_or(-1);
    let paths = cx.client.list_devices().await?;

    let mut devices = Vec::with_capacity(paths.len());
    for (index, path) in paths.iter().enumerate() {
        let name = cx.client.get_device_name(path).await.unwrap_or_default();
        devices.push(DeviceRow {
            index,
            name: if name.is_empty() { sysname_of(path).to_string() } else { name },
            model: cx.client.get_device_model(path).await.unwrap_or_default(),
            sysname: sysname_of(path).to_string(),
            profiles: cx.client.get_device_profiles(path).await.unwrap_or_default().len(),
        });
    }

    let view = DeviceList::new(api_version, devices);
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn show(cx: &Cx) -> Result<()> {
    let device = cx.device().await?;
    let view = DeviceView {
        version: 1,
        index: device.index,
        name: cx.client.get_device_name(&device.path).await?,
        model: cx.client.get_device_model(&device.path).await?,
        sysname: sysname_of(&device.path).to_string(),
        firmware_version: cx.client.get_device_firmware(&device.path).await.unwrap_or_default(),
        profiles: profile_rows(cx, &device.path).await?,
    };
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn commit(cx: &Cx) -> Result<()> {
    cx.commit().await?;
    cx.changed(&Change::new("Device", "state", "committed to hardware"));
    Ok(())
}

pub async fn test_load_device(cx: &Cx, json_file: &str) -> Result<()> {
    let json = std::fs::read_to_string(json_file)
        .hint("the file must contain a test-device description in JSON")
        .map_err(|err| err.context(format!("cannot read '{json_file}'")))?;
    let path = cx
        .client
        .load_test_device(&json)
        .await
        .hint("the daemon must be built with --features dev-hooks")?;
    cx.changed(&Change::new("Test device", "loaded", path));
    Ok(())
}

pub async fn test_reset(cx: &Cx) -> Result<()> {
    cx.client
        .reset_test_device()
        .await
        .hint("the daemon must be built with --features dev-hooks")?;
    cx.changed(&Change::new("Test devices", "state", "all removed"));
    Ok(())
}

// ---------------------------------------------------------------------------
// Profiles
// ---------------------------------------------------------------------------

pub async fn profile_list(cx: &Cx) -> Result<()> {
    let device = cx.device().await?;
    let view = ProfileList::new(profile_rows(cx, &device.path).await?);
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn profile_show(cx: &Cx) -> Result<()> {
    let profile = cx.profile().await?;
    let angle = cx.client.get_profile_angle_snapping(&profile.path).await.unwrap_or(-1);
    let debounce = cx.client.get_profile_debounce(&profile.path).await.unwrap_or(-1);

    let view = ProfileView {
        version: 1,
        profile: profile_row(cx, &profile.path).await?,
        supported_rates: cx.client.get_profile_report_rates(&profile.path).await.unwrap_or_default(),
        /* A negative value is the API's way of saying "this device has no such setting". */
        angle_snapping: (angle >= 0).then_some(angle == 1),
        debounce_ms: (debounce >= 0).then_some(debounce),
        supported_debounces: cx.client.get_profile_debounces(&profile.path).await.unwrap_or_default(),
        resolutions: resolution_rows(cx, &profile.path).await?,
        buttons: button_rows(cx, &profile.path).await?,
        leds: led_rows(cx, &profile.path).await?,
    };
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn profile_activate(cx: &Cx) -> Result<()> {
    let profile = cx.profile().await?;
    let object = format!("Profile {}", profile.index);
    if cx.client.get_profile_is_active(&profile.path).await.unwrap_or(false) {
        cx.unchanged(&object, "state", "active");
        return Ok(());
    }
    cx.client
        .call_profile_set_active(&profile.path)
        .await
        .hint("a disabled profile cannot be activated; enable it first")?;
    cx.commit().await?;
    cx.changed(&Change::new(object, "state", "active"));
    Ok(())
}

pub async fn profile_name(cx: &Cx, name: Option<String>) -> Result<()> {
    let profile = cx.profile().await?;
    let current = cx.client.get_profile_name(&profile.path).await.unwrap_or_default();

    let Some(name) = name else {
        if current.is_empty() {
            cx.note(&format!("profile {} has no name set", profile.index));
        } else {
            cx.value(&current);
        }
        return Ok(());
    };

    let object = format!("Profile {}", profile.index);
    if current == name {
        cx.unchanged(&object, "name", &name);
        return Ok(());
    }
    cx.client.set_profile_name(&profile.path, &name).await?;
    cx.commit().await?;
    let mut change = Change::new(object, "name", &name);
    if !current.is_empty() {
        change = change.from(current);
    }
    cx.changed(&change);
    Ok(())
}

pub async fn profile_set_enabled(cx: &Cx, enabled: bool) -> Result<()> {
    let profile = cx.profile().await?;
    let object = format!("Profile {}", profile.index);
    let currently_enabled = !cx.client.get_profile_disabled(&profile.path).await?;
    let label = if enabled { "enabled" } else { "disabled" };

    if currently_enabled == enabled {
        cx.unchanged(&object, "state", label);
        return Ok(());
    }
    cx.client
        .set_profile_disabled(&profile.path, !enabled)
        .await
        .hint_if(!enabled, "the active profile usually cannot be disabled; switch away first")?;
    cx.commit().await?;
    cx.changed(
        &Change::new(object, "state", label)
            .from(if currently_enabled { "enabled" } else { "disabled" }),
    );
    Ok(())
}

pub async fn profile_rate(cx: &Cx, rate: Option<u32>) -> Result<()> {
    let profile = cx.profile().await?;
    let current = cx.client.get_profile_report_rate(&profile.path).await?;
    let supported = cx.client.get_profile_report_rates(&profile.path).await.unwrap_or_default();

    let Some(rate) = rate else {
        cx.reading_numbers(&current.to_string(), &supported);
        return Ok(());
    };

    let object = format!("Profile {}", profile.index);
    if !supported.is_empty() && !supported.contains(&rate) {
        return Err(CliError::UnsupportedValue {
            what: "report rate".to_string(),
            value: format!("{rate} Hz"),
            supported: render::summarize_numbers(&supported),
        }
        .into());
    }
    if current == rate {
        cx.unchanged(&object, "report rate", &format!("{rate} Hz"));
        return Ok(());
    }
    cx.client.set_profile_report_rate(&profile.path, rate).await?;
    cx.commit().await?;
    cx.changed(
        &Change::new(object, "report rate", format!("{rate} Hz")).from(format!("{current} Hz")),
    );
    Ok(())
}

pub async fn profile_angle_snapping(cx: &Cx, value: Option<OnOff>) -> Result<()> {
    let profile = cx.profile().await?;
    let current = cx.client.get_profile_angle_snapping(&profile.path).await?;
    if current < 0 {
        return unsupported(cx, "angle snapping", value.is_some());
    }

    let Some(value) = value else {
        cx.value(OnOff::from_bool(current == 1).label());
        return Ok(());
    };

    let object = format!("Profile {}", profile.index);
    if (current == 1) == value.as_bool() {
        cx.unchanged(&object, "angle snapping", value.label());
        return Ok(());
    }
    cx.client
        .set_profile_angle_snapping(&profile.path, i32::from(value.as_bool()))
        .await?;
    cx.commit().await?;
    cx.changed(
        &Change::new(object, "angle snapping", value.label())
            .from(OnOff::from_bool(current == 1).label()),
    );
    Ok(())
}

pub async fn profile_debounce(cx: &Cx, ms: Option<i32>) -> Result<()> {
    let profile = cx.profile().await?;
    let current = cx.client.get_profile_debounce(&profile.path).await?;
    if current < 0 {
        return unsupported(cx, "debounce", ms.is_some());
    }
    let supported = cx.client.get_profile_debounces(&profile.path).await.unwrap_or_default();

    let Some(ms) = ms else {
        cx.reading_numbers(&current.to_string(), &supported);
        return Ok(());
    };

    let object = format!("Profile {}", profile.index);
    if !supported.is_empty() && !u32::try_from(ms).is_ok_and(|ms| supported.contains(&ms)) {
        return Err(CliError::UnsupportedValue {
            what: "debounce time".to_string(),
            value: format!("{ms} ms"),
            supported: render::summarize_numbers(&supported),
        }
        .into());
    }
    if current == ms {
        cx.unchanged(&object, "debounce", &format!("{ms} ms"));
        return Ok(());
    }
    cx.client.set_profile_debounce(&profile.path, ms).await?;
    cx.commit().await?;
    cx.changed(&Change::new(object, "debounce", format!("{ms} ms")).from(format!("{current} ms")));
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolutions
// ---------------------------------------------------------------------------

pub async fn resolution_list(cx: &Cx) -> Result<()> {
    let profile = cx.profile().await?;
    let rows = resolution_rows(cx, &profile.path).await?;
    let view = ResolutionList::new(rows);
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else if view.resolutions.is_empty() {
        cx.note("this profile has no resolutions");
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn resolution_show(cx: &Cx) -> Result<()> {
    let resolution = cx.resolution().await?;
    let view = ResolutionView { version: 1, resolution: resolution_row(cx, &resolution.path).await? };
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn resolution_dpi(cx: &Cx, dpi: Option<Dpi>) -> Result<()> {
    let resolution = cx.resolution().await?;
    let current = cx.client.get_resolution_dpi(&resolution.path).await?;
    let supported = cx.client.get_resolution_dpi_list(&resolution.path).await.unwrap_or_default();

    let Some(dpi) = dpi else {
        cx.reading_numbers(&current.to_string(), &supported);
        return Ok(());
    };

    let object = format!("Resolution {}", resolution.index);
    /* The device publishes the steps it can actually do; anything else is silently clamped or
     * rejected by the hardware, so refuse it here with the list in hand. */
    if !supported.is_empty() {
        let wanted = match dpi {
            Dpi::Unified(dpi) => vec![dpi],
            Dpi::Separate { x, y } => vec![x, y],
        };
        if let Some(bad) = wanted.iter().find(|value| !supported.contains(value)) {
            return Err(CliError::UnsupportedValue {
                what: "DPI".to_string(),
                value: bad.to_string(),
                supported: render::summarize_numbers(&supported),
            }
            .into());
        }
    }
    if current == dpi {
        cx.unchanged(&object, "DPI", &dpi.to_string());
        return Ok(());
    }
    cx.client
        .set_resolution_dpi(&resolution.path, dpi)
        .await
        .hint_if(
            matches!(dpi, Dpi::Separate { .. }),
            "separate X/Y DPI needs a device with the separate-xy capability",
        )?;
    cx.commit().await?;
    cx.changed(&Change::new(object, "DPI", dpi.to_string()).from(current.to_string()));
    Ok(())
}

pub async fn resolution_activate(cx: &Cx) -> Result<()> {
    let resolution = cx.resolution().await?;
    let object = format!("Resolution {}", resolution.index);
    if cx.client.get_resolution_is_active(&resolution.path).await.unwrap_or(false) {
        cx.unchanged(&object, "state", "active");
        return Ok(());
    }
    cx.client.call_resolution_set_active(&resolution.path).await?;
    cx.commit().await?;
    cx.changed(&Change::new(object, "state", "active"));
    Ok(())
}

pub async fn resolution_default(cx: &Cx) -> Result<()> {
    let resolution = cx.resolution().await?;
    let object = format!("Resolution {}", resolution.index);
    if cx.client.get_resolution_is_default(&resolution.path).await.unwrap_or(false) {
        cx.unchanged(&object, "state", "default");
        return Ok(());
    }
    cx.client.call_resolution_set_default(&resolution.path).await?;
    cx.commit().await?;
    cx.changed(&Change::new(object, "state", "default"));
    Ok(())
}

pub async fn resolution_set_enabled(cx: &Cx, enabled: bool) -> Result<()> {
    let resolution = cx.resolution().await?;
    let object = format!("Resolution {}", resolution.index);
    let currently_enabled = !cx.client.get_resolution_is_disabled(&resolution.path).await?;
    let label = if enabled { "enabled" } else { "disabled" };

    if currently_enabled == enabled {
        cx.unchanged(&object, "state", label);
        return Ok(());
    }
    cx.client
        .set_resolution_is_disabled(&resolution.path, !enabled)
        .await
        .hint("disabling a resolution needs a device with the `disable` capability")?;
    cx.commit().await?;
    cx.changed(
        &Change::new(object, "state", label)
            .from(if currently_enabled { "enabled" } else { "disabled" }),
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Buttons
// ---------------------------------------------------------------------------

pub async fn button_list(cx: &Cx) -> Result<()> {
    let profile = cx.profile().await?;
    let view = ButtonList::new(button_rows(cx, &profile.path).await?);
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else if view.buttons.is_empty() {
        cx.note("this profile has no remappable buttons");
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn button_show(cx: &Cx) -> Result<()> {
    let button = cx.button().await?;
    let action_types = cx.client.get_button_action_types(&button.path).await.unwrap_or_default();
    let view = ButtonView {
        version: 1,
        button: button_row(cx, &button.path).await?,
        supported_types: action_types
            .iter()
            .map(|action_type| codes::action_type_name(*action_type))
            .collect(),
    };
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

/// Map a button to a key, another button, a special action, or nothing.
pub async fn button_set(cx: &Cx, action_type: u32, value: u32) -> Result<()> {
    let button = cx.button().await?;
    let object = format!("Button {}", button.index);
    let current = cx.client.get_button_mapping(&button.path).await.ok();

    let supported = cx.client.get_button_action_types(&button.path).await.unwrap_or_default();
    if !supported.is_empty() && !supported.contains(&action_type) {
        return Err(CliError::UnsupportedValue {
            what: "button action type".to_string(),
            value: codes::action_type_name(action_type).to_string(),
            supported: action_type_names(&supported),
        }
        .into());
    }

    let wanted = match action_type {
        codes::ACTION_NONE => ButtonMapping::Inactive { action_type },
        _ => ButtonMapping::Value { action_type, value },
    };
    let describe = |mapping: &ButtonMapping| {
        let (kind, action) = describe_mapping(mapping);
        format!("{kind} {action}")
    };

    if current.as_ref() == Some(&wanted) {
        cx.unchanged(&object, "mapping", &describe(&wanted));
        return Ok(());
    }
    cx.client.set_button_mapping(&button.path, action_type, value).await?;
    cx.commit().await?;

    let mut change = Change::new(object, "mapping", describe(&wanted));
    if let Some(current) = &current {
        change = change.from(describe(current));
    }
    cx.changed(&change);
    Ok(())
}

pub async fn button_macro(cx: &Cx, steps: &[MacroStep]) -> Result<()> {
    let button = cx.button().await?;
    let object = format!("Button {}", button.index);
    let events = values::expand_macro(steps);
    let current = cx.client.get_button_mapping(&button.path).await.ok();

    let supported = cx.client.get_button_action_types(&button.path).await.unwrap_or_default();
    if !supported.is_empty() && !supported.contains(&codes::ACTION_MACRO) {
        return Err(CliError::UnsupportedValue {
            what: "button action type".to_string(),
            value: "macro".to_string(),
            supported: action_type_names(&supported),
        }
        .into());
    }

    let wanted = ButtonMapping::Macro { events: events.clone() };
    if current.as_ref() == Some(&wanted) {
        cx.unchanged(&object, "mapping", &format!("macro {}", values::format_macro(&events)));
        return Ok(());
    }
    cx.client.set_button_macro_mapping(&button.path, &events).await?;
    cx.commit().await?;

    let mut change = Change::new(
        object,
        "mapping",
        format!("macro {} ({} events)", values::format_macro(&events), events.len()),
    );
    if let Some(current) = &current {
        let (kind, action) = describe_mapping(current);
        change = change.from(format!("{kind} {action}"));
    }
    cx.changed(&change);
    Ok(())
}

// ---------------------------------------------------------------------------
// LEDs
// ---------------------------------------------------------------------------

pub async fn led_list(cx: &Cx) -> Result<()> {
    let profile = cx.profile().await?;
    let view = LedList::new(led_rows(cx, &profile.path).await?);
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else if view.leds.is_empty() {
        cx.note("this device has no configurable lighting");
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn led_show(cx: &Cx) -> Result<()> {
    let led = cx.led().await?;
    let modes = cx.client.get_led_modes(&led.path).await.unwrap_or_default();
    let secondary = cx.client.get_led_secondary_color(&led.path).await.unwrap_or_default();
    let tertiary = cx.client.get_led_tertiary_color(&led.path).await.unwrap_or_default();
    let secondary = Rgb::from_wire(secondary.0, secondary.1, secondary.2);
    let tertiary = Rgb::from_wire(tertiary.0, tertiary.1, tertiary.2);

    let view = LedView {
        version: 1,
        led: led_row(cx, &led.path).await?,
        secondary_color: secondary.to_string(),
        secondary_rgb: [secondary.r, secondary.g, secondary.b],
        tertiary_color: tertiary.to_string(),
        tertiary_rgb: [tertiary.r, tertiary.g, tertiary.b],
        color_depth: codes::color_depth_name(
            cx.client.get_led_color_depth(&led.path).await.unwrap_or(0),
        ),
        supported_modes: modes.iter().map(|mode| LedModeArg::label_of(*mode)).collect(),
    };
    if cx.json {
        cx.emit_json(&serde_json::to_value(&view)?);
    } else {
        cx.block(&view.human(cx.out));
    }
    Ok(())
}

pub async fn led_mode(cx: &Cx, mode: Option<LedModeArg>) -> Result<()> {
    let led = cx.led().await?;
    let current = cx.client.get_led_mode(&led.path).await?;

    let Some(mode) = mode else {
        let modes = cx.client.get_led_modes(&led.path).await.unwrap_or_default();
        cx.reading(
            LedModeArg::label_of(current),
            &modes.iter().map(|mode| LedModeArg::label_of(*mode).to_string()).collect::<Vec<_>>(),
        );
        return Ok(());
    };

    let object = format!("LED {}", led.index);
    let supported = cx.client.get_led_modes(&led.path).await.unwrap_or_default();
    if !supported.is_empty() && !supported.contains(&mode.wire()) {
        return Err(CliError::UnsupportedLedMode {
            index: led.index,
            requested: mode.label(),
            supported: supported.iter().map(|mode| LedModeArg::label_of(*mode)).collect(),
        }
        .into());
    }
    if current == mode.wire() {
        cx.unchanged(&object, "mode", mode.label());
        return Ok(());
    }
    cx.client.set_led_mode(&led.path, mode.wire()).await?;
    cx.commit().await?;
    cx.changed(
        &Change::new(object, "mode", mode.label()).from(LedModeArg::label_of(current)),
    );
    Ok(())
}

/// Which of the three colour slots a `led color` command addresses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorSlot {
    Primary,
    Secondary,
    Tertiary,
}

impl ColorSlot {
    const fn property(self) -> &'static str {
        match self {
            Self::Primary => "color",
            Self::Secondary => "secondary color",
            Self::Tertiary => "tertiary color",
        }
    }
}

pub async fn led_color(cx: &Cx, slot: ColorSlot, color: Option<Rgb>) -> Result<()> {
    let led = cx.led().await?;
    let current = match slot {
        ColorSlot::Primary => cx.client.get_led_color(&led.path).await?,
        ColorSlot::Secondary => cx.client.get_led_secondary_color(&led.path).await?,
        ColorSlot::Tertiary => cx.client.get_led_tertiary_color(&led.path).await?,
    };
    let current = Rgb::from_wire(current.0, current.1, current.2);

    let Some(color) = color else {
        cx.value(&current.to_string());
        return Ok(());
    };

    let object = format!("LED {}", led.index);
    if current == color {
        cx.unchanged(&object, slot.property(), &color.to_string());
        return Ok(());
    }

    let (r, g, b) = (u32::from(color.r), u32::from(color.g), u32::from(color.b));
    match slot {
        ColorSlot::Primary => cx.client.set_led_color(&led.path, r, g, b).await,
        ColorSlot::Secondary => cx.client.set_led_secondary_color(&led.path, r, g, b).await,
        ColorSlot::Tertiary => cx.client.set_led_tertiary_color(&led.path, r, g, b).await,
    }?;
    cx.commit().await?;

    /* A colour set on an LED that is off, or in a mode that ignores it, is stored but invisible;
     * say so rather than let the user think the hardware is broken. */
    let mode = cx.client.get_led_mode(&led.path).await.unwrap_or(0);
    cx.changed(
        &Change::new(object, slot.property(), color.to_string())
            .from(current.to_string())
            .swatch(color),
    );
    advise_invisible_color(cx, slot, mode, led.index);
    Ok(())
}

fn advise_invisible_color(cx: &Cx, slot: ColorSlot, mode: u32, led: u32) {
    let Some(mode) = LedModeArg::from_wire(mode) else { return };
    let visible = match slot {
        ColorSlot::Primary => mode.uses_color(),
        ColorSlot::Secondary => matches!(mode, LedModeArg::Starlight | LedModeArg::TriColor),
        ColorSlot::Tertiary => matches!(mode, LedModeArg::TriColor),
    };
    if !visible {
        cx.note(&format!(
            "LED {led} is in {} mode, which ignores the {}; `ratbagctl led {led} mode solid` shows it",
            mode.label(),
            slot.property(),
        ));
    }
}

pub async fn led_brightness(cx: &Cx, value: Option<u32>) -> Result<()> {
    let led = cx.led().await?;
    let current = cx.client.get_led_brightness(&led.path).await?;

    let Some(value) = value else {
        cx.value(&current.to_string());
        return Ok(());
    };

    let object = format!("LED {}", led.index);
    if current == value {
        cx.unchanged(&object, "brightness", &value.to_string());
        return Ok(());
    }
    cx.client.set_led_brightness(&led.path, value).await?;
    cx.commit().await?;
    cx.changed(
        &Change::new(object, "brightness", value.to_string()).from(current.to_string()),
    );
    Ok(())
}

pub async fn led_duration(cx: &Cx, ms: Option<u32>) -> Result<()> {
    let led = cx.led().await?;
    let current = cx.client.get_led_effect_duration(&led.path).await?;

    let Some(ms) = ms else {
        cx.value(&current.to_string());
        return Ok(());
    };

    let object = format!("LED {}", led.index);
    if current == ms {
        cx.unchanged(&object, "duration", &format!("{ms} ms"));
        return Ok(());
    }
    cx.client.set_led_effect_duration(&led.path, ms).await?;
    cx.commit().await?;
    cx.changed(&Change::new(object, "duration", format!("{ms} ms")).from(format!("{current} ms")));
    Ok(())
}

// ---------------------------------------------------------------------------
// Row builders, shared between the list and detail views
// ---------------------------------------------------------------------------

async fn profile_rows(cx: &Cx, device_path: &str) -> Result<Vec<ProfileRow>> {
    let paths = cx.client.get_device_profiles(device_path).await?;
    let mut rows = Vec::with_capacity(paths.len());
    for path in &paths {
        rows.push(profile_row(cx, path).await?);
    }
    Ok(rows)
}

async fn profile_row(cx: &Cx, path: &str) -> Result<ProfileRow> {
    Ok(ProfileRow {
        index: cx.client.get_profile_index(path).await?,
        name: cx.client.get_profile_name(path).await.unwrap_or_default(),
        report_rate: cx.client.get_profile_report_rate(path).await.unwrap_or(0),
        active: cx.client.get_profile_is_active(path).await.unwrap_or(false),
        enabled: !cx.client.get_profile_disabled(path).await.unwrap_or(false),
        dirty: cx.client.get_profile_is_dirty(path).await.unwrap_or(false),
    })
}

async fn resolution_rows(cx: &Cx, profile_path: &str) -> Result<Vec<ResolutionRow>> {
    let paths = cx.client.get_profile_resolutions(profile_path).await.unwrap_or_default();
    let mut rows = Vec::with_capacity(paths.len());
    for path in &paths {
        rows.push(resolution_row(cx, path).await?);
    }
    Ok(rows)
}

async fn resolution_row(cx: &Cx, path: &str) -> Result<ResolutionRow> {
    let capabilities = cx.client.get_resolution_capabilities(path).await.unwrap_or_default();
    Ok(ResolutionRow {
        index: cx.client.get_resolution_index(path).await?,
        dpi: cx.client.get_resolution_dpi(path).await?.to_string(),
        active: cx.client.get_resolution_is_active(path).await.unwrap_or(false),
        default: cx.client.get_resolution_is_default(path).await.unwrap_or(false),
        disabled: cx.client.get_resolution_is_disabled(path).await.unwrap_or(false),
        supported_dpi: cx.client.get_resolution_dpi_list(path).await.unwrap_or_default(),
        capabilities: capabilities
            .iter()
            .map(|cap| codes::resolution_cap_name(*cap))
            .collect(),
    })
}

async fn button_rows(cx: &Cx, profile_path: &str) -> Result<Vec<ButtonRow>> {
    let paths = cx.client.get_profile_buttons(profile_path).await.unwrap_or_default();
    let mut rows = Vec::with_capacity(paths.len());
    for path in &paths {
        rows.push(button_row(cx, path).await?);
    }
    Ok(rows)
}

async fn button_row(cx: &Cx, path: &str) -> Result<ButtonRow> {
    let mapping = cx.client.get_button_mapping(path).await?;
    let (action_type, action) = describe_mapping(&mapping);
    Ok(ButtonRow { index: cx.client.get_button_index(path).await?, action_type, action })
}

async fn led_rows(cx: &Cx, profile_path: &str) -> Result<Vec<LedRow>> {
    let paths = cx.client.get_profile_leds(profile_path).await.unwrap_or_default();
    let mut rows = Vec::with_capacity(paths.len());
    for path in &paths {
        rows.push(led_row(cx, path).await?);
    }
    Ok(rows)
}

async fn led_row(cx: &Cx, path: &str) -> Result<LedRow> {
    let mode = cx.client.get_led_mode(path).await?;
    let color = cx.client.get_led_color(path).await.unwrap_or_default();
    let color = Rgb::from_wire(color.0, color.1, color.2);
    Ok(LedRow {
        index: cx.client.get_led_index(path).await?,
        mode: LedModeArg::label_of(mode),
        mode_code: mode,
        color: color.to_string(),
        rgb: [color.r, color.g, color.b],
        brightness: cx.client.get_led_brightness(path).await.unwrap_or(0),
        duration_ms: cx.client.get_led_effect_duration(path).await.unwrap_or(0),
    })
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Turn a mapping into `(type name, what it does in words)`.
pub fn describe_mapping(mapping: &ButtonMapping) -> (&'static str, String) {
    match mapping {
        ButtonMapping::Inactive { action_type } => {
            (codes::action_type_name(*action_type), "nothing".to_string())
        }
        ButtonMapping::Value { action_type, value } => match *action_type {
            codes::ACTION_BUTTON => {
                (codes::action_type_name(codes::ACTION_BUTTON), codes::logical_button_display(*value))
            }
            codes::ACTION_SPECIAL => {
                (codes::action_type_name(codes::ACTION_SPECIAL), SpecialArg::label_of(*value))
            }
            codes::ACTION_KEY => (codes::action_type_name(codes::ACTION_KEY), keys::display(*value)),
            other => (codes::action_type_name(other), value.to_string()),
        },
        ButtonMapping::Macro { events } => {
            (codes::action_type_name(codes::ACTION_MACRO), values::format_macro(events))
        }
    }
}

/// Report a setting the device does not implement.
///
/// Reading it is a fair question and answered with a note, leaving the exit status at 0. Trying to
/// write it is a real failure.
fn unsupported(cx: &Cx, what: &str, writing: bool) -> Result<()> {
    if writing {
        return Err(CliError::NotSupported { what: what.to_string() }.into());
    }
    cx.note(&format!("{what} is not supported on this device"));
    Ok(())
}

/// `key, button, macro` — the action types a button accepts, for an error hint.
fn action_type_names(action_types: &[u32]) -> String {
    action_types
        .iter()
        .map(|action_type| codes::action_type_name(*action_type))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_are_described_in_words() {
        assert_eq!(
            describe_mapping(&ButtonMapping::Value { action_type: codes::ACTION_KEY, value: 30 }),
            ("key", "a (KEY_A, 30)".to_string()),
        );
        assert_eq!(
            describe_mapping(&ButtonMapping::Value { action_type: codes::ACTION_BUTTON, value: 2 }),
            ("button", "right (2)".to_string()),
        );
        assert_eq!(
            describe_mapping(&ButtonMapping::Value {
                action_type: codes::ACTION_SPECIAL,
                value: SpecialArg::WheelUp.wire(),
            }),
            ("special", "wheel-up".to_string()),
        );
        assert_eq!(
            describe_mapping(&ButtonMapping::Macro { events: vec![(30, 1), (30, 0)] }),
            ("macro", "a".to_string()),
        );
        assert_eq!(
            describe_mapping(&ButtonMapping::Inactive { action_type: codes::ACTION_NONE }),
            ("disabled", "nothing".to_string()),
        );
    }

    #[test]
    fn action_types_are_named_in_hints() {
        assert_eq!(
            action_type_names(&[codes::ACTION_KEY, codes::ACTION_BUTTON, codes::ACTION_MACRO]),
            "key, button, macro",
        );
        assert_eq!(action_type_names(&[]), "");
    }
}
