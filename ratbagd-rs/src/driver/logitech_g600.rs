/// Logitech G600 gaming mouse driver.
///
/// Targets the Logitech G600 MMO Gaming Mouse, a 20-button device with
/// 3 profiles, 4 DPI levels, and one RGB LED zone.  The G600 also
/// features a "G-Shift" modifier that provides an alternate set of 20
/// button bindings.
///
/// The protocol is straightforward: each profile is stored as a
/// fixed-length 154-byte HID feature report accessible via a
/// dedicated report ID (0xF3, 0xF4, 0xF5).  The active profile and
/// resolution are read/written through a 4-byte feature report at
/// 0xF0.  There are no checksums, no command-response handshakes,
/// and no variable-length data.
///
/// Reference implementation: `src/driver-logitech-g600.c`.
use anyhow::{bail, Result};
use async_trait::async_trait;
use tracing::{debug, warn};

use crate::device::{
    ActionType, Color, DeviceInfo, Dpi, LedMode, special_action,
};
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

const NUM_PROFILES: usize = 3;
const NUM_BUTTONS_PER_MODE: usize = 20;
const NUM_DPI: usize = 4;

const DPI_MIN: u32 = 200;
const DPI_MAX: u32 = 8200;

/* HID report IDs */
const REPORT_ID_GET_ACTIVE: u8 = 0xF0;
const REPORT_ID_PROFILE_0: u8 = 0xF3;
const REPORT_ID_PROFILE_1: u8 = 0xF4;
const REPORT_ID_PROFILE_2: u8 = 0xF5;

/// Size of a full profile report (bytes).
const REPORT_SIZE_PROFILE: usize = 154;

/* LED effect values */
const LED_SOLID: u8 = 0x00;
const LED_BREATHE: u8 = 0x01;
const LED_CYCLE: u8 = 0x02;

/* Available polling rates (Hz). */
const REPORT_RATES: [u32; 8] = [125, 142, 166, 200, 250, 333, 500, 1000];

/* Available LED modes. */
const LED_MODES: [LedMode; 4] = [
    LedMode::Off,
    LedMode::Solid,
    LedMode::Breathing,
    LedMode::Cycle,
];

/* ------------------------------------------------------------------ */
/* Button action codes                                                  */
/* ------------------------------------------------------------------ */

/// Button action mapping entry: hardware raw code -> (ActionType, value).
struct ButtonMapping {
    raw: u8,
    action_type: ActionType,
    value: u32,
}

static BUTTON_MAP: &[ButtonMapping] = &[
    /* 0x00 is handled separately: it means keyboard key when modifier/key are nonzero */
    ButtonMapping { raw: 0x01, action_type: ActionType::Button, value: 1 },
    ButtonMapping { raw: 0x02, action_type: ActionType::Button, value: 2 },
    ButtonMapping { raw: 0x03, action_type: ActionType::Button, value: 3 },
    ButtonMapping { raw: 0x04, action_type: ActionType::Button, value: 4 },
    ButtonMapping { raw: 0x05, action_type: ActionType::Button, value: 5 },
    ButtonMapping { raw: 0x11, action_type: ActionType::Special, value: special_action::RESOLUTION_UP },
    ButtonMapping { raw: 0x12, action_type: ActionType::Special, value: special_action::RESOLUTION_DOWN },
    ButtonMapping { raw: 0x13, action_type: ActionType::Special, value: special_action::RESOLUTION_CYCLE_UP },
    ButtonMapping { raw: 0x14, action_type: ActionType::Special, value: special_action::PROFILE_CYCLE_UP },
    ButtonMapping { raw: 0x15, action_type: ActionType::Special, value: special_action::RESOLUTION_ALTERNATE },
    ButtonMapping { raw: 0x17, action_type: ActionType::Special, value: special_action::SECOND_MODE },
];

/* ------------------------------------------------------------------ */
/* Report data layouts                                                  */
/* ------------------------------------------------------------------ */

/// A single button entry in the profile report (3 bytes, packed).
#[derive(Debug, Default, Clone, Copy)]
struct ButtonEntry {
    code: u8,
    modifier: u8,
    key: u8,
}

