/* Button action types exposed over DBus. */
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ActionType {
    None = 0,
    Button = 1,
    Special = 2,
    Key = 3,
    Macro = 4,
    Unknown = 1000,
}

/* Color as an RGB triplet. */
#[derive(Debug, Clone, Copy, Default)]
pub struct Color {
    pub red: u32,
    pub green: u32,
    pub blue: u32,
}

/* Resolution value, either unified or per-axis. */
#[derive(Debug, Clone, Copy)]
pub enum Dpi {
    Unified(u32),
    Separate { x: u32, y: u32 },
}

/* Device state synced from hardware. */
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub sysname: String,
    pub name: String,
    pub model: String,
    pub firmware_version: String,
    pub profiles: Vec<ProfileInfo>,
}

impl DeviceInfo {
    /* Translate a numeric bustype from HID_ID into the string used in `.device` files. */
    fn bustype_to_string(bustype: u16) -> String {
        match bustype {
            0x03 => "usb".to_string(),
            0x05 => "bluetooth".to_string(),
            _ => format!("{:04x}", bustype),
        }
    }

    /* Build a `DeviceInfo` struct from a matched `DeviceEntry` and detected hardware props. */
    pub fn from_entry(
        sysname: &str,
        name: &str,
        bustype: u16,
        vid: u16,
        pid: u16,
        entry: &crate::device_database::DeviceEntry,
    ) -> Self {
        let bus_str = Self::bustype_to_string(bustype);
        let model = format!("{}:{:04x}:{:04x}:0", bus_str, vid, pid);

        /* Use the driver config to determine the number of profiles, buttons, etc. */
        let num_profiles = entry
            .driver_config
            .as_ref()
            .and_then(|c| c.profiles)
            .unwrap_or(1) as usize;
        let num_buttons = entry
            .driver_config
            .as_ref()
            .and_then(|c| c.buttons)
            .unwrap_or(0) as usize;
        let num_leds = entry
            .driver_config
            .as_ref()
            .and_then(|c| c.leds)
            .unwrap_or(0) as usize;
        let num_dpis = entry
            .driver_config
            .as_ref()
            .and_then(|c| c.dpis)
            .unwrap_or(1) as usize;

        /* Build DPI list from the range specification if available */
        let dpi_list: Vec<u32> = entry
            .driver_config
            .as_ref()
            .and_then(|c| c.dpi_range.as_ref())
            .map(|r| (r.min..=r.max).step_by(r.step as usize).collect())
            .unwrap_or_else(|| vec![800, 1600]);

        let profiles: Vec<ProfileInfo> = (0..num_profiles as u32)
            .map(|idx| ProfileInfo {
                index: idx,
                name: String::new(),
                is_active: idx == 0,
                is_enabled: true,
                is_dirty: false,
                report_rate: 1000,
                report_rates: vec![125, 250, 500, 1000],
                angle_snapping: -1,
                debounce: -1,
                debounces: Vec::new(),
                resolutions: (0..num_dpis as u32)
                    .map(|ri| ResolutionInfo {
                        index: ri,
                        dpi: Dpi::Unified(800),
                        dpi_list: dpi_list.clone(),
                        capabilities: Vec::new(),
                        is_active: ri == 0,
                        is_default: ri == 0,
                        is_disabled: false,
                    })
                    .collect(),
                buttons: (0..num_buttons as u32)
                    .map(|bi| ButtonInfo {
                        index: bi,
                        action_type: ActionType::Button,
                        action_types: vec![0, 1, 2, 3, 4],
                        mapping_value: bi,
                        macro_entries: Vec::new(),
                    })
                    .collect(),
                leds: (0..num_leds as u32)
                    .map(|li| LedInfo {
                        index: li,
                        mode: 0,
                        modes: vec![0, 1, 2, 3],
                        color: Color::default(),
                        color_depth: 1,
                        effect_duration: 0,
                        brightness: 255,
                    })
                    .collect(),
            })
            .collect();

        Self {
            sysname: sysname.to_string(),
            name: name.to_string(),
            model,
            firmware_version: String::new(),
            profiles,
        }
    }
}

/* Profile state. */
#[derive(Debug, Clone)]
pub struct ProfileInfo {
    pub index: u32,
    pub name: String,
    pub is_active: bool,
    pub is_enabled: bool,
    pub is_dirty: bool,
    pub report_rate: u32,
    pub report_rates: Vec<u32>,
    pub angle_snapping: i32,
    pub debounce: i32,
    pub debounces: Vec<u32>,
    pub resolutions: Vec<ResolutionInfo>,
    pub buttons: Vec<ButtonInfo>,
    pub leds: Vec<LedInfo>,
}

/* Resolution state. */
#[derive(Debug, Clone)]
pub struct ResolutionInfo {
    pub index: u32,
    pub dpi: Dpi,
    pub dpi_list: Vec<u32>,
    pub capabilities: Vec<u32>,
    pub is_active: bool,
    pub is_default: bool,
    pub is_disabled: bool,
}

/* Button mapping state. */
#[derive(Debug, Clone)]
pub struct ButtonInfo {
    pub index: u32,
    pub action_type: ActionType,
    pub action_types: Vec<u32>,
    pub mapping_value: u32,
    pub macro_entries: Vec<(u32, u32)>,
}

/* LED state. */
#[derive(Debug, Clone)]
pub struct LedInfo {
    pub index: u32,
    pub mode: u32,
    pub modes: Vec<u32>,
    pub color: Color,
    pub color_depth: u32,
    pub effect_duration: u32,
    pub brightness: u32,
}
