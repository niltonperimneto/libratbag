/// Etekcity/Redragon gaming mouse driver.
///
/// Targets mice using the Etekcity USB HID protocol: Redragon M709, Etekcity
/// Scroll 1, and similar devices.  The protocol uses HID feature reports with
/// a configure-then-read pattern: a 3-byte "configure profile" request
/// selects the data type, followed by a get-feature to read it.  The device
/// requires ~100 ms delays between HID operations.
///
/// Reference implementation: `src/driver-etekcity.c`.
use anyhow::Result;
use async_trait::async_trait;
use tokio::time::{Duration, sleep};
use tracing::debug;

use crate::device::{
    ActionType, DeviceInfo, Dpi, special_action,
    RATBAG_RESOLUTION_CAP_SEPARATE_XY_RESOLUTION,
};
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                  */
/* ------------------------------------------------------------------ */

const ETEKCITY_PROFILE_MAX: u8 = 4;
const ETEKCITY_NUM_DPI: usize = 6;

/* HID report IDs */
const ETEKCITY_REPORT_ID_CONFIGURE_PROFILE: u8 = 0x04;
const ETEKCITY_REPORT_ID_PROFILE: u8 = 0x05;
const ETEKCITY_REPORT_ID_SETTINGS: u8 = 0x06;
const ETEKCITY_REPORT_ID_KEY_MAPPING: u8 = 0x07;
const ETEKCITY_REPORT_ID_MACRO: u8 = 0x09;

/* Report sizes in bytes */
const ETEKCITY_REPORT_SIZE_PROFILE: usize = 50;
const ETEKCITY_REPORT_SIZE_SETTINGS: usize = 40;
const ETEKCITY_REPORT_SIZE_MACRO: usize = 130;

/* Configuration subtypes for CONFIGURE_PROFILE */
const ETEKCITY_CONFIG_SETTINGS: u8 = 0x10;
const ETEKCITY_CONFIG_KEY_MAPPING: u8 = 0x20;

/* Available polling rates (Hz) indexed by the report_rate byte. */
const REPORT_RATES: [u32; 4] = [125, 250, 500, 1000];

/* Inter-HID-operation delay required by the device. */
const HID_DELAY: Duration = Duration::from_millis(100);

const DPI_MIN: u32 = 50;
const DPI_MAX: u32 = 8200;
const DPI_STEP: u32 = 50;

/* ------------------------------------------------------------------ */
/* Button mapping table                                                */
/* ------------------------------------------------------------------ */

struct BtnMap {
    raw: u8,
    action_type: ActionType,
    value: u32,
}

static BUTTON_MAP: &[BtnMap] = &[
    BtnMap { raw: 1,  action_type: ActionType::Button,  value: 1 },
    BtnMap { raw: 2,  action_type: ActionType::Button,  value: 2 },
    BtnMap { raw: 3,  action_type: ActionType::Button,  value: 3 },
    BtnMap { raw: 4,  action_type: ActionType::Special, value: special_action::DOUBLECLICK },
    BtnMap { raw: 6,  action_type: ActionType::None,    value: 0 },
    BtnMap { raw: 7,  action_type: ActionType::Button,  value: 4 },
    BtnMap { raw: 8,  action_type: ActionType::Button,  value: 5 },
    BtnMap { raw: 9,  action_type: ActionType::Special, value: special_action::WHEEL_UP },
    BtnMap { raw: 10, action_type: ActionType::Special, value: special_action::WHEEL_DOWN },
    BtnMap { raw: 11, action_type: ActionType::Special, value: special_action::WHEEL_LEFT },
    BtnMap { raw: 12, action_type: ActionType::Special, value: special_action::WHEEL_RIGHT },
    BtnMap { raw: 13, action_type: ActionType::Special, value: special_action::RESOLUTION_CYCLE_UP },
    BtnMap { raw: 14, action_type: ActionType::Special, value: special_action::RESOLUTION_UP },
    BtnMap { raw: 15, action_type: ActionType::Special, value: special_action::RESOLUTION_DOWN },
    BtnMap { raw: 16, action_type: ActionType::Macro,   value: 0 },
    BtnMap { raw: 18, action_type: ActionType::Special, value: special_action::PROFILE_CYCLE_UP },
    BtnMap { raw: 19, action_type: ActionType::Special, value: special_action::PROFILE_UP },
    BtnMap { raw: 20, action_type: ActionType::Special, value: special_action::PROFILE_DOWN },
];

fn raw_to_action(raw: u8) -> (ActionType, u32) {
    for m in BUTTON_MAP {
        if m.raw == raw {
            return (m.action_type, m.value);
        }
    }
    (ActionType::Unknown, u32::from(raw))
}

