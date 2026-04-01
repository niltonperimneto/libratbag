use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, warn};

use crate::device::DeviceInfo;
use crate::driver::{DeviceDriver, DeviceIo};

/* ---------------------------------------------------------------------- */
/* Constants                                                              */
/* ---------------------------------------------------------------------- */
const STEELSERIES_NUM_PROFILES: u8 = 1;
const STEELSERIES_NUM_DPI: u8 = 2;

const STEELSERIES_REPORT_SIZE_SHORT: usize = 32;
const STEELSERIES_REPORT_SIZE: usize = 64;
const STEELSERIES_REPORT_LONG_SIZE: usize = 262;

/* Opcodes - V1 Short */
const STEELSERIES_ID_DPI_SHORT: u8 = 0x03;
const STEELSERIES_ID_REPORT_RATE_SHORT: u8 = 0x04;
const STEELSERIES_ID_LED_INTENSITY_SHORT: u8 = 0x05;
const STEELSERIES_ID_LED_EFFECT_SHORT: u8 = 0x07;
const STEELSERIES_ID_LED_COLOR_SHORT: u8 = 0x08;
const STEELSERIES_ID_LED_COLOR_SHORT_RIVAL100: u8 = 0x05;
const STEELSERIES_ID_SAVE_SHORT: u8 = 0x09;
const STEELSERIES_ID_FIRMWARE_PROTOCOL1: u8 = 0x10;

/* Opcodes - V2 */
const STEELSERIES_ID_BUTTONS: u8 = 0x31;
const STEELSERIES_ID_DPI: u8 = 0x53;
const STEELSERIES_ID_REPORT_RATE: u8 = 0x54;
const STEELSERIES_ID_LED: u8 = 0x5b;
const STEELSERIES_ID_SAVE: u8 = 0x59;
const STEELSERIES_ID_FIRMWARE_PROTOCOL2: u8 = 0x90;
const STEELSERIES_ID_SETTINGS: u8 = 0x92;

/* Opcodes - V3 */
const STEELSERIES_ID_DPI_PROTOCOL3: u8 = 0x03;
const STEELSERIES_ID_REPORT_RATE_PROTOCOL3: u8 = 0x04;
const STEELSERIES_ID_LED_PROTOCOL3: u8 = 0x05;
const STEELSERIES_ID_SAVE_PROTOCOL3: u8 = 0x09;
const STEELSERIES_ID_FIRMWARE_PROTOCOL3: u8 = 0x10;
const STEELSERIES_ID_SETTINGS_PROTOCOL3: u8 = 0x16;

/* Opcodes - V4 */
const STEELSERIES_ID_DPI_PROTOCOL4: u8 = 0x15;
const STEELSERIES_ID_REPORT_RATE_PROTOCOL4: u8 = 0x17;

/* Buttons */
const STEELSERIES_BUTTON_OFF: u8 = 0x00;
const STEELSERIES_BUTTON_RES_CYCLE: u8 = 0x30;
const STEELSERIES_BUTTON_WHEEL_UP: u8 = 0x31;
const STEELSERIES_BUTTON_WHEEL_DOWN: u8 = 0x32;
const STEELSERIES_BUTTON_KEY: u8 = 0x10;
const STEELSERIES_BUTTON_KBD: u8 = 0x51;
const STEELSERIES_BUTTON_CONSUMER: u8 = 0x61;

/* Button payload stride per button in the report (bytes) */
const STEELSERIES_BUTTON_SIZE_SENSEIRAW: usize = 3;
const STEELSERIES_BUTTON_SIZE_STANDARD: usize = 5;

/* DPI scaling: hardware stores (dpi / step) - 1; marker byte used by V2/V3 */
const STEELSERIES_DPI_MAGIC_MARKER: u8 = 0x42;

/* SteelSeries does not use numbered reports; all output reports carry
 * report_id = 0x00 as the first byte.  The actual command opcode lives at
 * byte offset 1 inside every output report buffer, mirroring the C union
 * steelseries_message layout where data[0] is the report_id and
 * parameters[0..] starts at data[1].  Feature reports, by contrast, use
 * the opcode itself as the HID feature report number in buf[0]. */
#[allow(dead_code)]
const STEELSERIES_REPORT_ID: u8 = 0x00;

/* ---------------------------------------------------------------------- */
/* Driver Instance                                                        */
/* ---------------------------------------------------------------------- */

pub struct SteelseriesDriver {
    version: u8,
}