/// Full profile report parsed from the 154-byte HID feature report.
#[derive(Debug, Clone)]
struct ProfileReport {
    id: u8,
    led_red: u8,
    led_green: u8,
    led_blue: u8,
    led_effect: u8,
    led_duration: u8,
    unknown1: [u8; 5],
    frequency: u8,
    dpi_shift: u8,
    dpi_default: u8,
    dpi: [u8; NUM_DPI],
    unknown2: [u8; 13],
    buttons: [ButtonEntry; NUM_BUTTONS_PER_MODE],
    g_shift_color: [u8; 3],
    g_shift_buttons: [ButtonEntry; NUM_BUTTONS_PER_MODE],
}

impl ProfileReport {
    /* Parse from a 154-byte buffer. */
    fn from_bytes(buf: &[u8; REPORT_SIZE_PROFILE]) -> Self {
        let mut buttons = [ButtonEntry::default(); NUM_BUTTONS_PER_MODE];
        let mut g_shift_buttons = [ButtonEntry::default(); NUM_BUTTONS_PER_MODE];

        /* Standard buttons start at byte 31: id(1) + led(5) + unknown1(5) +
         * frequency(1) + dpi_shift(1) + dpi_default(1) + dpi(4) + unknown2(13) = 31 */
        let btn_base = 31;
        for i in 0..NUM_BUTTONS_PER_MODE {
            let off = btn_base + i * 3;
            buttons[i] = ButtonEntry {
                code: buf[off],
                modifier: buf[off + 1],
                key: buf[off + 2],
            };
        }

        /* G-Shift color at btn_base + 60 = 91, then G-Shift buttons at 94 */
        let gs_color_off = btn_base + NUM_BUTTONS_PER_MODE * 3; /* 91 */
        let gs_btn_base = gs_color_off + 3; /* 94 */
        for i in 0..NUM_BUTTONS_PER_MODE {
            let off = gs_btn_base + i * 3;
            g_shift_buttons[i] = ButtonEntry {
                code: buf[off],
                modifier: buf[off + 1],
                key: buf[off + 2],
            };
        }

        let mut unknown1 = [0u8; 5];
        unknown1.copy_from_slice(&buf[6..11]);
        let mut unknown2 = [0u8; 13];
        unknown2.copy_from_slice(&buf[18..31]);
        let mut dpi = [0u8; NUM_DPI];
        dpi.copy_from_slice(&buf[14..18]);

        Self {
            id: buf[0],
            led_red: buf[1],
            led_green: buf[2],
            led_blue: buf[3],
            led_effect: buf[4],
            led_duration: buf[5],
            unknown1,
            frequency: buf[11],
            dpi_shift: buf[12],
            dpi_default: buf[13],
            dpi,
            unknown2,
            buttons,
            g_shift_color: [buf[gs_color_off], buf[gs_color_off + 1], buf[gs_color_off + 2]],
            g_shift_buttons,
        }
    }

    /* Serialize back to a 154-byte buffer. */
    fn to_bytes(&self) -> [u8; REPORT_SIZE_PROFILE] {
        let mut buf = [0u8; REPORT_SIZE_PROFILE];
        buf[0] = self.id;
        buf[1] = self.led_red;
        buf[2] = self.led_green;
        buf[3] = self.led_blue;
        buf[4] = self.led_effect;
        buf[5] = self.led_duration;
        buf[6..11].copy_from_slice(&self.unknown1);
        buf[11] = self.frequency;
        buf[12] = self.dpi_shift;
        buf[13] = self.dpi_default;
        buf[14..18].copy_from_slice(&self.dpi);
        buf[18..31].copy_from_slice(&self.unknown2);

        let btn_base = 31;
        for i in 0..NUM_BUTTONS_PER_MODE {
            let off = btn_base + i * 3;
            buf[off] = self.buttons[i].code;
            buf[off + 1] = self.buttons[i].modifier;
            buf[off + 2] = self.buttons[i].key;
        }

        let gs_color_off = btn_base + NUM_BUTTONS_PER_MODE * 3;
        buf[gs_color_off] = self.g_shift_color[0];
        buf[gs_color_off + 1] = self.g_shift_color[1];
        buf[gs_color_off + 2] = self.g_shift_color[2];

        let gs_btn_base = gs_color_off + 3;
        for i in 0..NUM_BUTTONS_PER_MODE {
            let off = gs_btn_base + i * 3;
            buf[off] = self.g_shift_buttons[i].code;
            buf[off + 1] = self.g_shift_buttons[i].modifier;
            buf[off + 2] = self.g_shift_buttons[i].key;
        }

        buf
    }
}