fn action_to_raw(action_type: ActionType, value: u32) -> u8 {
    for m in BUTTON_MAP {
        if m.action_type == action_type && m.value == value {
            return m.raw;
        }
    }
    6 /* NONE */
}

/* ------------------------------------------------------------------ */
/* DPI helpers                                                          */
/* ------------------------------------------------------------------ */

fn dpi_to_raw(dpi: u32) -> Option<u8> {
    if dpi < DPI_MIN || dpi > DPI_MAX || dpi % DPI_STEP != 0 {
        return None;
    }
    u8::try_from(dpi / DPI_STEP).ok()
}

fn raw_to_dpi(raw: u8) -> u32 {
    u32::from(raw) * DPI_STEP
}

/* ------------------------------------------------------------------ */
/* Button index mapping (protocol gap at offsets 8-12)                  */
/* ------------------------------------------------------------------ */

fn button_to_raw_index(button: usize) -> usize {
    if button < 8 { button } else { button + 5 }
}

/* ------------------------------------------------------------------ */
/* HID helpers                                                          */
/* ------------------------------------------------------------------ */

fn set_config_profile(io: &mut DeviceIo, profile: u8, config_type: u8) -> Result<()> {
    let buf = [ETEKCITY_REPORT_ID_CONFIGURE_PROFILE, profile, config_type];
    io.set_feature_report(&buf).map_err(anyhow::Error::from)?;
    Ok(())
}