impl SteelseriesDriver {
    pub fn new() -> Self {
        Self { version: 0 }
    }
}

/* ---------------------------------------------------------------------- */
/* Quirk helpers                                                          */
/* ---------------------------------------------------------------------- */

fn is_quirk(info: &DeviceInfo, name: &str) -> bool {
    info.driver_config.quirks.iter().any(|q| q == name)
}

fn is_senseiraw(info: &DeviceInfo) -> bool {
    is_quirk(info, "STEELSERIES_QUIRK_SENSEIRAW")
}

fn is_rival100(info: &DeviceInfo) -> bool {
    is_quirk(info, "STEELSERIES_QUIRK_RIVAL100")
}

/* Resolve the DPI step from the driver config.  Most SteelSeries devices
 * store the DPI index as (dpi / step - 1) where step comes from the
 * device database DpiRange.  Fallback to 100 if no range is configured. */
fn dpi_step(info: &DeviceInfo) -> u32 {
    info.driver_config
        .dpi_range
        .as_ref()
        .map(|r| r.step)
        .unwrap_or(100)
}

/* ---------------------------------------------------------------------- */
/* DeviceDriver trait implementation                                       */
/* ---------------------------------------------------------------------- */

#[async_trait]
impl DeviceDriver for SteelseriesDriver {
    fn name(&self) -> &str {
        "SteelSeries"
    }

    async fn probe(&mut self, _io: &mut DeviceIo) -> Result<()> {
        debug!("Probe called for SteelSeries");
        Ok(())
    }

    async fn load_profiles(&mut self, io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        if let Some(v) = info.driver_config.device_version {
            self.version = v as u8;
        } else {
            warn!("DeviceVersion not found in config, defaulting to 1");
            self.version = 1;
        }

        let button_count = info.driver_config.buttons.unwrap_or(0) as usize;
        let led_count = info.driver_config.leds.unwrap_or(0) as usize;
        let senseiraw = is_senseiraw(info);

        /* Build the DPI list from the range specification if available. */
        let dpi_list: Vec<u32> = info
            .driver_config
            .dpi_range
            .as_ref()
            .map(|r| (r.min..=r.max).step_by(r.step as usize).collect())
            .unwrap_or_default();

        let report_rates = vec![125, 250, 500, 1000];

        info.profiles.clear();
        for profile_id in 0..STEELSERIES_NUM_PROFILES {
            let mut profile = crate::device::ProfileInfo {
                index: profile_id as u32,
                name: format!("Profile {}", profile_id),
                is_active: true,
                is_enabled: true,
                is_dirty: false,
                report_rate: 1000,
                report_rates: report_rates.clone(),
                angle_snapping: -1,
                debounce: -1,
                debounces: vec![],
                capabilities: vec![],
                resolutions: vec![],
                buttons: vec![],
                leds: vec![],
            };

            for res_id in 0..STEELSERIES_NUM_DPI {
                profile.resolutions.push(crate::device::ResolutionInfo {
                    index: res_id as u32,
                    is_active: res_id == 0,
                    is_default: res_id == 0,
                    dpi: crate::device::Dpi::Unified(800 * (res_id as u32 + 1)),
                    dpi_list: dpi_list.clone(),
                    capabilities: vec![],
                    is_disabled: false,
                });
            }

            /* Build button defaults following the C driver's
             * button_defaults_for_layout logic: for devices with <= 6
             * buttons, button 5 (index 5) is mapped to resolution cycle
             * up; for 7 buttons, button 6 gets the cycle; for 8+, button
             * 7 gets it. */
            for btn_id in 0..button_count as u32 {
                let mut action_types = vec![
                    crate::device::ActionType::None as u32,
                    crate::device::ActionType::Button as u32,
                    crate::device::ActionType::Special as u32,
                ];
                if !senseiraw {
                    action_types.push(crate::device::ActionType::Macro as u32);
                }

                let (action_type, mapping_value) =
                    button_defaults_for_layout(btn_id, button_count as u32);

                profile.buttons.push(crate::device::ButtonInfo {
                    index: btn_id,
                    action_type,
                    action_types,
                    mapping_value,
                    macro_entries: vec![],
                });
            }

            for led_id in 0..led_count as u32 {
                /* V1 devices support Off, Solid, Breathing; V2+ add Cycle. */
                let mut modes = vec![
                    crate::device::LedMode::Off,
                    crate::device::LedMode::Solid,
                    crate::device::LedMode::Breathing,
                ];
                if self.version >= 2 {
                    modes.push(crate::device::LedMode::Cycle);
                }

                let (color_depth, color, brightness) = if senseiraw {
                    /* Monochrome – brightness controls intensity */
                    (1u32, crate::device::Color::default(), 255u32)
                } else {
                    /* RGB_888 – default to blue as in the C driver */
                    (
                        3u32,
                        crate::device::Color {
                            red: 0,
                            green: 0,
                            blue: 255,
                        },
                        255u32,
                    )
                };

                profile.leds.push(crate::device::LedInfo {
                    index: led_id,
                    mode: crate::device::LedMode::Solid,
                    modes,
                    color,
                    secondary_color: crate::device::Color::default(),
                    tertiary_color: crate::device::Color::default(),
                    color_depth,
                    effect_duration: 1000,
                    brightness,
                });
            }

            /* Attempt to override defaults by reading active hardware settings. */
            if let Err(e) = self.read_settings(io, &mut profile).await {
                warn!("SteelSeries: failed to read hardware settings: {e}");
            }

            info.profiles.push(profile);
        }

        if let Ok(fw) = self.read_firmware_version(io).await {
            info.firmware_version = fw;
        }

        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let profile = info
            .profiles
            .iter()
            .find(|p| p.is_active)
            .or_else(|| info.profiles.first())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No profiles found in DeviceInfo (SteelSeries hardware requires at least 1)"
                )
            })?;

        /* Write DPI */
        for res in &profile.resolutions {
            if res.is_active {
                self.write_dpi(io, res, info).await?;
                break;
            }
        }

        /* Write Buttons */
        self.write_buttons(io, profile, info).await?;

        /* Write LEDs */
        for led in &profile.leds {
            self.write_led(io, led, info).await?;
        }

        self.write_report_rate(io, profile.report_rate).await?;

        /* Write Save (EEPROM target) */
        self.write_save(io).await?;

        Ok(())
    }
}