/// Polled active-profile + resolution report (4 bytes).
#[derive(Debug, Default, Clone, Copy)]
struct ActiveProfileReport {
    id: u8,
    packed: u8,
    unknown3: u8,
    unknown4: u8,
}

impl ActiveProfileReport {
    fn from_bytes(buf: &[u8; 4]) -> Self {
        Self {
            id: buf[0],
            packed: buf[1],
            unknown3: buf[2],
            unknown4: buf[3],
        }
    }

    /* Extract the active profile index (0-based). */
    fn profile(&self) -> u8 {
        (self.packed >> 4) & 0x0f
    }

    /* Extract the active resolution index (0-based). */
    fn resolution(&self) -> u8 {
        (self.packed >> 1) & 0x03
    }
}

/* ------------------------------------------------------------------ */
/* DPI / frequency helpers                                              */
/* ------------------------------------------------------------------ */

fn dpi_to_raw(dpi: u32) -> Option<u8> {
    if dpi < DPI_MIN || dpi > DPI_MAX || dpi % 50 != 0 {
        return None;
    }
    u8::try_from(dpi / 50).ok()
}

fn raw_to_dpi(raw: u8) -> u32 {
    u32::from(raw) * 50
}

fn raw_to_hz(raw: u8) -> u32 {
    if raw == 0 {
        1000
    } else {
        1000 / (u32::from(raw) + 1)
    }
}

fn hz_to_raw(hz: u32) -> u8 {
    if hz >= 1000 {
        0
    } else {
        ((1000 / hz) - 1) as u8
    }
}

/* ------------------------------------------------------------------ */
/* Button mapping helpers                                               */
/* ------------------------------------------------------------------ */

fn raw_to_action(btn: &ButtonEntry) -> (ActionType, u32) {
    /* Code 0x00 with nonzero modifier/key means keyboard key action. */
    if btn.code == 0x00 && (btn.modifier != 0 || btn.key != 0) {
        return (ActionType::Key, u32::from(btn.key));
    }

    /* Look up in the static table. */
    for m in BUTTON_MAP {
        if m.raw == btn.code {
            return (m.action_type, m.value);
        }
    }

    /* Code 0x00 with all zeros → None. */
    if btn.code == 0x00 {
        return (ActionType::None, 0);
    }

    /* Unknown code — expose as Unknown with the raw value. */
    (ActionType::Unknown, u32::from(btn.code))
}

fn action_to_raw(action_type: ActionType, value: u32) -> ButtonEntry {
    match action_type {
        ActionType::Key => ButtonEntry {
            code: 0x00,
            modifier: 0x00,
            key: value as u8,
        },
        ActionType::None => ButtonEntry::default(),
        ActionType::Button | ActionType::Special => {
            for m in BUTTON_MAP {
                if m.action_type == action_type && m.value == value {
                    return ButtonEntry {
                        code: m.raw,
                        modifier: 0x00,
                        key: 0x00,
                    };
                }
            }
            ButtonEntry::default()
        }
        _ => ButtonEntry::default(),
    }
}

/* ------------------------------------------------------------------ */
/* LED helpers                                                          */
/* ------------------------------------------------------------------ */

fn hw_led_to_mode(effect: u8) -> LedMode {
    match effect {
        LED_SOLID => LedMode::Solid,
        LED_BREATHE => LedMode::Breathing,
        LED_CYCLE => LedMode::Cycle,
        _ => LedMode::Off,
    }
}

fn mode_to_hw_led(mode: LedMode) -> u8 {
    match mode {
        LedMode::Solid => LED_SOLID,
        LedMode::Breathing => LED_BREATHE,
        LedMode::Cycle => LED_CYCLE,
        _ => LED_SOLID, /* Off is solid with black color */
    }
}

