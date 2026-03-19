/// MarsGaming MM4 gaming mouse driver.
///
/// Targets MarsGaming MM4 mice using the proprietary MarsGaming HID protocol.
/// Features: 5 profiles, up to 5 DPI resolutions per profile, 19 buttons, 1 LED zone.
///
/// The protocol uses HID feature reports with a set-then-get pattern:
/// a 16-byte command is sent via set_feature_report (report ID 0x02), then the
/// response is read via get_feature_report with a report-type-specific ID
/// (0x03 for resolutions, 0x04 for buttons/LEDs).  LED colors are inverted:
/// 0x00 = fully bright, 0xFF = fully dark.
///
/// Reference implementation: `src/driver-marsgaming/`.
use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::device::{
    ActionType, Color, DeviceInfo, Dpi, LedMode, special_action,
    RATBAG_RESOLUTION_CAP_SEPARATE_XY_RESOLUTION,
};
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

const NUM_PROFILES: usize = 5;
const NUM_RESOLUTIONS: usize = 5;
const RES_SCALING: u32 = 50;
const RES_MIN: u32 = 50;
const RES_MAX: u32 = 16400;

const REPORT_RATES: [u32; 4] = [125, 250, 500, 1000];
const LED_MODES: [LedMode; 3] = [LedMode::Off, LedMode::Solid, LedMode::Breathing];

/* Report type constants used in the command byte. */
const REPORT_TYPE_WRITE: u8 = 0x02;
const REPORT_TYPE_READ: u8 = 0x03;

/* ------------------------------------------------------------------ */
/* Button action enum                                                   */
/* ------------------------------------------------------------------ */

fn raw_button_to_action(action: u8, params: &[u8; 3]) -> (ActionType, u32) {
    match action {
        0x01 => (ActionType::Button, 1), /* left click */
        0x02 => (ActionType::Button, 2), /* right click */
        0x03 => (ActionType::Button, 3), /* middle click */
        0x04 => (ActionType::Button, 4), /* backward */
        0x05 => (ActionType::Button, 5), /* forward */
        0x08 => (ActionType::Special, special_action::RESOLUTION_CYCLE_UP),
        0x09 => (ActionType::Special, special_action::RESOLUTION_DOWN),
        0x0a => (ActionType::Special, special_action::RESOLUTION_UP),
        0x0d => (ActionType::Special, special_action::PROFILE_CYCLE_UP),
        0x0e if params[0] == 0 && params[1] == 0 && params[2] == 0 => {
            (ActionType::None, 0) /* disable */
        }
        0x0e => (ActionType::Key, u32::from(params[1])), /* media key */
        0x0f => (ActionType::Key, u32::from(params[1])), /* combo key */
        0x10 => (ActionType::Key, u32::from(params[1])), /* single key */
        0x11 => (ActionType::Macro, u32::from(params[0])), /* macro */
        _ => (ActionType::Unknown, u32::from(action)),
    }
}

fn action_to_raw_button(action_type: ActionType, value: u32) -> [u8; 4] {
    match action_type {
        ActionType::Button => {
            let code = match value {
                1 => 0x01,
                2 => 0x02,
                3 => 0x03,
                4 => 0x04,
                5 => 0x05,
                _ => 0x0e, /* disable */
            };
            [code, 0, 0, 0]
        }
        ActionType::Special => {
            let code = match value {
                v if v == special_action::RESOLUTION_CYCLE_UP => 0x08,
                v if v == special_action::RESOLUTION_DOWN => 0x09,
                v if v == special_action::RESOLUTION_UP => 0x0a,
                v if v == special_action::PROFILE_CYCLE_UP => 0x0d,
                _ => 0x0e,
            };
            [code, 0, 0, 0]
        }
        ActionType::Key => [0x10, 0, value as u8, 0],
        ActionType::Macro => [0x11, value as u8, 0, 0],
        ActionType::None | _ => [0x0e, 0, 0, 0],
    }
}

/* ------------------------------------------------------------------ */
/* Query / command helpers                                              */
/* ------------------------------------------------------------------ */

fn mars_query(
    io: &mut DeviceIo,
    cmd_bytes: &[u8; 8],
    response_id: u8,
    response_buf: &mut [u8],
) -> Result<()> {
    let mut cmd = [0u8; 16];
    cmd[..8].copy_from_slice(cmd_bytes);
    cmd[0] = 0x02; /* set_feature report ID */
    io.set_feature_report(&cmd).map_err(anyhow::Error::from)?;

    response_buf[0] = response_id;
    io.get_feature_report(response_buf)
        .map_err(anyhow::Error::from)?;
    Ok(())
}