/* ---------------------------------------------------------------------- */
/* Button default layout                                                  */
/* ---------------------------------------------------------------------- */

/* Map a button index to its default action, mirroring the C driver's
 * button_defaults_for_layout() function.  Returns (ActionType, mapping_value). */
fn button_defaults_for_layout(btn_id: u32, button_count: u32) -> (crate::device::ActionType, u32) {
    /* Index of the button that should get the resolution-cycle-up special */
    let special_idx = if button_count <= 6 {
        5
    } else if button_count == 7 {
        6
    } else {
        7
    };

    if btn_id == special_idx {
        /* Special: resolution cycle up.  mapping_value encodes
         * RATBAG_BUTTON_ACTION_SPECIAL_RESOLUTION_CYCLE_UP. */
        (
            crate::device::ActionType::Special,
            crate::device::special_action::RESOLUTION_CYCLE_UP,
        )
    } else if btn_id < 8 {
        /* Regular mouse button (1-indexed for DBus compatibility). */
        (crate::device::ActionType::Button, btn_id + 1)
    } else {
        (crate::device::ActionType::None, 0)
    }
}

/* ---------------------------------------------------------------------- */
/* Helper methods – all payloads built as explicit byte arrays            */
/*                                                                        */
/* Output reports: buf[0] = 0x00 (report_id), opcode at buf[1], data at   */
/* buf[2..].  Feature reports: buf[0] = opcode (HID report number), data  */
/* at buf[1..].  All indices in the C driver's parameters[] array are     */
/* therefore offset by +1 in the Rust output-report buffers.              */
/* ---------------------------------------------------------------------- */

impl SteelseriesDriver {
    /* ------------------------------------------------------------------ */
    /* write_dpi                                                          */
    /* ------------------------------------------------------------------ */

