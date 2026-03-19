/// SinoWealth-based gaming mouse driver.
///
/// Covers mice using the SinoWealth HID protocol: Glorious Model O/O-,
/// G-Wolves Skoll, Genesis Xenon 770, DreamMachines DM5, and similar devices.
///
/// The protocol uses HID feature reports for all communication. Configuration
/// data is accessed via a set-then-get pattern: first set a 6-byte command
/// report (ID 0x05) to select what to read, then get a 520-byte config report
/// (ID 0x04 or 0x06) containing the response data. Writes are done by setting
/// the 520-byte report directly with a `config_write` byte set to `size - 8`.
///
/// Key protocol features: up to 3 profiles, 8 DPI slots per profile, 20
/// programmable buttons, sensor-aware DPI encoding (PMW3360/3327 vs PMW3389),
/// and RGB/RBG color byte ordering depending on device model.
///
/// Reference implementation: `src/driver-sinowealth.c`.
use anyhow::Result;
use async_trait::async_trait;
use tracing::debug;

use crate::device::{ActionType, Color, DeviceInfo, Dpi, LedMode, RgbColor, special_action};
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Report IDs and sizes                                                 */
/* ------------------------------------------------------------------ */

const REPORT_ID_CONFIG: u8 = 0x04;
const REPORT_ID_CMD: u8 = 0x05;
const REPORT_ID_CONFIG_LONG: u8 = 0x06;

const CMD_SIZE: usize = 6;
const CONFIG_REPORT_SIZE: usize = 520;
const CONFIG_SIZE_MAX: usize = 167;
const CONFIG_SIZE_MIN: usize = 123;
const BUTTON_REPORT_SIZE: usize = 88;

/* ------------------------------------------------------------------ */
/* Command IDs                                                          */
/* ------------------------------------------------------------------ */

const CMD_FIRMWARE_VERSION: u8 = 0x01;
const CMD_PROFILE: u8 = 0x02;
const CMD_DEBOUNCE: u8 = 0x1a;

/* Per-profile config/button command IDs. */
const CMD_GET_CONFIG: [u8; 3] = [0x11, 0x21, 0x31];
const CMD_GET_BUTTONS: [u8; 3] = [0x12, 0x22, 0x32];

/* ------------------------------------------------------------------ */
/* Config report byte offsets                                           */
/* ------------------------------------------------------------------ */

const OFF_REPORT_ID: usize = 0;
const OFF_COMMAND_ID: usize = 1;
const OFF_CONFIG_WRITE: usize = 3;
const OFF_SENSOR: usize = 9;
const OFF_RATE_FLAGS: usize = 10;
const OFF_DPI_COUNT: usize = 11;
const OFF_DISABLED_DPI: usize = 12;
const OFF_DPIS: usize = 13;
const OFF_RGB_EFFECT: usize = 53;
const OFF_SINGLE_MODE: usize = 56;
const OFF_SINGLE_COLOR: usize = 57;
const OFF_BREATHING7_MODE: usize = 60;
const OFF_BREATHING7_COLORCOUNT: usize = 61;
const OFF_BREATHING7_COLORS: usize = 62;
const OFF_GLORIOUS_MODE: usize = 54;

/* Button report byte offsets. */
const OFF_BTN_DATA: usize = 8;

/* ------------------------------------------------------------------ */
/* Sensor types                                                         */
/* ------------------------------------------------------------------ */

const SENSOR_PMW3360: u8 = 0x06;
const SENSOR_PMW3327: u8 = 0x0e;
const SENSOR_PMW3389: u8 = 0x0f;

/* ------------------------------------------------------------------ */
/* Button types                                                         */
/* ------------------------------------------------------------------ */

const BTN_TYPE_NONE: u8 = 0x00;
const BTN_TYPE_BUTTON: u8 = 0x11;
const BTN_TYPE_WHEEL: u8 = 0x12;
const BTN_TYPE_KEY: u8 = 0x21;
const BTN_TYPE_MULTIMEDIA: u8 = 0x22;
const BTN_TYPE_SWITCH_DPI: u8 = 0x41;
const BTN_TYPE_DPI_LOCK: u8 = 0x42;
const BTN_TYPE_SPECIAL: u8 = 0x50;
const BTN_TYPE_MACRO: u8 = 0x70;

/* ------------------------------------------------------------------ */
/* RGB effect modes                                                     */
/* ------------------------------------------------------------------ */

const RGB_OFF: u8 = 0x00;
const RGB_SINGLE: u8 = 0x02;
const RGB_BREATHING7: u8 = 0x03;
const RGB_BREATHING1: u8 = 0x0a;
const RGB_NOT_SUPPORTED: u8 = 0xff;

/* XY independent DPI flag in config_flags nibble. */
const XY_INDEPENDENT: u8 = 0x08;

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