/* ------------------------------------------------------------------ */
/* Cached state                                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct G600Data {
    profile_reports: [Option<ProfileReport>; NUM_PROFILES],
    active: ActiveProfileReport,
}

/// Report IDs for the three profiles, indexed by profile number.
const PROFILE_REPORT_IDS: [u8; NUM_PROFILES] = [
    REPORT_ID_PROFILE_0,
    REPORT_ID_PROFILE_1,
    REPORT_ID_PROFILE_2,
];

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct LG600Driver {
    data: Option<G600Data>,
}

impl LG600Driver {
    pub fn new() -> Self {
        Self { data: None }
    }

    /* Read all three profile reports from hardware. */
    fn read_profiles(&mut self, io: &mut DeviceIo) -> Result<()> {
        let data = self.data.as_mut().unwrap();
        for (idx, &report_id) in PROFILE_REPORT_IDS.iter().enumerate() {
            let mut buf = [0u8; REPORT_SIZE_PROFILE];
            buf[0] = report_id;
            io.get_feature_report(&mut buf)
                .map_err(anyhow::Error::from)?;
            data.profile_reports[idx] = Some(ProfileReport::from_bytes(&buf));
        }
        Ok(())
    }
}

#[async_trait]
impl DeviceDriver for LG600Driver {
    fn name(&self) -> &str {
        "Logitech G600"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Read the active profile report to confirm the device responds. */
        let mut active_buf = [0u8; 4];
        active_buf[0] = REPORT_ID_GET_ACTIVE;
        io.get_feature_report(&mut active_buf)
            .map_err(anyhow::Error::from)?;

        self.data = Some(G600Data {
            profile_reports: Default::default(),
            active: ActiveProfileReport::from_bytes(&active_buf),
        });

        /* Read all three profile reports. */
        self.read_profiles(io)?;

        debug!("Logitech G600: probe succeeded");
        Ok(())
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("G600: probe was not called"))?;

        let active_profile = data.active.profile();
        let active_resolution = data.active.resolution();

        /* Build DPI list from range. */
        let dpi_list: Vec<u32> = (DPI_MIN..=DPI_MAX).step_by(50).collect();