    async fn write_dpi(
        &self,
        io: &mut DeviceIo,
        res: &crate::device::ResolutionInfo,
        info: &DeviceInfo,
    ) -> Result<()> {
        let dpi_val = match res.dpi {
            crate::device::Dpi::Unified(d) => d,
            crate::device::Dpi::Separate { x, .. } => x,
            crate::device::Dpi::Unknown => 800,
        };
        let step = dpi_step(info);
        let res_id = res.index as u8 + 1;

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        match self.version {
            1 => {
                /* V1 with DPI list: reverse-lookup the index (entries are
                 * enumerated in reverse order in the C driver).  With DPI
                 * range: compute (dpi / step - 1). */
                let scaled: u8 = if !res.dpi_list.is_empty() {
                    let pos = res.dpi_list.iter().position(|&d| d == dpi_val).unwrap_or(0);
                    (res.dpi_list.len() - pos) as u8
                } else {
                    (dpi_val / step).saturating_sub(1) as u8
                };

                let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
                /* buf[0] = 0x00 (report_id, already zero) */
                buf[1] = STEELSERIES_ID_DPI_SHORT;
                buf[2] = res_id;
                buf[3] = scaled;
                io.write_report(&buf).await
            }
            2 => {
                let scaled = (dpi_val / step).saturating_sub(1) as u8;
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_DPI;
                buf[3] = res_id;
                buf[4] = scaled;
                buf[7] = STEELSERIES_DPI_MAGIC_MARKER;
                io.write_report(&buf).await
            }
            3 => {
                let scaled = (dpi_val / step).saturating_sub(1) as u8;
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_DPI_PROTOCOL3;
                buf[3] = res_id;
                buf[4] = scaled;
                buf[6] = STEELSERIES_DPI_MAGIC_MARKER;
                io.write_report(&buf).await
            }
            4 => {
                /* V4 uses STEELSERIES_REPORT_SIZE (64 bytes), not SHORT. */
                let scaled = (dpi_val / step).saturating_sub(1) as u8;
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_DPI_PROTOCOL4;
                buf[2] = res_id;
                buf[3] = scaled;
                io.write_report(&buf).await
            }
            _ => Ok(()),
        }
    }

    /* ------------------------------------------------------------------ */
    /* write_buttons                                                      */
    /* ------------------------------------------------------------------ */

