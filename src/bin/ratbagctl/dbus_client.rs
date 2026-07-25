/* ratbagctl DBus client: low-level helper for calling the org.freedesktop.ratbag1 API, wrapping
 * property access and method calls for devices, profiles, resolutions, buttons, and LEDs. */
//! Low-level DBus proxy client for `org.freedesktop.ratbag1`.
//!
//! All communication with the daemon goes through this module.

use anyhow::{anyhow, Context, Result};
use zbus::zvariant::{OwnedValue, Value};
use zbus::Connection;

use crate::errors::{CliError, DeviceChoice};

const BUS_NAME: &str = "org.freedesktop.ratbag1";
const MANAGER_PATH: &str = "/org/freedesktop/ratbag1";
const MANAGER_IFACE: &str = "org.freedesktop.ratbag1.Manager";
const DEVICE_IFACE: &str = "org.freedesktop.ratbag1.Device";
const PROFILE_IFACE: &str = "org.freedesktop.ratbag1.Profile";
const RESOLUTION_IFACE: &str = "org.freedesktop.ratbag1.Resolution";
const BUTTON_IFACE: &str = "org.freedesktop.ratbag1.Button";
const LED_IFACE: &str = "org.freedesktop.ratbag1.Led";

/// A client that talks to the `ratbagd` daemon over the session DBus.
pub struct RatbagClient {
    conn: Connection,
}

/// A device selector resolved to its object path.
#[derive(Clone, Debug)]
pub struct ResolvedDevice {
    pub path: String,
    /// Position in `ratbagctl list`, so messages can name the device the way the user saw it.
    pub index: usize,
}

/// A resolution's DPI, which the API exposes either as one value or as separate X and Y values.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Dpi {
    Unified(u32),
    Separate { x: u32, y: u32 },
}

impl std::fmt::Display for Dpi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unified(dpi) => write!(f, "{dpi}"),
            Self::Separate { x, y } => write!(f, "{x}x{y}"),
        }
    }
}

/// What a button is currently mapped to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ButtonMapping {
    /// Action type 0 (disabled) or 1000 (unknown to the daemon).
    Inactive { action_type: u32 },
    /// A button, special action or key: one numeric value whose meaning depends on the type.
    Value { action_type: u32, value: u32 },
    /// A macro, as `(keycode, direction)` pairs where direction 1 is press and 0 is release.
    Macro { events: Vec<(u32, u32)> },
}