        for (pi, profile) in info.profiles.iter_mut().enumerate() {
            let report = match &data.profile_reports[pi] {
                Some(r) => r,
                None => {
                    warn!("G600: profile {} not loaded, skipping", pi);
                    continue;
                }
            };

            /* Polling rate */
            profile.report_rate = raw_to_hz(report.frequency);
            profile.report_rates = REPORT_RATES.to_vec();

            /* Active profile flag */
            profile.is_active = pi as u8 == active_profile;

            /* Resolutions */
            for (ri, res) in profile.resolutions.iter_mut().enumerate() {
                let raw = *report.dpi.get(ri).unwrap_or(&0);
                let dpi = raw_to_dpi(raw);
                res.dpi = Dpi::Unified(dpi);
                res.dpi_list = dpi_list.clone();
                res.is_default = report.dpi_default.wrapping_sub(1) == ri as u8;
                res.is_active = if profile.is_active {
                    ri as u8 == active_resolution
                } else {
                    res.is_default
                };
            }

            /* Buttons: the device exposes 20 standard + 20 G-Shift buttons.
             * The DeviceInfo was pre-populated with NUM_BUTTONS entries from
             * the .device file, so we fill as many as we have. */
            for (bi, btn_info) in profile.buttons.iter_mut().enumerate() {
                let btn_entry = if bi < NUM_BUTTONS_PER_MODE {
                    &report.buttons[bi]
                } else if bi < NUM_BUTTONS_PER_MODE * 2 {
                    &report.g_shift_buttons[bi - NUM_BUTTONS_PER_MODE]
                } else {
                    continue;
                };

                let (action_type, value) = raw_to_action(btn_entry);
                btn_info.action_type = action_type;
                btn_info.mapping_value = value;
            }

            /* LED: single LED zone */
            if let Some(led) = profile.leds.first_mut() {
                led.modes = LED_MODES.to_vec();
                led.color_depth = 3; /* RGB 8-8-8 */

                if report.led_red == 0 && report.led_green == 0 && report.led_blue == 0
                    && report.led_effect == LED_SOLID
                {
                    led.mode = LedMode::Off;
                } else {
                    led.mode = hw_led_to_mode(report.led_effect);
                }

                led.color = Color {
                    red: u32::from(report.led_red),
                    green: u32::from(report.led_green),
                    blue: u32::from(report.led_blue),
                };
                led.effect_duration = u32::from(report.led_duration) * 1000;
            }
        }

        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("G600: probe was not called"))?;

        for (pi, profile) in info.profiles.iter().enumerate() {
            if !profile.is_dirty {
                continue;
            }

            let report = match &mut data.profile_reports[pi] {
                Some(r) => r,
                None => {
                    warn!("G600: profile {} not loaded, cannot commit", pi);
                    continue;
                }
            };

            debug!("G600: committing profile {}", pi);

            /* Polling rate */
            report.frequency = hz_to_raw(profile.report_rate);

            /* Resolutions */
            let mut active_resolution: u32 = 0;
            for (ri, res) in profile.resolutions.iter().enumerate() {
                let dpi_val = match res.dpi {
                    Dpi::Unified(d) => d,
                    Dpi::Separate { x, .. } => x,
                    Dpi::Unknown => 800,
                };
                report.dpi[ri] = dpi_to_raw(dpi_val).unwrap_or(0x10); /* 800 DPI fallback */

                if res.is_default {
                    report.dpi_default = (ri as u8) + 1;
                }
                if profile.is_active && res.is_active {
                    active_resolution = ri as u32;
                }
            }

            /* Buttons */
            for (bi, btn_info) in profile.buttons.iter().enumerate() {
                let entry = action_to_raw(btn_info.action_type, btn_info.mapping_value);
                if bi < NUM_BUTTONS_PER_MODE {
                    report.buttons[bi] = entry;
                } else if bi < NUM_BUTTONS_PER_MODE * 2 {
                    report.g_shift_buttons[bi - NUM_BUTTONS_PER_MODE] = entry;
                }
            }

            /* LED */
            if let Some(led) = profile.leds.first() {
                match led.mode {
                    LedMode::Off => {
                        report.led_effect = LED_SOLID;
                        report.led_red = 0;
                        report.led_green = 0;
                        report.led_blue = 0;
                    }
                    LedMode::Solid => {
                        report.led_effect = LED_SOLID;
                        report.led_red = led.color.red.min(255) as u8;
                        report.led_green = led.color.green.min(255) as u8;
                        report.led_blue = led.color.blue.min(255) as u8;
                    }
                    LedMode::Breathing => {
                        report.led_effect = LED_BREATHE;
                        report.led_red = led.color.red.min(255) as u8;
                        report.led_green = led.color.green.min(255) as u8;
                        report.led_blue = led.color.blue.min(255) as u8;
                        report.led_duration = (led.effect_duration / 1000).min(15) as u8;
                    }
                    LedMode::Cycle => {
                        report.led_effect = LED_CYCLE;
                        report.led_red = led.color.red.min(255) as u8;
                        report.led_green = led.color.green.min(255) as u8;
                        report.led_blue = led.color.blue.min(255) as u8;
                        report.led_duration = (led.effect_duration / 1000).min(15) as u8;
                    }
                    _ => {}
                }

                /* Copy main color to G-Shift color (matching C driver behaviour). */
                report.g_shift_color = [report.led_red, report.led_green, report.led_blue];
            }

            /* Write the profile report. */
            let buf = report.to_bytes();
            io.set_feature_report(&buf).map_err(anyhow::Error::from)?;

            /* If this is the active profile, update the resolution. */
            if profile.is_active {
                if active_resolution >= NUM_DPI as u32 {
                    bail!("G600: active resolution index {} out of range", active_resolution);
                }
                let set_buf: [u8; 4] = [
                    REPORT_ID_GET_ACTIVE,
                    0x40 | ((active_resolution as u8) << 1),
                    0x00,
                    0x00,
                ];
                io.set_feature_report(&set_buf)
                    .map_err(anyhow::Error::from)?;
            }
        }

        Ok(())
    }
}

