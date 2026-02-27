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
const STEELSERIES_ID_LED_EFFECT_SHORT: u8 = 0x07;
const STEELSERIES_ID_LED_COLOR_SHORT: u8 = 0x08;
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

/* ---------------------------------------------------------------------- */
/* Driver Instance                                                        */
/* ---------------------------------------------------------------------- */

/* ---------------------------------------------------------------------- */
/* Payload Structures                                                     */
/* ---------------------------------------------------------------------- */

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesDpiReportV1 {
    pub report_id: u8,
    pub res_id: u8,
    pub dpi_scaled: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
}

impl SteelseriesDpiReportV1 {
    pub fn new(res_id: u8, dpi_scaled: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_DPI_SHORT,
            res_id,
            dpi_scaled,
            padding: [0u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
        }
    }

    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        buf[0] = self.report_id;
        buf[1] = self.res_id;
        buf[2] = self.dpi_scaled;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesDpiReportV2 {
    pub report_id: u8,
    pub padding1: u8,
    pub res_id: u8,
    pub dpi_scaled: u8,
    pub padding2: [u8; 2],
    pub magic_42: u8,
    pub padding3: [u8; STEELSERIES_REPORT_SIZE - 7],
}

impl SteelseriesDpiReportV2 {
    pub fn new(res_id: u8, dpi_scaled: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_DPI,
            padding1: 0,
            res_id,
            dpi_scaled,
            padding2: [0u8; 2],
            magic_42: 0x42,
            padding3: [0u8; STEELSERIES_REPORT_SIZE - 7],
        }
    }

    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf[2] = self.res_id;
        buf[3] = self.dpi_scaled;
        buf[6] = self.magic_42;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesDpiReportV3 {
    pub report_id: u8,
    pub padding1: u8,
    pub res_id: u8,
    pub dpi_scaled: u8,
    pub padding2: u8,
    pub magic_42: u8,
    pub padding3: [u8; STEELSERIES_REPORT_SIZE - 6],
}

impl SteelseriesDpiReportV3 {
    pub fn new(res_id: u8, dpi_scaled: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_DPI_PROTOCOL3,
            padding1: 0,
            res_id,
            dpi_scaled,
            padding2: 0,
            magic_42: 0x42,
            padding3: [0u8; STEELSERIES_REPORT_SIZE - 6],
        }
    }

    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf[2] = self.res_id;
        buf[3] = self.dpi_scaled;
        buf[5] = self.magic_42;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesDpiReportV4 {
    pub report_id: u8,
    pub res_id: u8,
    pub dpi_scaled: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
}

impl SteelseriesDpiReportV4 {
    pub fn new(res_id: u8, dpi_scaled: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_DPI_PROTOCOL4,
            res_id,
            dpi_scaled,
            padding: [0u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
        }
    }

    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        buf[0] = self.report_id;
        buf[1] = self.res_id;
        buf[2] = self.dpi_scaled;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesReportRateV1 {
    pub report_id: u8,
    pub padding1: u8,
    pub rate_val: u8,
    pub padding2: [u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
}

impl SteelseriesReportRateV1 {
    pub fn new(rate_val: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_REPORT_RATE_SHORT,
            padding1: 0,
            rate_val,
            padding2: [0u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        buf[0] = self.report_id;
        buf[2] = self.rate_val;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesReportRateV2 {
    pub report_id: u8,
    pub padding1: u8,
    pub rate_val: u8,
    pub padding2: [u8; STEELSERIES_REPORT_SIZE - 3],
}

impl SteelseriesReportRateV2 {
    pub fn new(rate_val: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_REPORT_RATE,
            padding1: 0,
            rate_val,
            padding2: [0u8; STEELSERIES_REPORT_SIZE - 3],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf[2] = self.rate_val;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesReportRateV3 {
    pub report_id: u8,
    pub padding1: u8,
    pub rate_val: u8,
    pub padding2: [u8; STEELSERIES_REPORT_SIZE - 3],
}

impl SteelseriesReportRateV3 {
    pub fn new(rate_val: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_REPORT_RATE_PROTOCOL3,
            padding1: 0,
            rate_val,
            padding2: [0u8; STEELSERIES_REPORT_SIZE - 3],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf[2] = self.rate_val;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesReportRateV4 {
    pub report_id: u8,
    pub padding1: u8,
    pub rate_val: u8,
    pub padding2: [u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
}

impl SteelseriesReportRateV4 {
    pub fn new(rate_val: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_REPORT_RATE_PROTOCOL4,
            padding1: 0,
            rate_val,
            padding2: [0u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        buf[0] = self.report_id;
        buf[2] = self.rate_val;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesSaveV1 {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE_SHORT - 1],
}

impl SteelseriesSaveV1 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_SAVE_SHORT,
            padding: [0u8; STEELSERIES_REPORT_SIZE_SHORT - 1],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        buf[0] = self.report_id;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesSaveV2 {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE - 1],
}

impl SteelseriesSaveV2 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_SAVE,
            padding: [0; 63],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesSaveV3 {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE - 1],
}

impl SteelseriesSaveV3 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_SAVE_PROTOCOL3,
            padding: [0; 63],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesFirmwareRequestV1 {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE_SHORT - 1],
}

impl SteelseriesFirmwareRequestV1 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_FIRMWARE_PROTOCOL1,
            padding: [0; 31],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE_SHORT];
        buf[0] = self.report_id;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesFirmwareRequestV2 {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE - 1],
}

impl SteelseriesFirmwareRequestV2 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_FIRMWARE_PROTOCOL2,
            padding: [0; 63],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesFirmwareRequestV3 {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE - 1],
}

impl SteelseriesFirmwareRequestV3 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_FIRMWARE_PROTOCOL3,
            padding: [0; 63],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesLedEffectReportV1 {
    pub report_id: u8,
    pub led_id: u8,
    pub effect: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE_SHORT - 3],
}
impl SteelseriesLedEffectReportV1 {
    pub fn new(led_id: u8, effect: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_LED_EFFECT_SHORT,
            led_id,
            effect,
            padding: [0; 29],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut b = [0; 32];
        b[0] = self.report_id;
        b[1] = self.led_id;
        b[2] = self.effect;
        b
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesLedColorReportV1 {
    pub report_id: u8,
    pub led_id: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE_SHORT - 5],
}
impl SteelseriesLedColorReportV1 {
    pub fn new(led_id: u8, r: u8, g: u8, b: u8) -> Self {
        Self {
            report_id: STEELSERIES_ID_LED_COLOR_SHORT,
            led_id,
            r,
            g,
            b,
            padding: [0; 27],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE_SHORT] {
        let mut b = [0; 32];
        b[0] = self.report_id;
        b[1] = self.led_id;
        b[2] = self.r;
        b[3] = self.g;
        b[4] = self.b;
        b
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct SteelseriesLedPoint {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub pos: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesLedReportV2 {
    pub report_id: u8,
    pub padding1: u8,
    pub led_id: u8,
    pub duration: [u8; 2],
    pub padding2: [u8; 14],
    pub disable_repeat: u8,
    pub padding3: [u8; 7],
    pub npoints: u8,
    pub points: [SteelseriesLedPoint; 9],
}
impl SteelseriesLedReportV2 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_LED,
            padding1: 0,
            led_id: 0,
            duration: [0; 2],
            padding2: [0; 14],
            disable_repeat: 0,
            padding3: [0; 7],
            npoints: 0,
            points: [SteelseriesLedPoint::default(); 9],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf[1] = self.padding1;
        buf[2] = self.led_id;
        buf[3..5].copy_from_slice(&self.duration);
        buf[5..19].copy_from_slice(&self.padding2);
        buf[19] = self.disable_repeat;
        buf[20..27].copy_from_slice(&self.padding3);
        buf[27] = self.npoints;

        let mut offset = 28;
        for p in &self.points {
            buf[offset] = p.r;
            buf[offset + 1] = p.g;
            buf[offset + 2] = p.b;
            buf[offset + 3] = p.pos;
            offset += 4;
        }
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesLedReportV3 {
    pub report_id: u8,
    pub padding1: u8,
    pub led_id: u8,
    pub padding2: [u8; 4],
    pub led_id2: u8,
    pub duration: [u8; 2],
    pub padding3: [u8; 14],
    pub disable_repeat: u8,
    pub padding4: [u8; 4],
    pub npoints: u8,
    pub points: [SteelseriesLedPoint; 8],
    pub padding5: [u8; 2],
}
impl SteelseriesLedReportV3 {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_LED_PROTOCOL3,
            padding1: 0,
            led_id: 0,
            padding2: [0; 4],
            led_id2: 0,
            duration: [0; 2],
            padding3: [0; 14],
            disable_repeat: 0,
            padding4: [0; 4],
            npoints: 0,
            points: [SteelseriesLedPoint::default(); 8],
            padding5: [0; 2],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        buf[0] = self.report_id;
        buf[1] = self.padding1;
        buf[2] = self.led_id;
        buf[3..7].copy_from_slice(&self.padding2);
        buf[7] = self.led_id2;
        buf[8..10].copy_from_slice(&self.duration);
        buf[10..24].copy_from_slice(&self.padding3);
        buf[24] = self.disable_repeat;
        buf[25..29].copy_from_slice(&self.padding4);
        buf[29] = self.npoints;

        let mut offset = 30;
        for p in &self.points {
            buf[offset] = p.r;
            buf[offset + 1] = p.g;
            buf[offset + 2] = p.b;
            buf[offset + 3] = p.pos;
            offset += 4;
        }

        buf[62..64].copy_from_slice(&self.padding5);
        buf
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesButtonReport {
    pub report_id: u8,
    pub padding: u8,
    pub buttons: [u8; 260],
}
impl SteelseriesButtonReport {
    pub fn new() -> Self {
        Self {
            report_id: STEELSERIES_ID_BUTTONS,
            padding: 0,
            buttons: [0; 260],
        }
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_LONG_SIZE] {
        let mut b = [0; 262];
        b[0] = self.report_id;
        b[1] = self.padding;
        b[2..262].copy_from_slice(&self.buttons);
        b
    }
    pub fn write_idx(&mut self, idx: usize, val: u8) {
        if idx >= 2 && idx < 262 {
            self.buttons[idx - 2] = val;
        }
    }
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct SteelseriesSettingsRequest {
    pub report_id: u8,
    pub padding: [u8; STEELSERIES_REPORT_SIZE - 1],
}
impl SteelseriesSettingsRequest {
    pub fn new(version: u8) -> Option<Self> {
        let id = match version {
            2 => STEELSERIES_ID_SETTINGS,
            3 => STEELSERIES_ID_SETTINGS_PROTOCOL3,
            _ => return None,
        };
        Some(Self {
            report_id: id,
            padding: [0; 63],
        })
    }
    pub fn into_bytes(self) -> [u8; STEELSERIES_REPORT_SIZE] {
        let mut buf = [0; 64];
        buf[0] = self.report_id;
        buf
    }
}

pub struct SteelseriesDriver {
    version: u8,
}

impl SteelseriesDriver {
    pub fn new() -> Self {
        Self { version: 0 }
    }
}

#[async_trait]
impl DeviceDriver for SteelseriesDriver {
    fn name(&self) -> &str {
        "SteelSeries"
    }

    async fn probe(&mut self, _io: &mut DeviceIo) -> Result<()> {
        debug!("Probe called for SteelSeries dummy");
        /* We will extract version from DeviceInfo during load_profiles since probe doesn't give us */
        /* the DeviceDb mappings yet, or we'll assume it defaults to 1 until load_profiles provides it. */
        Ok(())
    }

    async fn load_profiles(&mut self, io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        if let Some(v) = info.driver_config.device_version {
            self.version = v as u8;
        } else {
            warn!("DeviceVersion not found in config, defaulting to 1");
            self.version = 1;
        }

        /* SteelSeries devices don't usually report their settings (they rely on software DBs). */
        /* Therefore `load_profiles` merely sets the basic skeleton structure natively. */
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
                angle_snapping: 0,
                debounce: 0,
                debounces: vec![],
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
                    dpi_list: vec![],
                    capabilities: vec![],
                    is_disabled: false,
                });
            }

            for btn_id in 0..6 {
                profile.buttons.push(crate::device::ButtonInfo {
                    index: btn_id,
                    action_type: crate::device::ActionType::Button,
                    action_types: vec![],
                    mapping_value: btn_id as u32 + 1,
                    macro_entries: vec![],
                });
            }

            for led_id in 0..2 {
                profile.leds.push(crate::device::LedInfo {
                    index: led_id,
                    mode: crate::device::LedMode::Solid,
                    modes: vec![],
                    color: crate::device::Color {
                        red: 255,
                        green: 0,
                        blue: 0,
                    },
                    secondary_color: crate::device::Color {
                        red: 0,
                        green: 0,
                        blue: 0,
                    },
                    tertiary_color: crate::device::Color {
                        red: 0,
                        green: 0,
                        blue: 0,
                    },
                    color_depth: 3,
                    effect_duration: 1000,
                    brightness: 255,
                });
            }

            /* Attempt to override defaults by reading active hardware settings */
            let _ = self.read_settings(io, &mut profile).await;

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
                self.write_dpi(io, res).await?;
                break;
            }
        }

        /* Write Buttons */
        self.write_buttons(io, profile, info).await?;

        /* Write LEDs */
        for led in &profile.leds {
            self.write_led(io, led).await?;
        }

        self.write_report_rate(io, profile.report_rate).await?;

        /* Write Save (EEPROM target) */
        self.write_save(io).await?;

        Ok(())
    }
}

impl SteelseriesDriver {
    async fn write_dpi(
        &self,
        io: &mut DeviceIo,
        res: &crate::device::ResolutionInfo,
    ) -> Result<()> {
        let dpi_val = match res.dpi {
            crate::device::Dpi::Unified(d) => d,
            crate::device::Dpi::Separate { x, .. } => x,
            crate::device::Dpi::Unknown => 800,
        };
        let scaled = (dpi_val / 100).saturating_sub(1) as u8;

        match self.version {
            1 => {
                let payload = SteelseriesDpiReportV1::new(res.index as u8 + 1, scaled);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&payload.into_bytes()).await
            }
            2 => {
                let payload = SteelseriesDpiReportV2::new(res.index as u8 + 1, scaled);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&payload.into_bytes()).await
            }
            3 => {
                let payload = SteelseriesDpiReportV3::new(res.index as u8 + 1, scaled);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&payload.into_bytes()).await
            }
            4 => {
                let payload = SteelseriesDpiReportV4::new(res.index as u8 + 1, scaled);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&payload.into_bytes()).await
            }
            _ => Ok(()),
        }
    }

    async fn write_buttons(
        &self,
        io: &mut DeviceIo,
        profile: &crate::device::ProfileInfo,
        info: &DeviceInfo,
    ) -> Result<()> {
        let mut report = SteelseriesButtonReport::new();

        let is_senseiraw = info
            .driver_config
            .quirks
            .iter()
            .any(|q| q == "STEELSERIES_QUIRK_SENSEIRAW");

        let button_size = if is_senseiraw { 3 } else { 5 };
        let report_size = if is_senseiraw {
            STEELSERIES_REPORT_SIZE_SHORT
        } else {
            STEELSERIES_REPORT_LONG_SIZE
        };

        for button in &profile.buttons {
            let idx = 2 + (button.index as usize) * button_size;
            if idx >= report_size {
                continue;
            } /* Bounds guard */

            match button.action_type {
                crate::device::ActionType::Button => {
                    report.write_idx(idx, button.mapping_value as u8);
                }
                crate::device::ActionType::Key => {
                    let key = button.mapping_value;
                    let hid_usage = (key % 256) as u8;

                    if is_senseiraw {
                        report.write_idx(idx, STEELSERIES_BUTTON_KEY);
                        report.write_idx(idx + 1, hid_usage);
                    } else {
                        report.write_idx(idx, STEELSERIES_BUTTON_KBD);
                        report.write_idx(idx + 1, hid_usage);
                    }
                }
                crate::device::ActionType::Macro => {
                    /* Extract modifiers and the final keycode from macro entries if simulating a key sequence */
                    let mut modifiers = 0u8;
                    let mut final_key = 0u8;

                    for &(ev_type, k) in &button.macro_entries {
                        if ev_type == 0 {
                            /* Press */
                            match k {
                                224 => {
                                    modifiers |= 0x01;
                                } /* LCTRL */
                                225 => {
                                    modifiers |= 0x02;
                                } /* LSHIFT */
                                226 => {
                                    modifiers |= 0x04;
                                } /* LALT */
                                227 => {
                                    modifiers |= 0x08;
                                } /* LMETA */
                                228 => {
                                    modifiers |= 0x10;
                                } /* RCTRL */
                                229 => {
                                    modifiers |= 0x20;
                                } /* RSHIFT */
                                230 => {
                                    modifiers |= 0x40;
                                } /* RALT */
                                231 => {
                                    modifiers |= 0x80;
                                } /* RMETA */
                                _ => final_key = (k % 256) as u8,
                            }
                        }
                    }

                    if is_senseiraw {
                        report.write_idx(idx, STEELSERIES_BUTTON_KEY);
                        report.write_idx(idx + 1, final_key);
                    } else {
                        report.write_idx(idx, STEELSERIES_BUTTON_KBD);
                        let mut cursor = idx;

                        /* Maximum of 3 modifiers allowed by SteelSeries protocol natively */
                        if (modifiers & 0x01) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE0);
                            cursor += 1;
                        }
                        if (modifiers & 0x02) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE1);
                            cursor += 1;
                        }
                        if (modifiers & 0x04) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE2);
                            cursor += 1;
                        }
                        if (modifiers & 0x08) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE3);
                            cursor += 1;
                        }
                        if (modifiers & 0x10) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE4);
                            cursor += 1;
                        }
                        if (modifiers & 0x20) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE5);
                            cursor += 1;
                        }
                        if (modifiers & 0x40) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE6);
                            cursor += 1;
                        }
                        if (modifiers & 0x80) != 0 && cursor - idx < 3 {
                            report.write_idx(cursor + 1, 0xE7);
                            cursor += 1;
                        }

                        report.write_idx(cursor + 1, final_key);
                    }
                }
                crate::device::ActionType::Special => {
                    /* Simple map for mapping_value -> RES_CYCLE etc... */
                    match button.mapping_value {
                        1 => report.write_idx(idx, STEELSERIES_BUTTON_RES_CYCLE),
                        2 => report.write_idx(idx, STEELSERIES_BUTTON_WHEEL_UP),
                        3 => report.write_idx(idx, STEELSERIES_BUTTON_WHEEL_DOWN),
                        _ => report.write_idx(idx, STEELSERIES_BUTTON_OFF),
                    }
                }
                _ => report.write_idx(idx, STEELSERIES_BUTTON_OFF),
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let b = report.into_bytes();
        if self.version == 3 {
            io.set_feature_report(&b[..report_size])?;
            Ok(())
        } else {
            io.write_report(&b[..report_size]).await
        }
    }

    async fn write_report_rate(&self, io: &mut DeviceIo, hz: u32) -> Result<()> {
        let rate_val = (1000 / std::cmp::max(hz, 125)) as u8;

        match self.version {
            1 => {
                let report = SteelseriesReportRateV1::new(rate_val);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&report.into_bytes()).await
            }
            2 => {
                let report = SteelseriesReportRateV2::new(rate_val);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&report.into_bytes()).await
            }
            3 => {
                let report = SteelseriesReportRateV3::new(rate_val);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&report.into_bytes()).await
            }
            4 => {
                let report = SteelseriesReportRateV4::new(rate_val);
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&report.into_bytes()).await
            }
            _ => Ok(()),
        }
    }

    async fn write_led(&self, io: &mut DeviceIo, led: &crate::device::LedInfo) -> Result<()> {
        match self.version {
            1 => self.write_led_v1(io, led).await,
            2 => self.write_led_v2(io, led).await,
            3 => self.write_led_v3(io, led).await,
            _ => Ok(()), /* Protocol 4 etc untested for LED parity here */
        }
    }

    async fn write_led_v1(&self, io: &mut DeviceIo, led: &crate::device::LedInfo) -> Result<()> {
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
            _ => return Ok(()),
        };

        let effect_report = SteelseriesLedEffectReportV1::new(led.index as u8 + 1, effect);
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&effect_report.into_bytes()).await?;

        let color_report = SteelseriesLedColorReportV1::new(
            led.index as u8 + 1,
            led.color.red as u8,
            led.color.green as u8,
            led.color.blue as u8,
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&color_report.into_bytes()).await
    }

    async fn write_led_v2(&self, io: &mut DeviceIo, led: &crate::device::LedInfo) -> Result<()> {
        let mut report = SteelseriesLedReportV2::new();
        report.led_id = led.index as u8;

        if matches!(
            led.mode,
            crate::device::LedMode::Off | crate::device::LedMode::Solid
        ) {
            report.disable_repeat = 0x01;
        }

        let mut npoints = 0;
        let c1 = &led.color;
        let off = led.mode == crate::device::LedMode::Off;

        report.points[npoints].r = if off { 0 } else { c1.red as u8 };
        report.points[npoints].g = if off { 0 } else { c1.green as u8 };
        report.points[npoints].b = if off { 0 } else { c1.blue as u8 };
        report.points[npoints].pos = 0x00;
        npoints += 1;

        if led.mode == crate::device::LedMode::Breathing {
            report.points[npoints].r = c1.red as u8;
            report.points[npoints].g = c1.green as u8;
            report.points[npoints].b = c1.blue as u8;
            report.points[npoints].pos = 0x7F;
            npoints += 1;

            report.points[npoints].pos = 0x7F; // Black out
            npoints += 1;
        }

        report.npoints = npoints as u8;
        let d = std::cmp::max(npoints as u16 * 330, led.effect_duration as u16);
        report.duration = d.to_le_bytes();

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.write_report(&report.into_bytes()).await
    }

    async fn write_led_v3(&self, io: &mut DeviceIo, led: &crate::device::LedInfo) -> Result<()> {
        let mut report = SteelseriesLedReportV3::new();
        report.led_id = led.index as u8;
        report.led_id2 = led.index as u8;

        if matches!(
            led.mode,
            crate::device::LedMode::Off | crate::device::LedMode::Solid
        ) {
            report.disable_repeat = 0x01;
        }

        let mut npoints = 0;
        let c1 = &led.color;
        let off = led.mode == crate::device::LedMode::Off;

        report.points[npoints].r = if off { 0 } else { c1.red as u8 };
        report.points[npoints].g = if off { 0 } else { c1.green as u8 };
        report.points[npoints].b = if off { 0 } else { c1.blue as u8 };
        report.points[npoints].pos = 0x00;
        npoints += 1;

        if led.mode == crate::device::LedMode::Breathing {
            report.points[npoints].r = c1.red as u8;
            report.points[npoints].g = c1.green as u8;
            report.points[npoints].b = c1.blue as u8;
            report.points[npoints].pos = 0x7F;
            npoints += 1;

            report.points[npoints].pos = 0x7F; // Black out
            npoints += 1;
        }

        report.npoints = npoints as u8;
        let d = std::cmp::max(npoints as u16 * 330, led.effect_duration as u16);
        report.duration = d.to_le_bytes();

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        io.set_feature_report(&report.into_bytes())?;
        Ok(())
    }

    async fn write_save(&self, io: &mut DeviceIo) -> Result<()> {
        match self.version {
            1 => {
                let report = SteelseriesSaveV1::new();
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                io.write_report(&report.into_bytes()).await
            }
            2 => {
                let report = SteelseriesSaveV2::new();
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                io.write_report(&report.into_bytes()).await
            }
            3 | 4 => {
                let report = SteelseriesSaveV3::new();
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                io.write_report(&report.into_bytes()).await
            }
            _ => Ok(()),
        }
    }

    async fn read_firmware_version(&self, io: &mut DeviceIo) -> Result<String> {
        match self.version {
            1 => {
                let req = SteelseriesFirmwareRequestV1::new();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&req.into_bytes()).await?;
            }
            2 => {
                let req = SteelseriesFirmwareRequestV2::new();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&req.into_bytes()).await?;
            }
            3 => {
                let req = SteelseriesFirmwareRequestV3::new();
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                io.write_report(&req.into_bytes()).await?;
            }
            _ => return Ok(String::new()),
        }

        /* Timeout to gracefully skip if the device doesn't respond (some variants are Write-Only) */
        let mut buf = [0u8; STEELSERIES_REPORT_SIZE];
        if let Ok(Ok(n)) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            io.read_report(&mut buf),
        )
        .await
        {
            if n >= 2 {
                /* Return formats as 'major.minor' - bound checking buffer size explicitly */
                let major = buf.get(1).copied().unwrap_or(0);
                let minor = buf.get(0).copied().unwrap_or(0);
                return Ok(format!("{}.{}", major, minor));
            }
        }

        Ok(String::new())
    }

    async fn read_settings(
        &self,
        io: &mut DeviceIo,
        profile: &mut crate::device::ProfileInfo,
    ) -> Result<()> {
        if let Some(req) = SteelseriesSettingsRequest::new(self.version) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            io.write_report(&req.into_bytes()).await?;

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
        }

        Ok(())
    }
}