    async fn write_buttons(
        &self,
        io: &mut DeviceIo,
        profile: &crate::device::ProfileInfo,
        info: &DeviceInfo,
    ) -> Result<()> {
        /* If the device reports zero macro length, button writes are
         * not supported – bail out early as the C driver does. */
        if info.driver_config.macro_length == Some(0) {
            return Ok(());
        }

        let senseiraw = is_senseiraw(info);
        let button_size = if senseiraw {
            STEELSERIES_BUTTON_SIZE_SENSEIRAW
        } else {
            STEELSERIES_BUTTON_SIZE_STANDARD
        };
        let report_size = if senseiraw {
            STEELSERIES_REPORT_SIZE_SHORT
        } else {
            STEELSERIES_REPORT_LONG_SIZE
        };
        let max_modifiers: usize = if senseiraw { 0 } else { 3 };

        let mut buf = [0u8; STEELSERIES_REPORT_LONG_SIZE];
        /* buf[0] = 0x00 (report_id) */
        buf[1] = STEELSERIES_ID_BUTTONS;

        for button in &profile.buttons {
            /* Each button takes button_size bytes starting at offset 3
             * (parameters index 2 offset by +1 for report_id). */
            let idx = 3 + (button.index as usize) * button_size;
            if idx >= report_size {
                continue;
            }

            match button.action_type {
                crate::device::ActionType::Button => {
                    buf[idx] = button.mapping_value as u8;
                }
                crate::device::ActionType::Key | crate::device::ActionType::Macro => {
                    /* Extract modifiers and the final keycode from macro
                     * entries if simulating a key sequence. */
                    let mut modifiers = 0u8;
                    let mut final_key = 0u8;

                    for &(ev_type, k) in &button.macro_entries {
                        if ev_type == 0 {
                            /* Key press event */
                            match k {
                                224 => modifiers |= 0x01, /* LCTRL */
                                225 => modifiers |= 0x02, /* LSHIFT */
                                226 => modifiers |= 0x04, /* LALT */
                                227 => modifiers |= 0x08, /* LMETA */
                                228 => modifiers |= 0x10, /* RCTRL */
                                229 => modifiers |= 0x20, /* RSHIFT */
                                230 => modifiers |= 0x40, /* RALT */
                                231 => modifiers |= 0x80, /* RMETA */
                                _ => final_key = (k % 256) as u8,
                            }
                        }
                    }

                    /* If no macro entries, fall back to mapping_value. */
                    if button.macro_entries.is_empty() {
                        final_key = (button.mapping_value % 256) as u8;
                    }

                    /* Enforce the maximum modifier count for this layout. */
                    if modifiers.count_ones() as usize > max_modifiers {
                        warn!(
                            "SteelSeries: button {} has too many modifiers ({}, max {})",
                            button.index,
                            modifiers.count_ones(),
                            max_modifiers
                        );
                    }

                    if final_key != 0 {
                        /* Keyboard usage */
                        if senseiraw {
                            buf[idx] = STEELSERIES_BUTTON_KEY;
                            if idx + 1 < report_size {
                                buf[idx + 1] = final_key;
                            }
                        } else {
                            buf[idx] = STEELSERIES_BUTTON_KBD;
                            let mut cursor = idx;

                            static MODIFIER_TABLE: [(u8, u8); 8] = [
                                (0x01, 0xE0),
                                (0x02, 0xE1),
                                (0x04, 0xE2),
                                (0x08, 0xE3),
                                (0x10, 0xE4),
                                (0x20, 0xE5),
                                (0x40, 0xE6),
                                (0x80, 0xE7),
                            ];
                            for &(mask, code) in &MODIFIER_TABLE {
                                if (modifiers & mask) != 0 && cursor - idx < max_modifiers {
                                    if cursor + 1 < report_size {
                                        buf[cursor + 1] = code;
                                    }
                                    cursor += 1;
                                }
                            }

                            if cursor + 1 < report_size {
                                buf[cursor + 1] = final_key;
                            }
                        }
                    } else {
                        /* No keyboard code – assume consumer usage, matching
                         * the C driver's STEELSERIES_BUTTON_CONSUMER path. */
                        buf[idx] = STEELSERIES_BUTTON_CONSUMER;
                        if idx + 1 < report_size {
                            buf[idx + 1] = (button.mapping_value % 256) as u8;
                        }
                    }
                }
                crate::device::ActionType::Special => {
                    use crate::device::special_action;
                    match button.mapping_value {
                        special_action::RESOLUTION_CYCLE_UP => {
                            buf[idx] = STEELSERIES_BUTTON_RES_CYCLE
                        }
                        special_action::WHEEL_UP => buf[idx] = STEELSERIES_BUTTON_WHEEL_UP,
                        special_action::WHEEL_DOWN => buf[idx] = STEELSERIES_BUTTON_WHEEL_DOWN,
                        _ => buf[idx] = STEELSERIES_BUTTON_OFF,
                    }
                }
                _ => buf[idx] = STEELSERIES_BUTTON_OFF,
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        if self.version == 3 {
            /* V3 uses a HID feature report.  Reframe: buf[1..] contains
             * the parameters with buf[1] = opcode (= feature report
             * number).  We slice buf[1..report_size] to form the
             * feature-report payload expected by set_feature_report. */
            io.set_feature_report(&buf[1..report_size])?;
            Ok(())
        } else {
            io.write_report(&buf[..report_size]).await
        }
    }

    /* ------------------------------------------------------------------ */
    /* write_report_rate                                                   */
    /* ------------------------------------------------------------------ */

    async fn write_report_rate(&self, io: &mut DeviceIo, hz: u32) -> Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        match self.version {
            1 | 4 => {
                /* V1 and V4 use discretized rate codes:
                 * 1000 Hz → 0x01, 500 Hz → 0x02, 250 Hz → 0x03, 125 Hz → 0x04. */
                let rate_code: u8 = if hz >= 1000 {
                    0x01
                } else if hz >= 375 {
                    0x02
                } else if hz <= 125 {
                    0x04
                } else {
                    0x03
                };

                let opcode = if self.version == 1 {
                    STEELSERIES_ID_REPORT_RATE_SHORT
                } else {
                    STEELSERIES_ID_REPORT_RATE_PROTOCOL4
                };

                let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
                buf[1] = opcode;
                buf[3] = rate_code;
                io.write_report(&buf).await
            }
            2 => {
                let rate_val = (1000 / std::cmp::max(hz, 125)) as u8;
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_REPORT_RATE;
                buf[3] = rate_val;
                io.write_report(&buf).await
            }
            3 => {
                let rate_val = (1000 / std::cmp::max(hz, 125)) as u8;
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_REPORT_RATE_PROTOCOL3;
                buf[3] = rate_val;
                io.write_report(&buf).await
            }
            _ => Ok(()),
        }
    }

    /* ------------------------------------------------------------------ */
    /* write_led (dispatcher)                                              */
    /* ------------------------------------------------------------------ */

    async fn write_led(
        &self,
        io: &mut DeviceIo,
        led: &crate::device::LedInfo,
        info: &DeviceInfo,
    ) -> Result<()> {
        match self.version {
            1 => self.write_led_v1(io, led, info).await,
            2 => self.write_led_v2(io, led).await,
            3 => self.write_led_v3(io, led).await,
            _ => Ok(()),
        }
    }

    /* ------------------------------------------------------------------ */
    /* write_led_v1 – handles Rival100 and SenseiRaw quirks               */
    /* ------------------------------------------------------------------ */

    async fn write_led_v1(
        &self,
        io: &mut DeviceIo,
        led: &crate::device::LedInfo,
        info: &DeviceInfo,
    ) -> Result<()> {
        let rival100 = is_rival100(info);
        let senseiraw = is_senseiraw(info);

        let effect = match led.mode {
            crate::device::LedMode::Off | crate::device::LedMode::Solid => 0x01,
            crate::device::LedMode::Breathing => {
                let ms = led.effect_duration;
                if ms <= 3000 {
                    0x04
                } else if ms <= 5000 {
                    0x03
                } else {
                    0x02
                }
            }
            _ => {
                /* Cycle and other modes are not supported on V1 hardware. */
                return Err(anyhow::anyhow!(
                    "SteelSeries V1: unsupported LED mode {:?}",
                    led.mode
                ));
            }
        };

        /* Effect report */
        let mut effect_buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        effect_buf[1] = STEELSERIES_ID_LED_EFFECT_SHORT;
        effect_buf[2] = if rival100 { 0x00 } else { led.index as u8 + 1 };
        effect_buf[3] = effect;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&effect_buf).await?;

        /* Second report: color or intensity depending on quirk. */
        let mut color_buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];