fn mars_write_cmd(io: &mut DeviceIo, cmd_bytes: &[u8]) -> Result<()> {
    let mut cmd = [0u8; 16];
    let len = cmd_bytes.len().min(16);
    cmd[..len].copy_from_slice(&cmd_bytes[..len]);
    cmd[0] = 0x02;
    io.set_feature_report(&cmd).map_err(anyhow::Error::from)?;
    Ok(())
}

/* ------------------------------------------------------------------ */
/* Per-profile cached data                                              */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct ProfileData {
    /* Raw resolution report bytes (64 bytes). */
    res_raw: [u8; 64],
    /* Raw button report bytes (1024 bytes). */
    btn_raw: Box<[u8; 1024]>,
    /* Raw LED report bytes (16 bytes). */
    led_raw: [u8; 16],
    /* Polling interval as reported by hardware. */
    polling_interval: u8,
}

impl Default for ProfileData {
    fn default() -> Self {
        Self {
            res_raw: [0u8; 64],
            btn_raw: Box::new([0u8; 1024]),
            led_raw: [0u8; 16],
            polling_interval: 1,
        }
    }
}

/* ------------------------------------------------------------------ */
/* Device-level cached state                                            */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct MarsData {
    profiles: Vec<ProfileData>,
    active_profile: u8,
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct MarsGamingDriver {
    data: Option<MarsData>,
}

impl MarsGamingDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

#[async_trait]
impl DeviceDriver for MarsGamingDriver {
    fn name(&self) -> &str {
        "MarsGaming MM4"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Query current active profile. */
        let mut resp16 = [0u8; 16];
        mars_query(
            io,
            &[0x02, REPORT_TYPE_READ, 0x43, 0x00, 0x01, 0x00, 0xfa, 0xfa],
            0x02,
            &mut resp16,
        )?;
        let active_profile = resp16[8];

        /* Read all profiles: resolutions, buttons, LEDs, polling rate. */
        let mut profiles = Vec::with_capacity(NUM_PROFILES);
        for pi in 0..NUM_PROFILES as u8 {
            let mut pd = ProfileData::default();

            /* Resolution report (64 bytes). */
            mars_query(
                io,
                &[0x02, REPORT_TYPE_READ, 0x4f, pi, 0x2a, 0x00, 0xfa, 0xfa],
                0x03,
                &mut pd.res_raw,
            )?;

            /* Button report (1024 bytes). */
            mars_query(
                io,
                &[0x02, REPORT_TYPE_READ, 0x90, pi, 0x4d, 0x00, 0xfa, 0xfa],
                0x04,
                pd.btn_raw.as_mut(),
            )?;

            /* LED report (16 bytes). */
            let mut led_buf = [0u8; 16];
            mars_query(
                io,
                &[0x02, REPORT_TYPE_READ, 0xf1, pi, 0x06, 0x00, 0xfa, 0xfa],
                0x04,
                &mut led_buf,
            )?;
            pd.led_raw = led_buf;

            /* Polling interval. */
            let mut poll_buf = [0u8; 16];
            mars_query(
                io,
                &[0x02, REPORT_TYPE_READ, 0x48 | pi, 0x00, 0x01, 0x00, 0xfa, 0xfa],
                0x02,
                &mut poll_buf,
            )?;
            pd.polling_interval = poll_buf[8].max(1);

            profiles.push(pd);
        }

        self.data = Some(MarsData {
            profiles,
            active_profile,
        });

        debug!("MarsGaming: probe succeeded, active profile = {}", active_profile);
        Ok(())
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("MarsGaming: probe was not called"))?;

        let dpi_list: Vec<u32> = (RES_MIN..=RES_MAX).step_by(RES_SCALING as usize).collect();

        for (pi, profile) in info.profiles.iter_mut().enumerate() {
            let pd = &data.profiles[pi];

            profile.is_active = pi as u8 == data.active_profile;

            /* Polling rate. */
            let hz = 1000 / u32::from(pd.polling_interval).max(1);
            profile.report_rate = hz;
            profile.report_rates = REPORT_RATES.to_vec();

            /* Parse resolution report.
             * Layout: [0]=report_id, [1]=type, [2]=unknown, [3]=profile_id,
             * [4..8]=unknowns, [8]=count_resolutions, [9]=current_resolution,
             * then 7-byte entries starting at offset 10. */
            let res_buf = &pd.res_raw;
            let count_res = (res_buf[8] as usize).min(NUM_RESOLUTIONS);
            let current_res = res_buf[9];

            for (ri, res) in profile.resolutions.iter_mut().enumerate() {
                if ri >= count_res {
                    res.is_disabled = true;
                    continue;
                }
                let base = 10 + ri * 7;
                let enabled = res_buf.get(base).copied().unwrap_or(0) != 0;
                let x_raw = u16::from_le_bytes([
                    res_buf.get(base + 1).copied().unwrap_or(0),
                    res_buf.get(base + 2).copied().unwrap_or(0),
                ]);
                let y_raw = u16::from_le_bytes([
                    res_buf.get(base + 3).copied().unwrap_or(0),
                    res_buf.get(base + 4).copied().unwrap_or(0),
                ]);
                let dpi_x = u32::from(x_raw) * RES_SCALING;
                let dpi_y = u32::from(y_raw) * RES_SCALING;

                res.dpi = Dpi::Separate { x: dpi_x, y: dpi_y };
                res.dpi_list = dpi_list.clone();
                res.is_active = ri as u8 == current_res;
                res.is_disabled = !enabled;
                res.capabilities = vec![RATBAG_RESOLUTION_CAP_SEPARATE_XY_RESOLUTION];
            }

            /* Parse button report.
             * Layout: [0]=report_id, [1]=type, ... [8]=button_count,
             * then 4-byte entries starting at offset 9. */
            let btn_buf = pd.btn_raw.as_ref();
            for (bi, btn_info) in profile.buttons.iter_mut().enumerate() {
                let base = 9 + bi * 4;
                if base + 3 >= 1024 {
                    break;
                }
                let action = btn_buf[base];
                let params = [btn_buf[base + 1], btn_buf[base + 2], btn_buf[base + 3]];
                let (action_type, value) = raw_button_to_action(action, &params);
                btn_info.action_type = action_type;
                btn_info.mapping_value = value;
            }

            /* Parse LED report.
             * Layout: [0..7]=header, [8]=red_inv, [9]=green_inv, [10]=blue_inv,
             * [11]=brightness, [12]=breathing_speed. */
            if let Some(led) = profile.leds.first_mut() {
                led.modes = LED_MODES.to_vec();
                led.color_depth = 3;

                let led_buf = &pd.led_raw;
                let red = 0xFF_u8.wrapping_sub(led_buf[8]);
                let green = 0xFF_u8.wrapping_sub(led_buf[9]);
                let blue = 0xFF_u8.wrapping_sub(led_buf[10]);
                let brightness_raw = led_buf[11]; /* 0-3 */
                let breathing_speed = led_buf[12];

                led.color = Color {
                    red: u32::from(red),
                    green: u32::from(green),
                    blue: u32::from(blue),
                };
                led.brightness = u32::from(brightness_raw) * (255 / 3);

                if brightness_raw == 0 {
                    led.mode = LedMode::Off;
                } else if breathing_speed == 0 || breathing_speed >= 10 {
                    led.mode = LedMode::Solid;
                } else {
                    led.mode = LedMode::Breathing;
                    led.effect_duration = u32::from(breathing_speed) * 2000;
                }
            }
        }

        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("MarsGaming: probe was not called"))?;

        for (pi, profile) in info.profiles.iter().enumerate() {
            if !profile.is_dirty {
                continue;
            }
            debug!("MarsGaming: committing profile {}", pi);
            let pd = &mut data.profiles[pi];

            /* Write polling rate. */
            let interval = (1000 / profile.report_rate.max(1)) as u8;
            mars_write_cmd(
                io,
                &[0x02, REPORT_TYPE_WRITE, 0x48 | (pi as u8), 0x00, 0x01, 0x00, 0xfa, 0xfa,
                  interval, 0, 0, 0, 0, 0, 0, 0],
            )?;

            /* Write resolutions.
             * Copy the read report and flip to write mode. */
            let mut res_write = pd.res_raw;
            res_write[1] = REPORT_TYPE_WRITE;
            res_write[6] = 0xfa;
            res_write[7] = 0xfa;

            for (ri, res) in profile.resolutions.iter().enumerate() {
                let base = 10 + ri * 7;
                if base + 6 >= 64 {
                    break;
                }
                let (dpi_x, dpi_y) = match res.dpi {
                    Dpi::Separate { x, y } => (x, y),
                    Dpi::Unified(d) => (d, d),
                    Dpi::Unknown => continue,
                };
                let x_raw = (dpi_x / RES_SCALING) as u16;
                let y_raw = (dpi_y / RES_SCALING) as u16;
                res_write[base] = if res.is_disabled { 0 } else { 1 };
                let x_bytes = x_raw.to_le_bytes();
                let y_bytes = y_raw.to_le_bytes();
                res_write[base + 1] = x_bytes[0];
                res_write[base + 2] = x_bytes[1];
                res_write[base + 3] = y_bytes[0];
                res_write[base + 4] = y_bytes[1];
            }
            io.set_feature_report(&res_write)
                .map_err(anyhow::Error::from)?;

            /* Write buttons.
             * Copy the read report and flip to write mode. */
            let mut btn_write = pd.btn_raw.clone();
            btn_write[1] = REPORT_TYPE_WRITE;
            btn_write[6] = 0xfa;
            btn_write[7] = 0xfa;

            for (bi, btn_info) in profile.buttons.iter().enumerate() {
                let base = 9 + bi * 4;
                if base + 3 >= 1024 {
                    break;
                }
                let raw = action_to_raw_button(btn_info.action_type, btn_info.mapping_value);
                btn_write[base] = raw[0];
                btn_write[base + 1] = raw[1];
                btn_write[base + 2] = raw[2];
                btn_write[base + 3] = raw[3];
            }
            io.set_feature_report(btn_write.as_ref())
                .map_err(anyhow::Error::from)?;

            /* Write LED.
             * Active profile gets a direct live-color command; all profiles
             * get the stored LED report. */
            if let Some(led) = profile.leds.first() {
                let red_inv = 0xFF_u8.wrapping_sub(led.color.red.min(255) as u8);
                let green_inv = 0xFF_u8.wrapping_sub(led.color.green.min(255) as u8);
                let blue_inv = 0xFF_u8.wrapping_sub(led.color.blue.min(255) as u8);

                let (brightness, breathing) = match led.mode {
                    LedMode::Off => (0u8, 0u8),
                    LedMode::Solid => ((led.brightness.min(255) as u8) * 3 / 255, 0u8),
                    LedMode::Breathing => {
                        let br = (led.brightness.min(255) as u8) * 3 / 255;
                        let speed = (led.effect_duration / 2000) as u8;
                        (br, speed)
                    }
                    _ => (0u8, 0u8),
                };

                /* Live command for active profile. */
                if profile.is_active {
                    mars_write_cmd(
                        io,
                        &[0x02, 0x04, red_inv, green_inv, blue_inv, brightness, breathing, 0x01,
                          0, 0, 0, 0, 0, 0, 0, 0],
                    )?;
                }

                /* Stored LED report. */
                mars_write_cmd(
                    io,
                    &[0x02, REPORT_TYPE_WRITE, 0xf1, pi as u8, 0x06, 0x00, 0xfa, 0xfa,
                      red_inv, green_inv, blue_inv, brightness, breathing, 0, 0, 0],
                )?;
            }
        }

        Ok(())
    }
}