/* ------------------------------------------------------------------ */
/* Tests                                                                */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_roundtrip() {
        for dpi in (DPI_MIN..=DPI_MAX).step_by(50) {
            let raw = dpi_to_raw(dpi).unwrap();
            assert_eq!(raw_to_dpi(raw), dpi);
        }
    }

    #[test]
    fn dpi_invalid() {
        assert!(dpi_to_raw(0).is_none());
        assert!(dpi_to_raw(150).is_none()); /* below min */
        assert!(dpi_to_raw(8250).is_none()); /* above max */
        assert!(dpi_to_raw(225).is_none()); /* not divisible by 50 */
    }

    #[test]
    fn frequency_roundtrip() {
        /* 0 → 1000 Hz */
        assert_eq!(raw_to_hz(0), 1000);
        assert_eq!(hz_to_raw(1000), 0);

        /* 1 → 500 Hz */
        assert_eq!(raw_to_hz(1), 500);
        assert_eq!(hz_to_raw(500), 1);

        /* 7 → 125 Hz */
        assert_eq!(raw_to_hz(7), 125);
        assert_eq!(hz_to_raw(125), 7);
    }

    #[test]
    fn active_profile_report_extraction() {
        /* profile=2, resolution=1: packed = (2 << 4) | (1 << 1) = 0x22 */
        let apr = ActiveProfileReport::from_bytes(&[0xF0, 0x22, 0x00, 0x00]);
        assert_eq!(apr.profile(), 2);
        assert_eq!(apr.resolution(), 1);

        /* profile=0, resolution=3: packed = (0 << 4) | (3 << 1) = 0x06 */
        let apr2 = ActiveProfileReport::from_bytes(&[0xF0, 0x06, 0x00, 0x00]);
        assert_eq!(apr2.profile(), 0);
        assert_eq!(apr2.resolution(), 3);
    }

    #[test]
    fn profile_report_roundtrip() {
        /* Build a buffer with known data, parse it, serialize it, and verify identity. */
        let mut buf = [0u8; REPORT_SIZE_PROFILE];
        buf[0] = REPORT_ID_PROFILE_0; /* id */
        buf[1] = 0xFF; /* led_red */
        buf[2] = 0x80; /* led_green */
        buf[3] = 0x00; /* led_blue */
        buf[4] = LED_BREATHE; /* led_effect */
        buf[5] = 0x05; /* led_duration */
        buf[11] = 0x01; /* frequency (500 Hz) */
        buf[13] = 0x02; /* dpi_default (slot 2, 1-indexed) */
        buf[14] = 0x10; /* dpi[0] = 800 */
        buf[15] = 0x20; /* dpi[1] = 1600 */
        buf[16] = 0x30; /* dpi[2] = 2400 */
        buf[17] = 0x40; /* dpi[3] = 3200 */

        /* Set button 0 to mouse button 1 */
        buf[31] = 0x01; /* code: mouse button */
        buf[32] = 0x00; /* modifier */
        buf[33] = 0x00; /* key */

        let report = ProfileReport::from_bytes(&buf);
        let serialized = report.to_bytes();
        assert_eq!(&buf[..], &serialized[..]);
    }

    #[test]
    fn button_action_mapping() {
        /* Mouse button 1 */
        let btn = ButtonEntry { code: 0x01, modifier: 0, key: 0 };
        let (at, val) = raw_to_action(&btn);
        assert_eq!(at, ActionType::Button);
        assert_eq!(val, 1);

        /* Keyboard key */
        let btn_key = ButtonEntry { code: 0x00, modifier: 0x02, key: 0x04 };
        let (at2, val2) = raw_to_action(&btn_key);
        assert_eq!(at2, ActionType::Key);
        assert_eq!(val2, 4);

        /* G-Shift */
        let btn_gs = ButtonEntry { code: 0x17, modifier: 0, key: 0 };
        let (at3, val3) = raw_to_action(&btn_gs);
        assert_eq!(at3, ActionType::Special);
        assert_eq!(val3, special_action::SECOND_MODE);

        /* None (all zeros) */
        let btn_none = ButtonEntry { code: 0x00, modifier: 0, key: 0 };
        let (at4, _) = raw_to_action(&btn_none);
        assert_eq!(at4, ActionType::None);
    }

    #[test]
    fn button_action_roundtrip() {
        /* Button → raw → action should round-trip for known mappings. */
        for m in BUTTON_MAP {
            let entry = action_to_raw(m.action_type, m.value);
            assert_eq!(entry.code, m.raw, "action_to_raw failed for raw={:#x}", m.raw);
            let (at, val) = raw_to_action(&entry);
            assert_eq!(at, m.action_type, "raw_to_action type mismatch for raw={:#x}", m.raw);
            assert_eq!(val, m.value, "raw_to_action value mismatch for raw={:#x}", m.raw);
        }
    }

    #[test]
    fn led_mode_roundtrip() {
        assert_eq!(hw_led_to_mode(mode_to_hw_led(LedMode::Solid)), LedMode::Solid);
        assert_eq!(hw_led_to_mode(mode_to_hw_led(LedMode::Breathing)), LedMode::Breathing);
        assert_eq!(hw_led_to_mode(mode_to_hw_led(LedMode::Cycle)), LedMode::Cycle);
    }
}