        if senseiraw {
            /* SenseiRaw uses LED intensity (monochrome) instead of RGB. */
            color_buf[1] = STEELSERIES_ID_LED_INTENSITY_SHORT;
            color_buf[2] = led.index as u8 + 1;
            if led.mode == crate::device::LedMode::Off || led.brightness == 0 {
                color_buf[3] = 1;
            } else {
                /* Split brightness into roughly 3 equal intensities:
                 * 0-85 → 2, 86-171 → 3, 172-255 → 4 */
                color_buf[3] = (led.brightness as u8 / 86) + 2;
            }
        } else if rival100 {
            /* Rival100 uses a different color opcode and led_id = 0x00. */
            color_buf[1] = STEELSERIES_ID_LED_COLOR_SHORT_RIVAL100;
            color_buf[2] = 0x00;
            color_buf[3] = led.color.red as u8;
            color_buf[4] = led.color.green as u8;
            color_buf[5] = led.color.blue as u8;
        } else {
            color_buf[1] = STEELSERIES_ID_LED_COLOR_SHORT;
            color_buf[2] = led.index as u8 + 1;
            color_buf[3] = led.color.red as u8;
            color_buf[4] = led.color.green as u8;
            color_buf[5] = led.color.blue as u8;
        }

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&color_buf).await
    }

    /* ------------------------------------------------------------------ */
    /* write_led_v2 – cycle-buffer matching C construct_cycle_buffer       */
    /* ------------------------------------------------------------------ */

    async fn write_led_v2(&self, io: &mut DeviceIo, led: &crate::device::LedInfo) -> Result<()> {
        /* V2 cycle spec (matches C steelseries_led_cycle_spec for V2):
         *   cmd_val  (parameters[0])      → buf index 1
         *   led_id   (parameters[2])      → buf index 3
         *   duration (parameters[3..5])   → buf index 4..6  (u16 LE)
         *   repeat   (parameters[19])     → buf index 20
         *   trigger  (parameters[23])     → buf index 24
         *   npoints  (parameters[27])     → buf index 28
         *   header_len = 28 → first color data at parameters[28] → buf index 29
         *   has_2_led_ids = false */
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[1] = STEELSERIES_ID_LED;
        buf[3] = led.index as u8;

        let (repeat, points, duration) = build_cycle_points(led);

        if !repeat {
            buf[20] = 0x01;
        }
        /* buf[24] = trigger_buttons (always 0x00) */

        let header_start = 29usize; /* parameters[28] → buf[29] */
        let npoints = write_cycle_points(&mut buf, header_start, &points);

        buf[28] = npoints;
        let d = std::cmp::max(npoints as u16 * 330, duration);
        buf[4..6].copy_from_slice(&d.to_le_bytes());

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&buf).await
    }

    /* ------------------------------------------------------------------ */
    /* write_led_v3 – cycle-buffer matching C construct_cycle_buffer       */
    /* ------------------------------------------------------------------ */

    async fn write_led_v3(&self, io: &mut DeviceIo, led: &crate::device::LedInfo) -> Result<()> {
        /* V3 cycle spec (matches C steelseries_led_cycle_spec for V3):
         *   cmd_val  (parameters[0])      → buf index 0  (feature report number)
         *   led_id   (parameters[2])      → buf index 2
         *   led_id2  (parameters[7])      → buf index 7
         *   duration (parameters[8..10])  → buf index 8..10  (u16 LE)
         *   repeat   (parameters[24])     → buf index 24
         *   trigger  (parameters[25])     → buf index 25
         *   npoints  (parameters[29])     → buf index 29
         *   header_len = 30 → first color data at parameters[30] → buf index 30
         *   has_2_led_ids = true
         *
         * V3 uses a HID feature report; we build the buffer with the
         * opcode as buf[0] (the feature report number). */
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = STEELSERIES_ID_LED_PROTOCOL3;
        buf[2] = led.index as u8;
        buf[7] = led.index as u8;

        let (repeat, points, duration) = build_cycle_points(led);

        if !repeat {
            buf[24] = 0x01;
        }
        /* buf[25] = trigger_buttons (always 0x00) */

        let header_start = 30usize;
        let npoints = write_cycle_points(&mut buf, header_start, &points);

        buf[29] = npoints;
        let d = std::cmp::max(npoints as u16 * 330, duration);
        buf[8..10].copy_from_slice(&d.to_le_bytes());

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.set_feature_report(&buf)?;
        Ok(())
    }

    /* ------------------------------------------------------------------ */
    /* write_save                                                         */
    /* ------------------------------------------------------------------ */

    async fn write_save(&self, io: &mut DeviceIo) -> Result<()> {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        match self.version {
            1 => {
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
                buf[1] = STEELSERIES_ID_SAVE_SHORT;
                io.write_report(&buf).await
            }
            2 => {
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_SAVE;
                io.write_report(&buf).await
            }
            3 | 4 => {
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_SAVE_PROTOCOL3;
                io.write_report(&buf).await
            }
            _ => Ok(()),
        }
    }

    /* ------------------------------------------------------------------ */
    /* read_firmware_version                                               */
    /* ------------------------------------------------------------------ */

    async fn read_firmware_version(&self, io: &mut DeviceIo) -> Result<String> {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        match self.version {
            1 => {
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
                buf[1] = STEELSERIES_ID_FIRMWARE_PROTOCOL1;
                io.write_report(&buf).await?;
            }
            2 => {
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_FIRMWARE_PROTOCOL2;
                io.write_report(&buf).await?;
            }
            3 => {
                let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
                buf[1] = STEELSERIES_ID_FIRMWARE_PROTOCOL3;
                io.write_report(&buf).await?;
            }
            _ => return Ok(String::new()),
        }

        /* Timeout to gracefully skip if the device doesn't respond
         * (some variants are write-only for certain reports). */
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        if let Ok(Ok(n)) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            io.read_report(&mut buf),
        )
        .await
        {
            if n >= 2 {
                let major = buf.get(1).copied().unwrap_or(0);
                let minor = buf.get(0).copied().unwrap_or(0);
                return Ok(format!("{}.{}", major, minor));
            }
        }

        Ok(String::new())
    }

    /* ------------------------------------------------------------------ */
    /* read_settings                                                       */
    /* ------------------------------------------------------------------ */

    async fn read_settings(
        &self,
        io: &mut DeviceIo,
        profile: &mut crate::device::ProfileInfo,
    ) -> Result<()> {
        let settings_id = match self.version {
            2 => STEELSERIES_ID_SETTINGS,
            3 => STEELSERIES_ID_SETTINGS_PROTOCOL3,
            _ => return Ok(()),
        };

        let mut req = [0u8; STEELSERIES_REPORT_SIZE];
        req[1] = settings_id;
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&req).await?;

        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        if let Ok(Ok(n)) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            io.read_report(&mut buf),
        )
        .await
        {
            if n < 2 {
                return Ok(());
            }

            if self.version == 2 {
                let active_resolution = buf.get(1).copied().unwrap_or(0).saturating_sub(1);
                for res in &mut profile.resolutions {
                    res.is_active = res.index == active_resolution as u32;
                    let dpi_idx = 2 + res.index as usize * 2;
                    if dpi_idx < n {
                        let dpi_val = 100 * (1 + buf.get(dpi_idx).copied().unwrap_or(0) as u32);
                        res.dpi = crate::device::Dpi::Unified(dpi_val);
                    }
                }

                for led in &mut profile.leds {
                    let offset = 6 + led.index as usize * 3;
                    if offset + 2 < n {
                        led.color.red = buf.get(offset).copied().unwrap_or(0) as u32;
                        led.color.green = buf.get(offset + 1).copied().unwrap_or(0) as u32;
                        led.color.blue = buf.get(offset + 2).copied().unwrap_or(0) as u32;
                    }
                }
            } else if self.version == 3 {
                let active_resolution = buf.get(0).copied().unwrap_or(0).saturating_sub(1);
                for res in &mut profile.resolutions {
                    res.is_active = res.index == active_resolution as u32;
                }
            }
        }

        Ok(())
    }
}