/// Last path segment of a device object path, which is the kernel sysname.
pub fn sysname_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl RatbagClient {
    /// Connect to the session bus.
    pub async fn connect() -> Result<Self> {
        let conn = Connection::session()
            .await
            .context("Cannot connect to the session DBus")?;
        Ok(Self { conn })
    }

    // -----------------------------------------------------------------------
    // Manager
    // -----------------------------------------------------------------------

    /// Get the DBus API version from the Manager.
    pub async fn get_api_version(&self) -> Result<i32> {
        self.get_i32_property(MANAGER_PATH, MANAGER_IFACE, "APIVersion").await
    }

    /// Get the list of device object paths from the Manager.
    pub async fn list_devices(&self) -> Result<Vec<String>> {
        let val = self.get_property(MANAGER_PATH, MANAGER_IFACE, "Devices").await?;
        extract_object_path_array(val).context("Failed to parse Devices property")
    }

    /// Load a synthetic test device (dev-hooks only).
    pub async fn load_test_device(&self, json: &str) -> Result<String> {
        let reply = self
            .conn
            .call_method(Some(BUS_NAME), MANAGER_PATH, Some(MANAGER_IFACE), "LoadTestDevice", &(json,))
            .await
            .context("LoadTestDevice call failed")?;
        let path: String = reply.body().deserialize()?;
        Ok(path)
    }

    /// Reset / remove all test devices (dev-hooks only).
    pub async fn reset_test_device(&self) -> Result<()> {
        self.conn
            .call_method(Some(BUS_NAME), MANAGER_PATH, Some(MANAGER_IFACE), "ResetTestDevice", &())
            .await
            .context("ResetTestDevice call failed")?;
        Ok(())
    }

    /// Resolve a device selector to an object path.
    ///
    /// `None` picks the only connected device, which is the common case; with more than one
    /// connected it is an error listing the candidates rather than a silent guess. A selector may
    /// be a zero-based index from `ratbagctl list`, a sysname, or part of the device's product
    /// name (`g502`), all case-insensitive.
    ///
    /// Cost: one `Devices` read for an index, a sysname, or the sole-device default. Product
    /// names need one `Name` read per device, so they are only tried once the cheap matches have
    /// all failed.
    pub async fn resolve_device(&self, spec: Option<&str>) -> Result<ResolvedDevice> {
        let devices = self.list_devices().await?;
        if devices.is_empty() {
            return Err(CliError::NoDevices.into());
        }

        let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
            if devices.len() == 1 {
                return Ok(ResolvedDevice { path: devices[0].clone(), index: 0 });
            }
            return Err(CliError::AmbiguousDefault {
                candidates: self.describe_devices(&devices).await,
            }
            .into());
        };

        /* A bare number is an index into `ratbagctl list`. */
        if let Ok(index) = spec.parse::<usize>() {
            return match devices.get(index) {
                Some(path) => Ok(ResolvedDevice { path: path.clone(), index }),
                None => Err(CliError::DeviceIndexOutOfRange { index, count: devices.len() }.into()),
            };
        }

        let wanted = spec.to_lowercase();

        /* Sysnames first: they are already in hand, so matching them costs nothing. */
        let by_sysname = |exact: bool| -> Vec<usize> {
            devices
                .iter()
                .enumerate()
                .filter(|(_, path)| {
                    let sysname = sysname_of(path).to_lowercase();
                    if exact { sysname == wanted } else { sysname.contains(&wanted) }
                })
                .map(|(index, _)| index)
                .collect()
        };

        for matches in [by_sysname(true), by_sysname(false)] {
            if let Some(resolved) = self.single_match(&devices, &matches, spec).await? {
                return Ok(resolved);
            }
        }

        /* Only now pay for the product names. */
        let mut names = Vec::with_capacity(devices.len());
        for path in &devices {
            names.push(self.get_device_name(path).await.unwrap_or_default().to_lowercase());
        }
        let by_name = |exact: bool| -> Vec<usize> {
            names
                .iter()
                .enumerate()
                .filter(|(_, name)| if exact { *name == &wanted } else { name.contains(&wanted) })
                .map(|(index, _)| index)
                .collect()
        };

        for matches in [by_name(true), by_name(false)] {
            if let Some(resolved) = self.single_match(&devices, &matches, spec).await? {
                return Ok(resolved);
            }
        }

        Err(CliError::NoSuchDevice {
            spec: spec.to_string(),
            candidates: self.describe_devices(&devices).await,
        }
        .into())
    }

    /* Accept a match set only when it names exactly one device: an ambiguous selector is
     * reported with its candidates instead of resolving to whichever came first. */
    async fn single_match(
        &self,
        devices: &[String],
        matches: &[usize],
        spec: &str,
    ) -> Result<Option<ResolvedDevice>> {
        match matches {
            [] => Ok(None),
            [index] => Ok(Some(ResolvedDevice { path: devices[*index].clone(), index: *index })),
            _ => {
                let matched: Vec<String> =
                    matches.iter().filter_map(|index| devices.get(*index).cloned()).collect();
                Err(CliError::AmbiguousDevice {
                    spec: spec.to_string(),
                    candidates: self.describe_devices(&matched).await,
                }
                .into())
            }
        }
    }

    /// Index, name and model for each device, for candidate lists in error hints.
    pub async fn describe_devices(&self, paths: &[String]) -> Vec<DeviceChoice> {
        let all = self.list_devices().await.unwrap_or_default();
        let mut described = Vec::with_capacity(paths.len());
        for path in paths {
            let name = self.get_device_name(path).await.unwrap_or_default();
            let model = self.get_device_model(path).await.unwrap_or_default();
            described.push(DeviceChoice {
                index: all.iter().position(|candidate| candidate == path).unwrap_or_default(),
                name: if name.is_empty() { sysname_of(path).to_string() } else { name },
                model,
            });
        }
        described
    }

    // -----------------------------------------------------------------------
    // Device
    // -----------------------------------------------------------------------

    pub async fn get_device_name(&self, path: &str) -> Result<String> {
        self.get_string_property(path, DEVICE_IFACE, "Name").await
    }

    pub async fn get_device_model(&self, path: &str) -> Result<String> {
        self.get_string_property(path, DEVICE_IFACE, "Model").await
    }

    pub async fn get_device_firmware(&self, path: &str) -> Result<String> {
        self.get_string_property(path, DEVICE_IFACE, "FirmwareVersion").await
    }

    pub async fn get_device_profiles(&self, path: &str) -> Result<Vec<String>> {
        let val = self.get_property(path, DEVICE_IFACE, "Profiles").await?;
        extract_object_path_array(val).context("Failed to parse Profiles property")
    }

    pub async fn commit_device(&self, path: &str) -> Result<u32> {
        let reply = self
            .conn
            .call_method(Some(BUS_NAME), path, Some(DEVICE_IFACE), "Commit", &())
            .await
            .context("Commit call failed")?;
        let result: u32 = reply.body().deserialize()?;
        Ok(result)
    }

    // -----------------------------------------------------------------------
    // Profile
    // -----------------------------------------------------------------------

    pub async fn get_profile_index(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, PROFILE_IFACE, "Index").await
    }

    pub async fn get_profile_name(&self, path: &str) -> Result<String> {
        self.get_string_property(path, PROFILE_IFACE, "Name").await
    }

    pub async fn set_profile_name(&self, path: &str, name: &str) -> Result<()> {
        self.set_property(path, PROFILE_IFACE, "Name", Value::from(name))
            .await
    }

    pub async fn get_profile_is_active(&self, path: &str) -> Result<bool> {
        self.get_bool_property(path, PROFILE_IFACE, "IsActive").await
    }

    pub async fn get_profile_is_dirty(&self, path: &str) -> Result<bool> {
        self.get_bool_property(path, PROFILE_IFACE, "IsDirty").await
    }

    pub async fn get_profile_disabled(&self, path: &str) -> Result<bool> {
        self.get_bool_property(path, PROFILE_IFACE, "Disabled").await
    }

    pub async fn set_profile_disabled(&self, path: &str, disabled: bool) -> Result<()> {
        self.set_property(path, PROFILE_IFACE, "Disabled", Value::from(disabled))
            .await
    }

    pub async fn get_profile_report_rate(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, PROFILE_IFACE, "ReportRate").await
    }

    pub async fn get_profile_report_rates(&self, path: &str) -> Result<Vec<u32>> {
        self.get_vec_u32_property(path, PROFILE_IFACE, "ReportRates").await
    }

    pub async fn get_profile_angle_snapping(&self, path: &str) -> Result<i32> {
        self.get_i32_property(path, PROFILE_IFACE, "AngleSnapping").await
    }

    pub async fn get_profile_debounce(&self, path: &str) -> Result<i32> {
        self.get_i32_property(path, PROFILE_IFACE, "Debounce").await
    }

    pub async fn set_profile_angle_snapping(&self, path: &str, value: i32) -> Result<()> {
        self.set_property(path, PROFILE_IFACE, "AngleSnapping", Value::from(value))
            .await
    }

    pub async fn set_profile_debounce(&self, path: &str, value: i32) -> Result<()> {
        self.set_property(path, PROFILE_IFACE, "Debounce", Value::from(value))
            .await
    }

    pub async fn get_profile_debounces(&self, path: &str) -> Result<Vec<u32>> {
        self.get_vec_u32_property(path, PROFILE_IFACE, "Debounces").await
    }

    pub async fn set_profile_report_rate(&self, path: &str, rate: u32) -> Result<()> {
        self.set_property(path, PROFILE_IFACE, "ReportRate", Value::from(rate))
            .await
    }

    pub async fn call_profile_set_active(&self, path: &str) -> Result<()> {
        self.conn
            .call_method(Some(BUS_NAME), path, Some(PROFILE_IFACE), "SetActive", &())
            .await
            .context("SetActive call failed")?;
        Ok(())
    }

    pub async fn get_profile_resolutions(&self, path: &str) -> Result<Vec<String>> {
        let val = self.get_property(path, PROFILE_IFACE, "Resolutions").await?;
        extract_object_path_array(val).context("Failed to parse Resolutions property")
    }

    pub async fn get_profile_buttons(&self, path: &str) -> Result<Vec<String>> {
        let val = self.get_property(path, PROFILE_IFACE, "Buttons").await?;
        extract_object_path_array(val).context("Failed to parse Buttons property")
    }

    pub async fn get_profile_leds(&self, path: &str) -> Result<Vec<String>> {
        let val = self.get_property(path, PROFILE_IFACE, "Leds").await?;
        extract_object_path_array(val).context("Failed to parse Leds property")
    }

    // -----------------------------------------------------------------------
    // Resolution
    // -----------------------------------------------------------------------

    pub async fn get_resolution_index(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, RESOLUTION_IFACE, "Index").await
    }

    pub async fn get_resolution_is_active(&self, path: &str) -> Result<bool> {
        self.get_bool_property(path, RESOLUTION_IFACE, "IsActive").await
    }

    pub async fn get_resolution_is_default(&self, path: &str) -> Result<bool> {
        self.get_bool_property(path, RESOLUTION_IFACE, "IsDefault").await
    }

    pub async fn get_resolution_is_disabled(&self, path: &str) -> Result<bool> {
        self.get_bool_property(path, RESOLUTION_IFACE, "IsDisabled").await
    }

    pub async fn set_resolution_is_disabled(&self, path: &str, disabled: bool) -> Result<()> {
        self.set_property(path, RESOLUTION_IFACE, "IsDisabled", Value::from(disabled))
            .await
    }

    pub async fn get_resolution_capabilities(&self, path: &str) -> Result<Vec<u32>> {
        self.get_vec_u32_property(path, RESOLUTION_IFACE, "Capabilities").await
    }

    /// Get the list of supported DPI values.
    pub async fn get_resolution_dpi_list(&self, path: &str) -> Result<Vec<u32>> {
        self.get_vec_u32_property(path, RESOLUTION_IFACE, "Resolutions").await
    }

    /// Read a resolution's DPI.
    ///
    /// The DBus property is a variant: either `u32` or `(u32, u32)`.
    pub async fn get_resolution_dpi(&self, path: &str) -> Result<Dpi> {
        let val = self.get_property(path, RESOLUTION_IFACE, "Resolution").await?;
        let inner: Value<'_> = val.into();
        match &inner {
            Value::U32(dpi) => Ok(Dpi::Unified(*dpi)),
            Value::Structure(fields) => match fields.fields() {
                [Value::U32(x), Value::U32(y)] if x == y => Ok(Dpi::Unified(*x)),
                [Value::U32(x), Value::U32(y)] => Ok(Dpi::Separate { x: *x, y: *y }),
                _ => Err(anyhow!("Malformed Resolution property at {}", path)),
            },
            _ => Err(anyhow!("Unexpected Resolution property type at {}", path)),
        }
    }

    /// Set a resolution's DPI.
    ///
    /// A single value is sent as a plain `u32`. Sending `(dpi, dpi)` instead would be read by the
    /// daemon as a separate X/Y resolution and rejected outright on the many devices that lack the
    /// separate-XY capability (see `RatbagResolution::parse_dpi_value` in src/ipc/resolution.rs).
    pub async fn set_resolution_dpi(&self, path: &str, dpi: Dpi) -> Result<()> {
        let value = match dpi {
            Dpi::Unified(dpi) => Value::from(dpi),
            Dpi::Separate { x, y } => Value::from((x, y)),
        };
        let owned = OwnedValue::try_from(value)
            .map_err(|e| anyhow!("Failed to encode D-Bus value: {e}"))?;
        let wrapped = Value::Value(Box::new(owned.into()));
        self.set_property(path, RESOLUTION_IFACE, "Resolution", wrapped)
            .await
    }

    pub async fn call_resolution_set_active(&self, path: &str) -> Result<()> {
        self.conn
            .call_method(
                Some(BUS_NAME),
                path,
                Some(RESOLUTION_IFACE),
                "SetActive",
                &(),
            )
            .await
            .context("SetActive call failed")?;
        Ok(())
    }

    pub async fn call_resolution_set_default(&self, path: &str) -> Result<()> {
        self.conn
            .call_method(
                Some(BUS_NAME),
                path,
                Some(RESOLUTION_IFACE),
                "SetDefault",
                &(),
            )
            .await
            .context("SetDefault call failed")?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Button
    // -----------------------------------------------------------------------

    pub async fn get_button_index(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, BUTTON_IFACE, "Index").await
    }

    /// Read what a button is mapped to.
    ///
    /// Returns the mapping structurally rather than pre-formatted, so that the renderer can turn
    /// keycodes and special-action codes into names.
    pub async fn get_button_mapping(&self, path: &str) -> Result<ButtonMapping> {
        let val = self.get_property(path, BUTTON_IFACE, "Mapping").await?;
        let inner: Value<'_> = val.into();
        let Value::Structure(mapping) = &inner else {
            return Err(anyhow!("Malformed Mapping property at {}", path));
        };
        let [Value::U32(action_type), variant] = mapping.fields() else {
            return Err(anyhow!("Malformed Mapping property at {}", path));
        };

        /* The payload's type is `v`, and clients may double-wrap it. */
        let mut payload = variant;
        while let Value::Value(inner) = payload {
            payload = inner.as_ref();
        }

        match payload {
            Value::U32(value) => Ok(match *action_type {
                0 | 1000 => ButtonMapping::Inactive { action_type: *action_type },
                _ => ButtonMapping::Value { action_type: *action_type, value: *value },
            }),
            Value::Array(array) => {
                let mut events = Vec::with_capacity(array.len());
                for item in array.iter() {
                    let Value::Structure(event) = item else {
                        return Err(anyhow!("Malformed macro mapping entry at {}", path));
                    };
                    let [Value::U32(keycode), Value::U32(direction)] = event.fields() else {
                        return Err(anyhow!("Malformed macro mapping entry at {}", path));
                    };
                    events.push((*keycode, *direction));
                }
                Ok(ButtonMapping::Macro { events })
            }
            _ => Err(anyhow!("Unsupported Mapping payload type at {}", path)),
        }
    }

    pub async fn get_button_action_types(&self, path: &str) -> Result<Vec<u32>> {
        self.get_vec_u32_property(path, BUTTON_IFACE, "ActionTypes").await
    }

    pub async fn set_button_mapping(
        &self,
        path: &str,
        action_type: u32,
        value: u32,
    ) -> Result<()> {
        let mapping = (action_type, Value::from(value));
        self.set_property(path, BUTTON_IFACE, "Mapping", Value::from(mapping))
            .await
    }

    /// Set a macro mapping (action type 4) with a list of (keycode, direction) pairs.
    pub async fn set_button_macro_mapping(
        &self,
        path: &str,
        events: &[(u32, u32)],
    ) -> Result<()> {
        for &(keycode, direction) in events {
            anyhow::ensure!(keycode <= u16::MAX as u32, "Invalid keycode {} (max 65535)", keycode);
            anyhow::ensure!(direction <= 1, "Invalid macro direction {} (expected 0 or 1)", direction);
        }
        let arr: Vec<(u32, u32)> = events.to_vec();
        let mapping = (4u32, Value::from(arr));
        self.set_property(path, BUTTON_IFACE, "Mapping", Value::from(mapping))
            .await
    }

    // -----------------------------------------------------------------------
    // LED
    // -----------------------------------------------------------------------

    pub async fn get_led_index(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, LED_IFACE, "Index").await
    }

    pub async fn get_led_mode(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, LED_IFACE, "Mode").await
    }

    pub async fn get_led_modes(&self, path: &str) -> Result<Vec<u32>> {
        self.get_vec_u32_property(path, LED_IFACE, "Modes").await
    }

    pub async fn get_led_color(&self, path: &str) -> Result<(u32, u32, u32)> {
        let val = self.get_property(path, LED_IFACE, "Color").await?;
        let inner: Value<'_> = val.into();
        if let Value::Structure(s) = &inner
            && let [Value::U32(r), Value::U32(g), Value::U32(b)] = s.fields()
        {
            return Ok((*r, *g, *b));
        }
        Err(anyhow!("Malformed Color property at {}", path))
    }

    pub async fn get_led_brightness(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, LED_IFACE, "Brightness").await
    }

    pub async fn get_led_effect_duration(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, LED_IFACE, "EffectDuration").await
    }

    pub async fn set_led_effect_duration(&self, path: &str, duration: u32) -> Result<()> {
        self.set_property(path, LED_IFACE, "EffectDuration", Value::from(duration))
            .await
    }

    pub async fn get_led_secondary_color(&self, path: &str) -> Result<(u32, u32, u32)> {
        let val = self.get_property(path, LED_IFACE, "SecondaryColor").await?;
        let inner: Value<'_> = val.into();
        if let Value::Structure(s) = &inner
            && let [Value::U32(r), Value::U32(g), Value::U32(b)] = s.fields()
        {
            return Ok((*r, *g, *b));
        }
        Err(anyhow!("Malformed SecondaryColor property at {}", path))
    }

    pub async fn set_led_secondary_color(&self, path: &str, r: u32, g: u32, b: u32) -> Result<()> {
        validate_rgb(r, g, b)?;
        self.set_property(path, LED_IFACE, "SecondaryColor", Value::from((r, g, b)))
            .await
    }

    pub async fn get_led_tertiary_color(&self, path: &str) -> Result<(u32, u32, u32)> {
        let val = self.get_property(path, LED_IFACE, "TertiaryColor").await?;
        let inner: Value<'_> = val.into();
        if let Value::Structure(s) = &inner
            && let [Value::U32(r), Value::U32(g), Value::U32(b)] = s.fields()
        {
            return Ok((*r, *g, *b));
        }
        Err(anyhow!("Malformed TertiaryColor property at {}", path))
    }

    pub async fn set_led_tertiary_color(&self, path: &str, r: u32, g: u32, b: u32) -> Result<()> {
        validate_rgb(r, g, b)?;
        self.set_property(path, LED_IFACE, "TertiaryColor", Value::from((r, g, b)))
            .await
    }

    pub async fn get_led_color_depth(&self, path: &str) -> Result<u32> {
        self.get_u32_property(path, LED_IFACE, "ColorDepth").await
    }

    pub async fn set_led_mode(&self, path: &str, mode: u32) -> Result<()> {
        self.set_property(path, LED_IFACE, "Mode", Value::from(mode)).await
    }

    pub async fn set_led_color(&self, path: &str, r: u32, g: u32, b: u32) -> Result<()> {
        validate_rgb(r, g, b)?;
        self.set_property(path, LED_IFACE, "Color", Value::from((r, g, b)))
            .await
    }

    pub async fn set_led_brightness(&self, path: &str, brightness: u32) -> Result<()> {
        anyhow::ensure!(brightness <= 255, "Brightness out of range: {} (expected 0..=255)", brightness);
        self.set_property(path, LED_IFACE, "Brightness", Value::from(brightness))
            .await
    }

    // -----------------------------------------------------------------------
    // Generic helpers
    // -----------------------------------------------------------------------

    async fn get_property(&self, path: &str, iface: &str, prop: &str) -> Result<OwnedValue> {
        let reply = self
            .conn
            .call_method(
                Some(BUS_NAME),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(iface, prop),
            )
            .await
            .with_context(|| format!("Get {}.{} at {} failed", iface, prop, path))?;
        let val: OwnedValue = reply.body().deserialize()?;
        Ok(val)
    }

    async fn set_property(&self, path: &str, iface: &str, prop: &str, value: Value<'_>) -> Result<()> {
        self.conn
            .call_method(
                Some(BUS_NAME),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "Set",
                &(iface, prop, value),
            )
            .await
            .with_context(|| format!("Set {}.{} at {} failed", iface, prop, path))?;
        Ok(())
    }

    async fn get_string_property(&self, path: &str, iface: &str, prop: &str) -> Result<String> {
        let val = self.get_property(path, iface, prop).await?;
        val.downcast_ref::<String>()
            .with_context(|| format!("Type mismatch for {}.{} at {}", iface, prop, path))
    }

    async fn get_u32_property(&self, path: &str, iface: &str, prop: &str) -> Result<u32> {
        let val = self.get_property(path, iface, prop).await?;
        val.downcast_ref::<u32>()
            .with_context(|| format!("Type mismatch for {}.{} at {}", iface, prop, path))
    }

    async fn get_i32_property(&self, path: &str, iface: &str, prop: &str) -> Result<i32> {
        let val = self.get_property(path, iface, prop).await?;
        val.downcast_ref::<i32>()
            .with_context(|| format!("Type mismatch for {}.{} at {}", iface, prop, path))
    }

    async fn get_bool_property(&self, path: &str, iface: &str, prop: &str) -> Result<bool> {
        let val = self.get_property(path, iface, prop).await?;
        val.downcast_ref::<bool>()
            .with_context(|| format!("Type mismatch for {}.{} at {}", iface, prop, path))
    }

    async fn get_vec_u32_property(&self, path: &str, iface: &str, prop: &str) -> Result<Vec<u32>> {
        let val = self.get_property(path, iface, prop).await?;
        extract_u32_array(val).with_context(|| format!("Type mismatch for {}.{} at {}", iface, prop, path))
    }
}