/* ------------------------------------------------------------------ */
/* Cached state                                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone)]
struct SettingsReport {
    report_id: u8,
    twenty_eight: u8,
    profile_id: u8,
    x_sensitivity: u8,
    y_sensitivity: u8,
    dpi_mask: u8,
    xres: [u8; ETEKCITY_NUM_DPI],
    yres: [u8; ETEKCITY_NUM_DPI],
    current_dpi: u8,
    padding1: [u8; 7],
    report_rate: u8,
    padding2: [u8; 4],
    light: u8,
    light_heartbeat: u8,
    padding3: [u8; 5],
}

impl Default for SettingsReport {
    fn default() -> Self {
        Self {
            report_id: ETEKCITY_REPORT_ID_SETTINGS,
            twenty_eight: 0x28,
            profile_id: 0,
            x_sensitivity: 0x0a,
            y_sensitivity: 0x0a,
            dpi_mask: 0,
            xres: [0; ETEKCITY_NUM_DPI],
            yres: [0; ETEKCITY_NUM_DPI],
            current_dpi: 0,
            padding1: [0; 7],
            report_rate: 0,
            padding2: [0; 4],
            light: 0,
            light_heartbeat: 0,
            padding3: [0; 5],
        }
    }
}

impl SettingsReport {
    fn from_bytes(buf: &[u8; ETEKCITY_REPORT_SIZE_SETTINGS]) -> Self {
        let mut xres = [0u8; ETEKCITY_NUM_DPI];
        let mut yres = [0u8; ETEKCITY_NUM_DPI];
        xres.copy_from_slice(&buf[6..12]);
        yres.copy_from_slice(&buf[12..18]);
        let mut padding1 = [0u8; 7];
        padding1.copy_from_slice(&buf[19..26]);
        let mut padding2 = [0u8; 4];
        padding2.copy_from_slice(&buf[27..31]);
        let mut padding3 = [0u8; 5];
        padding3.copy_from_slice(&buf[33..38]);

        Self {
            report_id: buf[0],
            twenty_eight: buf[1],
            profile_id: buf[2],
            x_sensitivity: buf[3],
            y_sensitivity: buf[4],
            dpi_mask: buf[5],
            xres,
            yres,
            current_dpi: buf[18],
            padding1,
            report_rate: buf[26],
            padding2,
            light: buf[31],
            light_heartbeat: buf[32],
            padding3,
        }
    }

    fn to_bytes(&self) -> [u8; ETEKCITY_REPORT_SIZE_SETTINGS] {
        let mut buf = [0u8; ETEKCITY_REPORT_SIZE_SETTINGS];
        buf[0] = self.report_id;
        buf[1] = self.twenty_eight;
        buf[2] = self.profile_id;
        buf[3] = self.x_sensitivity;
        buf[4] = self.y_sensitivity;
        buf[5] = self.dpi_mask;
        buf[6..12].copy_from_slice(&self.xres);
        buf[12..18].copy_from_slice(&self.yres);
        buf[18] = self.current_dpi;
        buf[19..26].copy_from_slice(&self.padding1);
        buf[26] = self.report_rate;
        buf[27..31].copy_from_slice(&self.padding2);
        buf[31] = self.light;
        buf[32] = self.light_heartbeat;
        buf[33..38].copy_from_slice(&self.padding3);
        buf
    }
}

#[derive(Debug)]
struct EtekcityData {
    profiles: Vec<[u8; ETEKCITY_REPORT_SIZE_PROFILE]>,
    settings: Vec<SettingsReport>,
    active_profile: u8,
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct EtekcityDriver {
    data: Option<EtekcityData>,
}

impl EtekcityDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

#[async_trait]
impl DeviceDriver for EtekcityDriver {
    fn name(&self) -> &str {
        "Etekcity"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        let num_profiles = (ETEKCITY_PROFILE_MAX + 1) as usize;

        /* Query the current active profile to confirm device presence. */
        let mut profile_buf = [0u8; 3];
        profile_buf[0] = ETEKCITY_REPORT_ID_PROFILE;
        io.get_feature_report(&mut profile_buf)
            .map_err(anyhow::Error::from)?;
        let active_profile = profile_buf[2];

        /* Read settings and key-mapping reports for each profile. */
        let mut settings = Vec::with_capacity(num_profiles);
        let mut profiles = Vec::with_capacity(num_profiles);

        for pi in 0..num_profiles as u8 {
            /* Read settings report. */
            set_config_profile(io, pi, ETEKCITY_CONFIG_SETTINGS)?;
            sleep(HID_DELAY).await;
            let mut settings_buf = [0u8; ETEKCITY_REPORT_SIZE_SETTINGS];
            settings_buf[0] = ETEKCITY_REPORT_ID_SETTINGS;
            io.get_feature_report(&mut settings_buf)
                .map_err(anyhow::Error::from)?;
            settings.push(SettingsReport::from_bytes(&settings_buf));

            /* Read key-mapping report. */
            set_config_profile(io, pi, ETEKCITY_CONFIG_KEY_MAPPING)?;
            sleep(HID_DELAY).await;
            let mut profile_buf = [0u8; ETEKCITY_REPORT_SIZE_PROFILE];
            profile_buf[0] = ETEKCITY_REPORT_ID_KEY_MAPPING;
            io.get_feature_report(&mut profile_buf)
                .map_err(anyhow::Error::from)?;
            profiles.push(profile_buf);

            sleep(Duration::from_millis(10)).await;
        }

        self.data = Some(EtekcityData {
            profiles,
            settings,
            active_profile,
        });

        debug!("Etekcity: probe succeeded, active profile = {}", active_profile);
        Ok(())
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Etekcity: probe was not called"))?;

        let dpi_list: Vec<u32> = (DPI_MIN..=DPI_MAX).step_by(DPI_STEP as usize).collect();

        for (pi, profile) in info.profiles.iter_mut().enumerate() {
            let settings = &data.settings[pi];
            let key_map = &data.profiles[pi];

            /* Active profile flag. */
            profile.is_active = pi as u8 == data.active_profile;

            /* Polling rate. */
            let rate_idx = settings.report_rate as usize;
            profile.report_rate = REPORT_RATES
                .get(rate_idx)
                .copied()
                .unwrap_or(1000);
            profile.report_rates = REPORT_RATES.to_vec();

            /* Resolutions (DPI). */
            for (ri, res) in profile.resolutions.iter_mut().enumerate() {
                if ri >= ETEKCITY_NUM_DPI {
                    break;
                }
                let enabled = settings.dpi_mask & (1 << ri) != 0;
                let dpi_x = if enabled { raw_to_dpi(settings.xres[ri]) } else { 0 };
                let dpi_y = if enabled { raw_to_dpi(settings.yres[ri]) } else { 0 };
                res.dpi = Dpi::Separate { x: dpi_x, y: dpi_y };
                res.dpi_list = dpi_list.clone();
                res.is_active = ri == settings.current_dpi as usize;
                res.is_disabled = !enabled;
                res.capabilities = vec![RATBAG_RESOLUTION_CAP_SEPARATE_XY_RESOLUTION];
            }

            /* Buttons: raw data layout is 3 bytes per button at offset 3 + index*3.
             * The button_to_raw_index function handles the gap in the protocol. */
            for (bi, btn_info) in profile.buttons.iter_mut().enumerate() {
                let raw_idx = button_to_raw_index(bi);
                let offset = 3 + raw_idx * 3;
                if let Some(&raw) = key_map.get(offset) {
                    let (action_type, value) = raw_to_action(raw);
                    btn_info.action_type = action_type;
                    btn_info.mapping_value = value;
                }
            }
        }

        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("Etekcity: probe was not called"))?;

        for (pi, profile) in info.profiles.iter().enumerate() {
            if !profile.is_dirty {
                continue;
            }
            debug!("Etekcity: committing profile {}", pi);

            let settings = &mut data.settings[pi];

            /* Update resolutions in the settings report. */
            settings.x_sensitivity = 0x0a;
            settings.y_sensitivity = 0x0a;
            for (ri, res) in profile.resolutions.iter().enumerate() {
                if ri >= ETEKCITY_NUM_DPI {
                    break;
                }
                let (dpi_x, dpi_y) = match res.dpi {
                    Dpi::Separate { x, y } => (x, y),
                    Dpi::Unified(d) => (d, d),
                    Dpi::Unknown => (800, 800),
                };
                settings.xres[ri] = dpi_to_raw(dpi_x).unwrap_or(16); /* 800 fallback */
                settings.yres[ri] = dpi_to_raw(dpi_y).unwrap_or(16);

                if res.is_disabled {
                    settings.dpi_mask &= !(1 << ri);
                } else {
                    settings.dpi_mask |= 1 << ri;
                }

                if res.is_active {
                    settings.current_dpi = ri as u8;
                }
            }

            /* Write the settings report. */
            set_config_profile(io, pi as u8, ETEKCITY_CONFIG_SETTINGS)?;
            sleep(HID_DELAY).await;
            let settings_buf = settings.to_bytes();
            io.set_feature_report(&settings_buf)
                .map_err(anyhow::Error::from)?;
            sleep(HID_DELAY).await;

            /* Update buttons in the key-mapping report. */
            let key_map = &mut data.profiles[pi];
            for (bi, btn_info) in profile.buttons.iter().enumerate() {
                let raw_idx = button_to_raw_index(bi);
                let offset = 3 + raw_idx * 3;
                if offset < ETEKCITY_REPORT_SIZE_PROFILE {
                    key_map[offset] = action_to_raw(btn_info.action_type, btn_info.mapping_value);
                }
            }

            /* Write the key-mapping report. */
            set_config_profile(io, pi as u8, ETEKCITY_CONFIG_KEY_MAPPING)?;
            sleep(HID_DELAY).await;
            io.set_feature_report(key_map)
                .map_err(anyhow::Error::from)?;
            sleep(HID_DELAY).await;
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
        for dpi in (DPI_MIN..=DPI_MAX).step_by(DPI_STEP as usize) {
            let raw = dpi_to_raw(dpi).unwrap();
            assert_eq!(raw_to_dpi(raw), dpi);
        }
    }

    #[test]
    fn dpi_invalid() {
        assert!(dpi_to_raw(0).is_none());
        assert!(dpi_to_raw(25).is_none()); /* below min */
        assert!(dpi_to_raw(8250).is_none()); /* above max */
        assert!(dpi_to_raw(75).is_none()); /* not divisible by 50 */
    }

    #[test]
    fn button_index_gap() {
        /* Buttons 0-7 map linearly. */
        for i in 0..8 {
            assert_eq!(button_to_raw_index(i), i);
        }
        /* Buttons 8-9 skip 5 positions. */
        assert_eq!(button_to_raw_index(8), 13);
        assert_eq!(button_to_raw_index(9), 14);
    }

    #[test]
    fn button_action_roundtrip() {
        for m in BUTTON_MAP {
            let raw = action_to_raw(m.action_type, m.value);
            assert_eq!(raw, m.raw, "action_to_raw failed for raw={}", m.raw);
            let (at, val) = raw_to_action(raw);
            assert_eq!(at, m.action_type, "raw_to_action type mismatch for raw={}", m.raw);
            assert_eq!(val, m.value, "raw_to_action value mismatch for raw={}", m.raw);
        }
    }

    #[test]
    fn settings_report_roundtrip() {
        let mut buf = [0u8; ETEKCITY_REPORT_SIZE_SETTINGS];
        buf[0] = ETEKCITY_REPORT_ID_SETTINGS;
        buf[1] = 0x28;
        buf[2] = 2; /* profile 2 */
        buf[3] = 0x0a; /* x_sensitivity */
        buf[4] = 0x0a; /* y_sensitivity */
        buf[5] = 0x3f; /* dpi_mask: all 6 enabled */
        buf[6] = 0x04; /* xres[0] = 200 DPI */
        buf[12] = 0x04; /* yres[0] = 200 DPI */
        buf[18] = 0; /* current_dpi = slot 0 */
        buf[26] = 3; /* report_rate = 1000 Hz */

        let report = SettingsReport::from_bytes(&buf);
        let serialized = report.to_bytes();
        assert_eq!(&buf[..], &serialized[..]);
    }
}