/* ---------------------------------------------------------------------- */
/* Cycle-point construction (shared between V2 and V3)                    */
/* ---------------------------------------------------------------------- */

/* A single color-position point in a LED cycle animation. */
struct CyclePoint {
    r: u8,
    g: u8,
    b: u8,
    pos: u8,
}

/* Build the list of cycle control points for a given LED mode.
 * Returns (repeat, points, duration_ms). */
fn build_cycle_points(led: &crate::device::LedInfo) -> (bool, Vec<CyclePoint>, u16) {
    match led.mode {
        crate::device::LedMode::Off => {
            let points = vec![CyclePoint {
                r: 0,
                g: 0,
                b: 0,
                pos: 0x00,
            }];
            (false, points, 5000)
        }
        crate::device::LedMode::Solid => {
            let points = vec![CyclePoint {
                r: led.color.red as u8,
                g: led.color.green as u8,
                b: led.color.blue as u8,
                pos: 0x00,
            }];
            (false, points, 5000)
        }
        crate::device::LedMode::Cycle => {
            /* 4-point rainbow: red → green → blue → red, matching the C
             * driver's hard-coded RATBAG_LED_CYCLE control points. */
            let points = vec![
                CyclePoint {
                    r: 0xFF,
                    g: 0x00,
                    b: 0x00,
                    pos: 0x00,
                },
                CyclePoint {
                    r: 0x00,
                    g: 0xFF,
                    b: 0x00,
                    pos: 0x55,
                },
                CyclePoint {
                    r: 0x00,
                    g: 0x00,
                    b: 0xFF,
                    pos: 0x55,
                },
                CyclePoint {
                    r: 0xFF,
                    g: 0x00,
                    b: 0x00,
                    pos: 0x55,
                },
            ];
            (true, points, led.effect_duration as u16)
        }
        crate::device::LedMode::Breathing => {
            /* 3-point breathe: black → color → black, matching the C
             * driver's RATBAG_LED_BREATHING control points. */
            let points = vec![
                CyclePoint {
                    r: 0,
                    g: 0,
                    b: 0,
                    pos: 0x00,
                },
                CyclePoint {
                    r: led.color.red as u8,
                    g: led.color.green as u8,
                    b: led.color.blue as u8,
                    pos: 0x7F,
                },
                CyclePoint {
                    r: 0,
                    g: 0,
                    b: 0,
                    pos: 0x7F,
                },
            ];
            (true, points, led.effect_duration as u16)
        }
        _ => {
            /* Unknown mode – treat as a static black point. */
            let points = vec![CyclePoint {
                r: 0,
                g: 0,
                b: 0,
                pos: 0x00,
            }];
            (false, points, 5000)
        }
    }
}

/* Write cycle points into a buffer following the C construct_cycle_buffer()
 * layout: the first point's color is duplicated as a 3-byte RGB header
 * immediately before the regular 4-byte (r,g,b,pos) point array.
 * Returns the number of points written. */
fn write_cycle_points(buf: &mut [u8], header_start: usize, points: &[CyclePoint]) -> u8 {
    let mut color_idx = header_start;

    for (i, pt) in points.iter().enumerate() {
        if i == 0 {
            /* Write the first point's color as a 3-byte header. */
            buf[color_idx] = pt.r;
            buf[color_idx + 1] = pt.g;
            buf[color_idx + 2] = pt.b;
            color_idx += 3;
        }

        let base = color_idx + i * 4;
        if base + 3 < buf.len() {
            buf[base] = pt.r;
            buf[base + 1] = pt.g;
            buf[base + 2] = pt.b;
            buf[base + 3] = pt.pos;
        }
    }

    points.len() as u8
}