const NUM_DPIS: usize = 8;
const NUM_PROFILES_MAX: usize = 3;
const NUM_BUTTONS_HW: usize = 20;
const DPI_STEP: u32 = 100;
const DPI_MIN: u32 = 100;

/* Report rates indexed from 1. */
const REPORT_RATES: &[u32] = &[125, 250, 500, 1000];
const DEBOUNCE_TIMES: &[u32] = &[4, 6, 8, 10, 12, 14, 16];

/* ------------------------------------------------------------------ */
/* Color format                                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LedFormat {
    Rgb,
    Rbg,
}

/* ------------------------------------------------------------------ */
/* Cached hardware state                                                */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct SinoweathData {
    firmware_version: [u8; 4],
    sensor: u8,
    active_profile: u8,
    config_size: usize,
    is_long: bool,
    led_format: LedFormat,
    profile_count: usize,
    /* Raw 520-byte config and button reports per profile. */
    configs: [Box<[u8; CONFIG_REPORT_SIZE]>; NUM_PROFILES_MAX],
    buttons: [Box<[u8; CONFIG_REPORT_SIZE]>; NUM_PROFILES_MAX],
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct SinowealhDriver {
    data: Option<SinoweathData>,
}

impl SinowealhDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

/* ------------------------------------------------------------------ */
/* I/O helpers                                                          */
/* ------------------------------------------------------------------ */

/* Send a short command and read back the 6-byte response. */
fn sw_query_cmd(io: &DeviceIo, cmd: u8) -> Result<[u8; CMD_SIZE]> {
    let mut buf = [0u8; CMD_SIZE];
    buf[0] = REPORT_ID_CMD;
    buf[1] = cmd;

    io.set_feature_report(&buf)
        .map_err(anyhow::Error::from)?;

    buf[0] = REPORT_ID_CMD;
    io.get_feature_report(&mut buf)
        .map_err(anyhow::Error::from)?;

    if buf[1] != cmd {
        anyhow::bail!(
            "SinoWealth: command {cmd:#04x} response mismatch (got {:#04x})",
            buf[1]
        );
    }

    Ok(buf)
}

/* Write a short command (no read-back). */
fn sw_write_cmd(io: &DeviceIo, buf: &[u8; CMD_SIZE]) -> Result<()> {
    io.set_feature_report(buf)
        .map_err(anyhow::Error::from)?;
    Ok(())
}

/* Read a 520-byte config/button report from the device.
 *
 * The SinoWealth protocol is: first write a CMD report to select what
 * to read, then get_feature_report on the CONFIG report ID. */
fn sw_read_config(
    io: &DeviceIo,
    config_cmd: u8,
    report_id: u8,
) -> Result<Box<[u8; CONFIG_REPORT_SIZE]>> {
    /* Step 1: Send command to select which config to read. */
    let cmd = [REPORT_ID_CMD, config_cmd, 0, 0, 0, 0];
    io.set_feature_report(&cmd)
        .map_err(anyhow::Error::from)?;

    /* Step 2: Read the config report. */
    let mut report = Box::new([0u8; CONFIG_REPORT_SIZE]);
    report[0] = report_id;
    io.get_feature_report(report.as_mut())
        .map_err(anyhow::Error::from)?;

    Ok(report)
}

/* Write a 520-byte config/button report to the device. */
fn sw_write_config(io: &DeviceIo, report: &[u8; CONFIG_REPORT_SIZE]) -> Result<()> {
    io.set_feature_report(report)
        .map_err(anyhow::Error::from)?;
    Ok(())
}

/* ------------------------------------------------------------------ */
/* DPI conversion (sensor-aware)                                        */
/* ------------------------------------------------------------------ */

/* PMW3360/PMW3327: dpi = (raw + 1) * 100
 * PMW3389: dpi = raw * 100 */
fn raw_to_dpi(sensor: u8, raw: u8) -> u32 {
    let r = u32::from(raw);
    match sensor {
        SENSOR_PMW3360 | SENSOR_PMW3327 => (r + 1) * DPI_STEP,
        _ => r * DPI_STEP, /* PMW3389 and unknown sensors */
    }
}

fn dpi_to_raw(sensor: u8, dpi: u32) -> u8 {
    let r = dpi / DPI_STEP;
    match sensor {
        SENSOR_PMW3360 | SENSOR_PMW3327 => r.saturating_sub(1) as u8,
        _ => r as u8,
    }
}

fn max_dpi_for_sensor(sensor: u8) -> u32 {
    match sensor {
        SENSOR_PMW3327 => 10200,
        0x08 => 7200, /* PMW3212 */
        SENSOR_PMW3360 => 12000,
        SENSOR_PMW3389 => 16000,
        _ => 2000, /* fallback */
    }
}

/* ------------------------------------------------------------------ */
/* Report rate conversion                                               */
/* ------------------------------------------------------------------ */

/* Raw values are 1-indexed: 1=125, 2=250, 3=500, 4=1000. */
fn raw_to_rate(raw: u8) -> u32 {
    match raw {
        1 => 125,
        2 => 250,
        3 => 500,
        4 => 1000,
        _ => 0,
    }
}

fn rate_to_raw(rate: u32) -> u8 {
    match rate {
        125 => 1,
        250 => 2,
        500 => 3,
        1000 => 4,
        _ => 0,
    }
}

/* ------------------------------------------------------------------ */
/* Color conversion (RGB/RBG)                                           */
/* ------------------------------------------------------------------ */

fn raw_to_color(format: LedFormat, data: &[u8]) -> RgbColor {
    match format {
        LedFormat::Rgb => RgbColor { r: data[0], g: data[1], b: data[2] },
        LedFormat::Rbg => RgbColor { r: data[0], g: data[2], b: data[1] },
    }
}

fn color_to_raw(format: LedFormat, color: RgbColor) -> [u8; 3] {
    match format {
        LedFormat::Rgb => [color.r, color.g, color.b],
        LedFormat::Rbg => [color.r, color.b, color.g],
    }
}

/* ------------------------------------------------------------------ */
/* RGB mode helpers                                                     */
/* ------------------------------------------------------------------ */

/* The rgb_mode byte packs speed (low nibble) and brightness (high nibble). */
fn rgb_mode_brightness(mode_byte: u8) -> u32 {
    let raw = (mode_byte >> 4) & 0x0f;
    (u32::from(raw) * 64).min(255)
}

fn brightness_to_rgb_mode(brightness: u32) -> u8 {
    ((brightness as u8).wrapping_add(1)) / 64
}

fn rgb_mode_duration(mode_byte: u8) -> u32 {
    match mode_byte & 0x0f {
        0 => 10000,
        1 => 1500,
        2 => 1000,
        3 => 500,
        _ => 0,
    }
}

fn duration_to_rgb_mode(duration: u32) -> u8 {
    if duration <= 500 { 3 }
    else if duration <= 1000 { 2 }
    else { 1 }
}

/* ------------------------------------------------------------------ */
/* Button parsing                                                       */
/* ------------------------------------------------------------------ */

/* Parse a 4-byte button entry into (ActionType, mapping_value). */
fn parse_button(btn: &[u8]) -> (ActionType, u32) {
    let btn_type = btn[0];
    let d = &btn[1..4];

    match btn_type {
        BTN_TYPE_BUTTON => {
            let button = match d[0] {
                0x01 => 1,
                0x02 => 2,
                0x04 => 3,
                0x08 => 5,
                0x10 => 4,
                _ => 0,
            };
            (ActionType::Button, button)
        }
        BTN_TYPE_WHEEL => {
            if d[0] == 0x01 {
                (ActionType::Special, special_action::WHEEL_UP)
            } else {
                (ActionType::Special, special_action::WHEEL_DOWN)
            }
        }
        BTN_TYPE_KEY => {
            /* d[0] = modifier mask, d[1] = HID key code */
            (ActionType::Key, u32::from(d[1]))
        }
        BTN_TYPE_MULTIMEDIA => {
            /* Multimedia keys are bitmasks across 3 data bytes. */
            (ActionType::Key, u32::from_le_bytes([d[0], d[1], d[2], 0]))
        }
        BTN_TYPE_SWITCH_DPI => match d[0] {
            0x00 => (ActionType::Special, special_action::RESOLUTION_CYCLE_UP),
            0x01 => (ActionType::Special, special_action::RESOLUTION_UP),
            0x02 => (ActionType::Special, special_action::RESOLUTION_DOWN),
            _ => (ActionType::Special, special_action::RESOLUTION_CYCLE_UP),
        },
        BTN_TYPE_DPI_LOCK => {
            /* d[0] = DPI / 100 */
            (ActionType::Special, special_action::RESOLUTION_DEFAULT)
        }
        BTN_TYPE_SPECIAL => match d[0] {
            0x01 => (ActionType::None, 0),
            0x06 => (ActionType::Special, special_action::PROFILE_CYCLE_UP),
            _ => (ActionType::Special, special_action::UNKNOWN),
        },
        BTN_TYPE_MACRO => (ActionType::Macro, 0),
        BTN_TYPE_NONE => (ActionType::None, 0),
        _ => {
            debug!("SinoWealth: unknown button type {btn_type:#04x}");
            (ActionType::Unknown, 0)
        }
    }
}

/* Encode a button action back to 4-byte hardware format. */
fn encode_button(action_type: ActionType, value: u32, buf: &mut [u8]) {
    buf[0..4].fill(0);

    match action_type {
        ActionType::Button => {
            buf[0] = BTN_TYPE_BUTTON;
            buf[1] = match value {
                1 => 0x01,
                2 => 0x02,
                3 => 0x04,
                5 => 0x08,
                4 => 0x10,
                _ => 0x01,
            };
        }
        ActionType::Special => match value {
            special_action::WHEEL_UP => {
                buf[0] = BTN_TYPE_WHEEL;
                buf[1] = 0x01;
            }
            special_action::WHEEL_DOWN => {
                buf[0] = BTN_TYPE_WHEEL;
                buf[1] = 0xff;
            }
            special_action::RESOLUTION_CYCLE_UP => {
                buf[0] = BTN_TYPE_SWITCH_DPI;
                buf[1] = 0x00;
            }
            special_action::RESOLUTION_UP => {
                buf[0] = BTN_TYPE_SWITCH_DPI;
                buf[1] = 0x01;
            }
            special_action::RESOLUTION_DOWN => {
                buf[0] = BTN_TYPE_SWITCH_DPI;
                buf[1] = 0x02;
            }
            special_action::PROFILE_CYCLE_UP => {
                buf[0] = BTN_TYPE_SPECIAL;
                buf[1] = 0x06;
            }
            _ => {
                buf[0] = BTN_TYPE_SPECIAL;
                buf[1] = 0x01; /* disable */
            }
        },
        ActionType::Key => {
            buf[0] = BTN_TYPE_KEY;
            buf[2] = value as u8;
        }
        ActionType::Macro => {
            buf[0] = BTN_TYPE_MACRO;
        }
        ActionType::None | ActionType::Unknown => {
            buf[0] = BTN_TYPE_SPECIAL;
            buf[1] = 0x01; /* disable action */
        }
    }
}

/* ------------------------------------------------------------------ */
/* DeviceDriver implementation                                          */
/* ------------------------------------------------------------------ */

#[async_trait]
impl DeviceDriver for SinowealhDriver {
    fn name(&self) -> &str {
        "SinoWealth"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Read firmware version. */
        let fw_resp = sw_query_cmd(io, CMD_FIRMWARE_VERSION)?;
        let firmware_version = [fw_resp[2], fw_resp[3], fw_resp[4], fw_resp[5]];
        let fw_str = String::from_utf8_lossy(&firmware_version);
        debug!("SinoWealth: firmware version = {fw_str}");

        /* Detect if device uses CONFIG_LONG (0x06) or CONFIG (0x04).
         * Try reading config with CONFIG_LONG first; if it fails, use CONFIG. */
        let (is_long, report_id) = {
            let cmd = [REPORT_ID_CMD, CMD_GET_CONFIG[0], 0, 0, 0, 0];
            io.set_feature_report(&cmd).map_err(anyhow::Error::from)?;

            let mut test_buf = [0u8; CONFIG_REPORT_SIZE];
            test_buf[0] = REPORT_ID_CONFIG_LONG;
            match io.get_feature_report(&mut test_buf) {
                Ok(_) => (true, REPORT_ID_CONFIG_LONG),
                Err(_) => {
                    /* Retry with standard report ID. */
                    test_buf[0] = REPORT_ID_CONFIG;
                    io.get_feature_report(&mut test_buf)
                        .map_err(anyhow::Error::from)?;
                    (false, REPORT_ID_CONFIG)
                }
            }
        };
        debug!("SinoWealth: is_long = {is_long}, report_id = {report_id:#04x}");

        /* Get active profile. */
        let profile_resp = sw_query_cmd(io, CMD_PROFILE)?;
        let active_profile = profile_resp[2].saturating_sub(1); /* 1-indexed → 0-indexed */
        debug!("SinoWealth: active profile = {active_profile}");

        /* Read config and button reports for each profile. */
        let mut configs: [Box<[u8; CONFIG_REPORT_SIZE]>; NUM_PROFILES_MAX] = [
            Box::new([0u8; CONFIG_REPORT_SIZE]),
            Box::new([0u8; CONFIG_REPORT_SIZE]),
            Box::new([0u8; CONFIG_REPORT_SIZE]),
        ];
        let mut buttons: [Box<[u8; CONFIG_REPORT_SIZE]>; NUM_PROFILES_MAX] = [
            Box::new([0u8; CONFIG_REPORT_SIZE]),
            Box::new([0u8; CONFIG_REPORT_SIZE]),
            Box::new([0u8; CONFIG_REPORT_SIZE]),
        ];

        /* Determine profile count from DriverConfig (set via DeviceInfo later).
         * For now read all 3; load_profiles will use info.profiles.len(). */
        let mut profile_count = 1usize;
        let mut config_size = CONFIG_SIZE_MAX;
        let mut sensor = SENSOR_PMW3360;

        for i in 0..NUM_PROFILES_MAX {
            match sw_read_config(io, CMD_GET_CONFIG[i], report_id) {
                Ok(report) => {
                    if i == 0 {
                        sensor = report[OFF_SENSOR];
                        debug!("SinoWealth: sensor = {sensor:#04x}");
                    }
                    configs[i] = report;
                    profile_count = i + 1;
                }
                Err(e) => {
                    if i == 0 {
                        return Err(e);
                    }
                    debug!("SinoWealth: profile {i} config read failed (expected for single-profile devices): {e}");
                    break;
                }
            }

            match sw_read_config(io, CMD_GET_BUTTONS[i], report_id) {
                Ok(report) => {
                    buttons[i] = report;
                }
                Err(e) => {
                    if i == 0 {
                        return Err(e);
                    }
                    debug!("SinoWealth: profile {i} button read failed: {e}");
                    break;
                }
            }
        }

        /* Auto-detect config size: check if bytes beyond CONFIG_SIZE_MIN
         * contain meaningful data (non-zero). */
        let has_extended = configs[0][CONFIG_SIZE_MIN..CONFIG_SIZE_MAX]
            .iter()
            .any(|&b| b != 0);
        if !has_extended {
            config_size = CONFIG_SIZE_MIN;
        }
        debug!("SinoWealth: config_size = {config_size}");

        /* Try debounce query (may not work on all devices). */
        match sw_query_cmd(io, CMD_DEBOUNCE) {
            Ok(resp) => {
                let debounce_ms = u32::from(resp[2]) * 2;
                debug!("SinoWealth: debounce = {debounce_ms} ms");
            }
            Err(e) => {
                debug!("SinoWealth: debounce query not supported: {e}");
            }
        }

        self.data = Some(SinoweathData {
            firmware_version,
            sensor,
            active_profile,
            config_size,
            is_long,
            led_format: LedFormat::Rbg, /* default, updated in load_profiles */
            profile_count,
            configs,
            buttons,
        });

        Ok(())
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("SinoWealth: probe() was not called"))?;

        /* Set firmware version. */
        info.firmware_version = String::from_utf8_lossy(&data.firmware_version).to_string();

        /* Check DriverConfig for LED format (RGB vs RBG). */
        let led_format_str = info
            .driver_config
            .quirks
            .iter()
            .find(|q| q.eq_ignore_ascii_case("rgb") || q.eq_ignore_ascii_case("rbg"));
        if let Some(fmt) = led_format_str {
            data.led_format = if fmt.eq_ignore_ascii_case("rgb") {
                LedFormat::Rgb
            } else {
                LedFormat::Rbg
            };
        }

        /* Use the actual profile count from probe (capped by info.profiles.len()). */
        let profile_count = data.profile_count.min(info.profiles.len());

        /* Build DPI list from sensor. */
        let max_dpi = max_dpi_for_sensor(data.sensor);
        let dpi_list: Vec<u32> = (DPI_MIN..=max_dpi).step_by(DPI_STEP as usize).collect();

        for profile in &mut info.profiles {
            let idx = profile.index as usize;
            profile.report_rates = REPORT_RATES.to_vec();
            profile.debounces = DEBOUNCE_TIMES.to_vec();

            if idx >= profile_count {
                profile.is_enabled = false;
                continue;
            }

            let config = &data.configs[idx];
            let btn_report = &data.buttons[idx];

            profile.is_active = idx == data.active_profile as usize;

            /* Parse polling rate. */
            let rate_raw = config[OFF_RATE_FLAGS] & 0x0f;
            profile.report_rate = raw_to_rate(rate_raw);

            /* Parse DPI. */
            let _dpi_count = (config[OFF_DPI_COUNT] & 0x0f) as usize;
            let active_dpi = ((config[OFF_DPI_COUNT] >> 4) & 0x0f) as usize;
            let disabled_mask = config[OFF_DISABLED_DPI];
            let xy_independent = (config[OFF_RATE_FLAGS] >> 4) & 0x0f & (XY_INDEPENDENT >> 0) != 0;

            let mut enabled_count = 0u32;
            for (ri, res) in profile.resolutions.iter_mut().enumerate() {
                if ri >= NUM_DPIS {
                    break;
                }

                res.dpi_list = dpi_list.clone();
                res.is_disabled = (disabled_mask & (1 << ri)) != 0;

                if xy_independent {
                    let x = raw_to_dpi(data.sensor, config[OFF_DPIS + ri * 2]);
                    let y = raw_to_dpi(data.sensor, config[OFF_DPIS + ri * 2 + 1]);
                    res.dpi = Dpi::Separate { x, y };
                } else {
                    let d = raw_to_dpi(data.sensor, config[OFF_DPIS + ri]);
                    res.dpi = Dpi::Unified(d);
                }

                if !res.is_disabled {
                    enabled_count += 1;
                    res.is_active = enabled_count == active_dpi as u32;
                    res.is_default = res.is_active;
                }
            }

            /* Parse buttons. */
            for button in &mut profile.buttons {
                let bi = button.index as usize;
                if bi >= NUM_BUTTONS_HW {
                    continue;
                }
                let off = OFF_BTN_DATA + bi * 4;
                if off + 4 <= btn_report.len() {
                    let (action_type, mapping_value) = parse_button(&btn_report[off..off + 4]);
                    button.action_type = action_type;
                    button.mapping_value = mapping_value;
                }
            }

            /* Parse LED (if device has LEDs). */
            if !profile.leds.is_empty() {
                let led = &mut profile.leds[0];
                let effect = config[OFF_RGB_EFFECT];

                match effect {
                    RGB_OFF => {
                        led.mode = LedMode::Off;
                    }
                    RGB_SINGLE => {
                        led.mode = LedMode::Solid;
                        let c = raw_to_color(
                            data.led_format,
                            &config[OFF_SINGLE_COLOR..OFF_SINGLE_COLOR + 3],
                        );
                        led.color = Color::from_rgb(c);
                        led.brightness = rgb_mode_brightness(config[OFF_SINGLE_MODE]) as u32;
                    }
                    RGB_BREATHING7 => {
                        let color_count = config[OFF_BREATHING7_COLORCOUNT];
                        if color_count >= 1 {
                            led.mode = LedMode::Breathing;
                            let c = raw_to_color(
                                data.led_format,
                                &config[OFF_BREATHING7_COLORS..OFF_BREATHING7_COLORS + 3],
                            );
                            led.color = Color::from_rgb(c);
                            led.brightness = rgb_mode_brightness(config[OFF_BREATHING7_MODE]) as u32;
                            led.effect_duration = rgb_mode_duration(config[OFF_BREATHING7_MODE]);
                        } else {
                            led.mode = LedMode::Off;
                        }
                    }
                    RGB_BREATHING1 => {
                        if data.config_size > CONFIG_SIZE_MIN {
                            led.mode = LedMode::Breathing;
                            let c = raw_to_color(
                                data.led_format,
                                &config[126..129],
                            );
                            led.color = Color::from_rgb(c);
                            led.brightness = rgb_mode_brightness(config[125]) as u32;
                            led.effect_duration = rgb_mode_duration(config[125]);
                        }
                    }
                    RGB_NOT_SUPPORTED => { /* do nothing */ }
                    _ => {
                        /* Glorious, Breathing, Constant, Random, Tail, Rave, Wave
                         * all map to Cycle mode. */
                        led.mode = LedMode::Cycle;
                        led.brightness = rgb_mode_brightness(config[OFF_GLORIOUS_MODE]) as u32;
                        led.effect_duration = rgb_mode_duration(config[OFF_GLORIOUS_MODE]);
                    }
                }
            }
        }

        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("SinoWealth: probe() was not called"))?;

        let report_id = if data.is_long {
            REPORT_ID_CONFIG_LONG
        } else {
            REPORT_ID_CONFIG
        };

        let profile_count = data.profile_count.min(info.profiles.len());

        for profile in &info.profiles {
            let idx = profile.index as usize;
            if idx >= profile_count {
                continue;
            }

            /* Update config report. */
            {
                let config = &mut data.configs[idx];

                /* Update report rate. */
                let rate_raw = rate_to_raw(profile.report_rate);
                if rate_raw > 0 {
                    config[OFF_RATE_FLAGS] = (config[OFF_RATE_FLAGS] & 0xf0) | rate_raw;
                }

                /* Check if any resolution needs XY independent mode. */
                let needs_xy = profile.resolutions.iter().any(|r| {
                    matches!(r.dpi, Dpi::Separate { x, y } if x != y)
                });

                if needs_xy {
                    config[OFF_RATE_FLAGS] |= XY_INDEPENDENT << 0;
                    /* Shift config_flags to high nibble position. */
                    let flags = (config[OFF_RATE_FLAGS] >> 4) | (XY_INDEPENDENT >> 4);
                    config[OFF_RATE_FLAGS] = (config[OFF_RATE_FLAGS] & 0x0f) | (flags << 4);
                } else {
                    let flags = (config[OFF_RATE_FLAGS] >> 4) & !(XY_INDEPENDENT >> 4);
                    config[OFF_RATE_FLAGS] = (config[OFF_RATE_FLAGS] & 0x0f) | (flags << 4);
                }

                /* Update DPI levels. */
                let mut dpi_count: u8 = 0;
                let mut active_dpi: u8 = 0;
                let mut dpi_enabled: u8 = 0;

                for res in &profile.resolutions {
                    let ri = res.index as usize;
                    if ri >= NUM_DPIS {
                        break;
                    }

                    if res.is_disabled {
                        continue;
                    }

                    let (x, y) = match res.dpi {
                        Dpi::Unified(d) => (d, d),
                        Dpi::Separate { x, y } => (x, y),
                        Dpi::Unknown => continue,
                    };

                    if needs_xy {
                        config[OFF_DPIS + ri * 2] = dpi_to_raw(data.sensor, x);
                        config[OFF_DPIS + ri * 2 + 1] = dpi_to_raw(data.sensor, y);
                    } else {
                        config[OFF_DPIS + ri] = dpi_to_raw(data.sensor, x);
                    }

                    dpi_enabled |= 1 << ri;
                    dpi_count += 1;
                    if res.is_active {
                        active_dpi = dpi_count;
                    }
                }

                config[OFF_DPI_COUNT] = (dpi_count & 0x0f) | ((active_dpi & 0x0f) << 4);
                config[OFF_DISABLED_DPI] = !dpi_enabled;

                /* Update LED if present. */
                if !profile.leds.is_empty() {
                    let led = &profile.leds[0];
                    match led.mode {
                        LedMode::Off => {
                            config[OFF_RGB_EFFECT] = RGB_OFF;
                        }
                        LedMode::Solid => {
                            config[OFF_RGB_EFFECT] = RGB_SINGLE;
                            let raw_color = color_to_raw(data.led_format, led.color.to_rgb());
                            config[OFF_SINGLE_COLOR..OFF_SINGLE_COLOR + 3]
                                .copy_from_slice(&raw_color);
                            let b = brightness_to_rgb_mode(led.brightness as u32);
                            config[OFF_SINGLE_MODE] = (config[OFF_SINGLE_MODE] & 0x0f) | (b << 4);
                        }
                        LedMode::Breathing => {
                            config[OFF_RGB_EFFECT] = RGB_BREATHING7;
                            config[OFF_BREATHING7_COLORCOUNT] = 1;
                            let raw_color = color_to_raw(data.led_format, led.color.to_rgb());
                            config[OFF_BREATHING7_COLORS..OFF_BREATHING7_COLORS + 3]
                                .copy_from_slice(&raw_color);
                            let b = brightness_to_rgb_mode(led.brightness as u32);
                            let s = duration_to_rgb_mode(led.effect_duration);
                            config[OFF_BREATHING7_MODE] = s | (b << 4);
                        }
                        LedMode::Cycle => {
                            /* Keep existing effect if it was a cycle variant;
                             * default to Glorious mode. */
                            let current = config[OFF_RGB_EFFECT];
                            if current == RGB_OFF || current == RGB_SINGLE
                                || current == RGB_BREATHING7 || current == RGB_BREATHING1
                            {
                                config[OFF_RGB_EFFECT] = 0x01; /* Glorious */
                            }
                            let b = brightness_to_rgb_mode(led.brightness as u32);
                            let s = duration_to_rgb_mode(led.effect_duration);
                            config[OFF_GLORIOUS_MODE] = s | (b << 4);
                        }
                        _ => {}
                    }
                }

                /* Set config_write field and report ID for writing. */
                config[OFF_REPORT_ID] = report_id;
                config[OFF_COMMAND_ID] = CMD_GET_CONFIG[idx];
                config[OFF_CONFIG_WRITE] = (data.config_size as u8).wrapping_sub(8);

                sw_write_config(io, config)?;
            }

            /* Update button report. */
            {
                let btn_report = &mut data.buttons[idx];

                for button in &profile.buttons {
                    let bi = button.index as usize;
                    if bi >= NUM_BUTTONS_HW {
                        continue;
                    }
                    let off = OFF_BTN_DATA + bi * 4;
                    if off + 4 <= btn_report.len() {
                        encode_button(
                            button.action_type,
                            button.mapping_value,
                            &mut btn_report[off..off + 4],
                        );
                    }
                }

                btn_report[OFF_REPORT_ID] = report_id;
                btn_report[OFF_COMMAND_ID] = CMD_GET_BUTTONS[idx];
                btn_report[OFF_CONFIG_WRITE] = (BUTTON_REPORT_SIZE as u8).wrapping_sub(8);

                sw_write_config(io, btn_report)?;
            }
        }

        /* Set debounce if dirty. */
        if let Some(profile) = info.profiles.first() {
            if profile.debounce > 0 {
                let buf = [
                    REPORT_ID_CMD,
                    CMD_DEBOUNCE,
                    (profile.debounce as u8) / 2,
                    0,
                    0,
                    0,
                ];
                let _ = sw_write_cmd(io, &buf);
            }
        }

        /* Set active profile. */
        if let Some(active) = info.profiles.iter().find(|p| p.is_active) {
            let buf = [
                REPORT_ID_CMD,
                CMD_PROFILE,
                (active.index as u8) + 1,
                0,
                0,
                0,
            ];
            sw_write_cmd(io, &buf)?;
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
    fn dpi_roundtrip_pmw3360() {
        /* PMW3360: dpi = (raw + 1) * 100 */
        assert_eq!(raw_to_dpi(SENSOR_PMW3360, 0), 100);
        assert_eq!(raw_to_dpi(SENSOR_PMW3360, 119), 12000);
        assert_eq!(dpi_to_raw(SENSOR_PMW3360, 100), 0);
        assert_eq!(dpi_to_raw(SENSOR_PMW3360, 12000), 119);

        /* Roundtrip. */
        for dpi in (DPI_MIN..=12000).step_by(DPI_STEP as usize) {
            let raw = dpi_to_raw(SENSOR_PMW3360, dpi);
            assert_eq!(raw_to_dpi(SENSOR_PMW3360, raw), dpi, "failed for dpi={dpi}");
        }
    }

    #[test]
    fn dpi_roundtrip_pmw3389() {
        /* PMW3389: dpi = raw * 100 */
        assert_eq!(raw_to_dpi(SENSOR_PMW3389, 1), 100);
        assert_eq!(raw_to_dpi(SENSOR_PMW3389, 160), 16000);
        assert_eq!(dpi_to_raw(SENSOR_PMW3389, 100), 1);
        assert_eq!(dpi_to_raw(SENSOR_PMW3389, 16000), 160);

        for dpi in (DPI_MIN..=16000).step_by(DPI_STEP as usize) {
            let raw = dpi_to_raw(SENSOR_PMW3389, dpi);
            assert_eq!(raw_to_dpi(SENSOR_PMW3389, raw), dpi, "failed for dpi={dpi}");
        }
    }

    #[test]
    fn report_rate_roundtrip() {
        assert_eq!(raw_to_rate(1), 125);
        assert_eq!(raw_to_rate(2), 250);
        assert_eq!(raw_to_rate(3), 500);
        assert_eq!(raw_to_rate(4), 1000);

        assert_eq!(rate_to_raw(125), 1);
        assert_eq!(rate_to_raw(250), 2);
        assert_eq!(rate_to_raw(500), 3);
        assert_eq!(rate_to_raw(1000), 4);
    }

    #[test]
    fn color_rgb_rbg_swap() {
        let rgb_data = [0xFF, 0x00, 0x80];
        let rgb = raw_to_color(LedFormat::Rgb, &rgb_data);
        assert_eq!(rgb, RgbColor { r: 0xFF, g: 0x00, b: 0x80 });

        let rbg = raw_to_color(LedFormat::Rbg, &rgb_data);
        assert_eq!(rbg, RgbColor { r: 0xFF, g: 0x80, b: 0x00 });

        /* Roundtrip for RGB. */
        let encoded = color_to_raw(LedFormat::Rgb, rgb);
        assert_eq!(encoded, rgb_data);

        /* Roundtrip for RBG. */
        let encoded = color_to_raw(LedFormat::Rbg, rbg);
        assert_eq!(encoded, rgb_data);
    }

    #[test]
    fn button_action_mapping() {
        /* Mouse button 1. */
        let btn = [BTN_TYPE_BUTTON, 0x01, 0, 0];
        let (at, val) = parse_button(&btn);
        assert_eq!(at, ActionType::Button);
        assert_eq!(val, 1);

        /* Wheel up. */
        let btn = [BTN_TYPE_WHEEL, 0x01, 0, 0];
        let (at, val) = parse_button(&btn);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::WHEEL_UP);

        /* DPI cycle up. */
        let btn = [BTN_TYPE_SWITCH_DPI, 0x00, 0, 0];
        let (at, val) = parse_button(&btn);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::RESOLUTION_CYCLE_UP);

        /* Disabled. */
        let btn = [BTN_TYPE_SPECIAL, 0x01, 0, 0];
        let (at, _) = parse_button(&btn);
        assert_eq!(at, ActionType::None);
    }

    #[test]
    fn button_encode_roundtrip() {
        let mut buf = [0u8; 4];

        encode_button(ActionType::Button, 3, &mut buf);
        let (at, val) = parse_button(&buf);
        assert_eq!(at, ActionType::Button);
        assert_eq!(val, 3);

        encode_button(ActionType::Special, special_action::RESOLUTION_DOWN, &mut buf);
        let (at, val) = parse_button(&buf);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::RESOLUTION_DOWN);
    }

    #[test]
    fn disabled_dpi_bitmask() {
        /* Bit set = disabled, bit clear = enabled. */
        let mask: u8 = 0b11111100; /* only slots 0 and 1 enabled */
        assert!((mask & (1 << 0)) == 0); /* slot 0: enabled */
        assert!((mask & (1 << 1)) == 0); /* slot 1: enabled */
        assert!((mask & (1 << 2)) != 0); /* slot 2: disabled */
    }

    #[test]
    fn config_size_detection() {
        /* All zeros beyond MIN → short config. */
        let buf = [0u8; CONFIG_REPORT_SIZE];
        let has_ext = buf[CONFIG_SIZE_MIN..CONFIG_SIZE_MAX].iter().any(|&b| b != 0);
        assert!(!has_ext);

        /* Non-zero data beyond MIN → long config. */
        let mut buf2 = [0u8; CONFIG_REPORT_SIZE];
        buf2[CONFIG_SIZE_MIN + 5] = 0x42;
        let has_ext2 = buf2[CONFIG_SIZE_MIN..CONFIG_SIZE_MAX].iter().any(|&b| b != 0);
        assert!(has_ext2);
    }
}