// ---------------------------------------------------------------------------
// Free-standing helpers for extracting arrays from OwnedValue
// ---------------------------------------------------------------------------

/// Extract a `Vec<String>` of object-path strings from an `OwnedValue`
/// that wraps an array of object-paths.
fn extract_object_path_array(val: OwnedValue) -> Result<Vec<String>> {
    let inner: Value<'_> = val.into();
    match inner {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr.iter() {
                match item {
                    Value::ObjectPath(p) => out.push(p.to_string()),
                    _ => return Err(anyhow!("Array contains non-object-path value")),
                }
            }
            Ok(out)
        }
        _ => Err(anyhow!("Value is not an array of object paths")),
    }
}

/// Extract a `Vec<u32>` from an `OwnedValue` that wraps an array of u32.
fn extract_u32_array(val: OwnedValue) -> Result<Vec<u32>> {
    let inner: Value<'_> = val.into();
    match inner {
        Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for value in arr.iter() {
                if let Value::U32(number) = value {
                    out.push(*number);
                } else {
                    return Err(anyhow!("Array contains non-u32 value"));
                }
            }
            Ok(out)
        }
        _ => Err(anyhow!("Value is not an array of u32")),
    }
}

fn validate_rgb(r: u32, g: u32, b: u32) -> Result<()> {
    anyhow::ensure!(r <= 255, "Red component out of range: {}", r);
    anyhow::ensure!(g <= 255, "Green component out of range: {}", g);
    anyhow::ensure!(b <= 255, "Blue component out of range: {}", b);
    Ok(())
}