/* ------------------------------------------------------------------ */
/* Helpers                                                              */
/* ------------------------------------------------------------------ */

fn dpi_to_raw(dpi: u32) -> Option<u16> {
    if dpi < RES_MIN || dpi > RES_MAX || dpi % RES_SCALING != 0 {
        return None;
    }
    u16::try_from(dpi / RES_SCALING).ok()
}

fn raw_to_dpi(raw: u16) -> u32 {
    u32::from(raw) * RES_SCALING
}

/* ------------------------------------------------------------------ */
/* Tests                                                                */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpi_roundtrip() {
        for dpi in (RES_MIN..=RES_MAX).step_by(RES_SCALING as usize) {
            let raw = dpi_to_raw(dpi).unwrap();
            assert_eq!(raw_to_dpi(raw), dpi);
        }
    }

    #[test]
    fn led_color_inversion_roundtrip() {
        for val in 0..=255u8 {
            let inverted = 0xFF_u8.wrapping_sub(val);
            let restored = 0xFF_u8.wrapping_sub(inverted);
            assert_eq!(restored, val);
        }
    }

    #[test]
    fn button_action_mapping() {
        /* Left click */
        let (at, v) = raw_button_to_action(0x01, &[0, 0, 0]);
        assert_eq!(at, ActionType::Button);
        assert_eq!(v, 1);

        /* DPI cycle up */
        let (at2, v2) = raw_button_to_action(0x08, &[0, 0, 0]);
        assert_eq!(at2, ActionType::Special);
        assert_eq!(v2, special_action::RESOLUTION_CYCLE_UP);

        /* Disable (0x0e with zero params) */
        let (at3, _) = raw_button_to_action(0x0e, &[0, 0, 0]);
        assert_eq!(at3, ActionType::None);

        /* Media key (0x0e with nonzero params) */
        let (at4, v4) = raw_button_to_action(0x0e, &[0, 0x42, 0]);
        assert_eq!(at4, ActionType::Key);
        assert_eq!(v4, 0x42);
    }

    #[test]
    fn button_roundtrip() {
        /* Button 1 */
        let raw = action_to_raw_button(ActionType::Button, 1);
        assert_eq!(raw[0], 0x01);
        let (at, v) = raw_button_to_action(raw[0], &[raw[1], raw[2], raw[3]]);
        assert_eq!(at, ActionType::Button);
        assert_eq!(v, 1);
    }
}
