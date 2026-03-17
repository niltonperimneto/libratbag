/* Logitech HID++ 2.0 driver implementation. */
/*  */
/* HID++ 2.0 is the modern feature-based protocol used by most current */
/* Logitech gaming mice. Each capability is exposed as a numbered "feature" */
/* that must be discovered at probe time via the Root feature (0x0000). */

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::time::{sleep, Duration};
use tracing::{debug, info, trace, warn};

use crate::device::{Color, DeviceInfo, Dpi, LedMode, ProfileInfo, RgbColor};
use crate::driver::DeviceIo;

use super::hidpp::{
    self, Hidpp20MatchResult, HidppReport, DEVICE_IDX_CORDED, DEVICE_IDX_RECEIVER,
    BUTTON_SUBTYPE_CONSUMER, BUTTON_SUBTYPE_KEYBOARD, BUTTON_SUBTYPE_MOUSE,
    BUTTON_TYPE_DISABLED, BUTTON_TYPE_HID, BUTTON_TYPE_MACRO, BUTTON_TYPE_SPECIAL,
    LED_HW_MODE_BREATHING, LED_HW_MODE_COLOR_WAVE,
    LED_HW_MODE_CYCLE, LED_HW_MODE_FIXED, LED_HW_MODE_OFF, LED_HW_MODE_STARLIGHT,
    PAGE_ADJUSTABLE_DPI, PAGE_ADJUSTABLE_REPORT_RATE,
    PAGE_COLOR_LED_EFFECTS, PAGE_ONBOARD_PROFILES, PAGE_RGB_EFFECTS,
    ROOT_FEATURE_INDEX, ROOT_FN_GET_FEATURE,
    ROOT_FN_GET_PROTOCOL_VERSION,
};

use crate::device::ActionType;

/* Software ID used in all our requests (arbitrary, identifies us) */
const SW_ID: u8 = 0x04;

/* Adjustable DPI (0x2201) function IDs */
const DPI_FN_GET_SENSOR_COUNT: u8 = 0x00;
const DPI_FN_GET_SENSOR_DPI_LIST: u8 = 0x01;
const DPI_FN_GET_SENSOR_DPI: u8 = 0x02;
const DPI_FN_SET_SENSOR_DPI: u8 = 0x03;

/* Adjustable Report Rate (0x8060) function IDs */
const RATE_FN_GET_REPORT_RATE_LIST: u8 = 0x00;
const RATE_FN_GET_REPORT_RATE: u8 = 0x01;

/* Color LED Effects (0x8070) function IDs.
 * C defines: GET_INFO=0x00, GET_ZONE_INFO=0x10, GET_ZONE_EFFECT_INFO=0x20,
 *            SET_ZONE_EFFECT=0x30, GET_ZONE_EFFECT=0xE0.
 * The address byte encodes (function << 4 | sw_id), so we store the function
 * number in the upper nibble position: 0x30 → fn 3, 0xE0 → fn 14. */
const LED_FN_GET_ZONE_EFFECT: u8 = 0x0E;
const LED_FN_SET_ZONE_EFFECT: u8 = 0x03;

/* Onboard Profiles (0x8100) function IDs.
 * C defines: GET_PROFILES_DESCR=0x00, SET_ONBOARD_MODE=0x10,
 * GET_ONBOARD_MODE=0x20, SET_CURRENT_PROFILE=0x30,
 * GET_CURRENT_PROFILE=0x40, MEMORY_READ=0x50,
 * MEMORY_ADDR_WRITE=0x60, MEMORY_WRITE=0x70,
 * MEMORY_WRITE_END=0x80. */
const PROFILES_FN_GET_PROFILES_DESCR: u8 = 0x00;
const PROFILES_FN_SET_MODE: u8 = 0x01;
const PROFILES_FN_GET_MODE: u8 = 0x02;
const PROFILES_FN_SET_CURRENT_PROFILE: u8 = 0x03;
const PROFILES_FN_GET_CURRENT_PROFILE: u8 = 0x04;
const PROFILES_FN_MEMORY_READ: u8 = 0x05;
const PROFILES_FN_MEMORY_ADDR_WRITE: u8 = 0x06;
const PROFILES_FN_MEMORY_WRITE: u8 = 0x07;
const PROFILES_FN_MEMORY_WRITE_END: u8 = 0x08;
const PROFILES_FN_GET_CURRENT_DPI_INDEX: u8 = 0x0B;
const PROFILES_FN_SET_CURRENT_DPI_INDEX: u8 = 0x0C;

/* Onboard profile sector addresses — must match the C constants
 * HIDPP20_USER_PROFILES_G402 and HIDPP20_ROM_PROFILES_G402. */
const USER_PROFILES_BASE: u16 = 0x0000;
const ROM_PROFILES_BASE: u16 = 0x0100;

/* Onboard profile mode values for PROFILES_FN_SET_MODE / GET_MODE.
 * Mode 1 = onboard (mouse runs stored profiles autonomously).
 * Mode 2 = host (software controls mouse via live feature requests).
 * C constant: HIDPP20_ONBOARD_MODE = 1. */
const ONBOARD_MODE_ONBOARD: u8 = 0x01;
const ONBOARD_MODE_HOST: u8 = 0x02;

/* EEPROM profile sector layout offsets.
 * C struct: hidpp20_profile / hidpp20_internal_led layout. */
const EEPROM_DPI_OFFSET: usize = 3;
const EEPROM_DPI_ENTRY_SIZE: usize = 2;
const EEPROM_DPI_COUNT: usize = 5;
const EEPROM_BUTTON_OFFSET: usize = 32;
const EEPROM_BUTTON_SIZE: usize = 4;
const EEPROM_LED_OFFSET: usize = 208;
const EEPROM_LED_SIZE: usize = 11;
const EEPROM_LED_COUNT: usize = 2;

/* Parse a getSensorDPIList response buffer (bytes after the sensor-index
 * byte) into an expanded list of supported DPI values.  The buffer
 * contains big-endian u16 entries terminated by a 0x0000 sentinel.
 *
 * A value >= 0xE000 is a range-step marker: step = value & 0x1FFF.
 * The preceding discrete entry is the range minimum and the next
 * entry is the range maximum.  Otherwise the entry is a single
 * discrete DPI value.  This mirrors the C hidpp20 DPI list parser. */
fn parse_dpi_list_entries(list_bytes: &[u8]) -> Result<Vec<u32>> {
    if list_bytes.len() % 2 != 0 {
        warn!(
            "HID++ 2.0: DPI list has odd byte count ({}), trailing byte ignored",
            list_bytes.len()
        );
    }

    let mut entries: Vec<u16> = Vec::new();
    for chunk in list_bytes.chunks_exact(2) {
        let val = u16::from_be_bytes([chunk[0], chunk[1]]);
        if val == 0 {
            break;
        }
        entries.push(val);
    }

    let mut dpi_list: Vec<u32> = Vec::new();
    let mut i = 0;
    while i < entries.len() {
        let val = entries[i];
        if val >= 0xE000 {
            let step = u32::from(val & 0x1FFF);
            let dpi_min = dpi_list.pop().ok_or_else(|| {
                anyhow::anyhow!(
                    "Malformed DPI list: range-step marker 0x{val:04X} \
                     at index {i} has no preceding discrete entry"
                )
            })?;
            let dpi_max = if i + 1 < entries.len() {
                u32::from(entries[i + 1])
            } else {
                return Err(anyhow::anyhow!(
                    "Malformed DPI list: range-step marker 0x{val:04X} \
                     at index {i} has no following range-max entry"
                ));
            };
            if step > 0 && dpi_max >= dpi_min {
                let mut v = dpi_min;
                while v <= dpi_max {
                    dpi_list.push(v);
                    v = v.saturating_add(step);
                }
            }
            i += 2;
        } else {
            dpi_list.push(u32::from(val));
            i += 1;
        }
    }

    Ok(dpi_list)
}

/* A feature page → runtime index mapping for a known set of capabilities. */
#[derive(Debug, Default)]
struct FeatureMap {
    adjustable_dpi: Option<u8>,
    onboard_profiles: Option<u8>,
    color_led_effects: Option<u8>,
    rgb_effects: Option<u8>,
    report_rate: Option<u8>,
}

impl FeatureMap {
    /* Store a discovered feature index based on its page ID. */
    fn insert(&mut self, page: u16, index: u8) {
        match page {
            PAGE_ADJUSTABLE_DPI => self.adjustable_dpi = Some(index),
            PAGE_ONBOARD_PROFILES => self.onboard_profiles = Some(index),
            PAGE_COLOR_LED_EFFECTS => self.color_led_effects = Some(index),
            PAGE_RGB_EFFECTS => self.rgb_effects = Some(index),
            PAGE_ADJUSTABLE_REPORT_RATE => self.report_rate = Some(index),
            _ => {}
        }
    }
}

/* Feature 0x2201 (Adjustable DPI): Payload for Get/Set Sensor DPI */
#[derive(Debug, Clone, Copy)]
struct Hidpp20DpiPayload {
    sensor_index: u8,
    current_dpi: [u8; 2], /* Big Endian u16 */
    default_dpi: [u8; 2], /* Big Endian u16 */
    padding: [u8; 11],
}

impl Hidpp20DpiPayload {
    fn from_bytes(buf: &[u8; 16]) -> Self {
        let sensor_index = buf[0];
        let mut current_dpi = [0u8; 2];
        current_dpi.copy_from_slice(&buf[1..3]);
        let mut default_dpi = [0u8; 2];
        default_dpi.copy_from_slice(&buf[3..5]);
        let mut padding = [0u8; 11];
        padding.copy_from_slice(&buf[5..16]);
        Self { sensor_index, current_dpi, default_dpi, padding }
    }
    fn into_bytes(self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0] = self.sensor_index;
        buf[1..3].copy_from_slice(&self.current_dpi);
        buf[3..5].copy_from_slice(&self.default_dpi);
        buf[5..16].copy_from_slice(&self.padding);
        buf
    }
    fn current_dpi(&self) -> u16 {
        u16::from_be_bytes(self.current_dpi)
    }
    fn default_dpi(&self) -> u16 {
        u16::from_be_bytes(self.default_dpi)
    }
    fn set_current_dpi(&mut self, dpi: u16) {
        self.current_dpi = dpi.to_be_bytes();
    }
}

/* Feature 0x8070 & 0x8071 (Color LED / RGB) */
#[derive(Debug, Clone, Copy, Default)]
struct Hidpp20LedGetZonePayload {
    zone_index: u8,
    payload: [u8; crate::driver::hidpp::LED_PAYLOAD_SIZE], /* 11 bytes */
}

impl Hidpp20LedGetZonePayload {
    fn from_bytes(buf: &[u8; 16]) -> Self {
        let zone_index = buf[0];
        let mut payload = [0u8; crate::driver::hidpp::LED_PAYLOAD_SIZE];
        payload.copy_from_slice(&buf[1..1+crate::driver::hidpp::LED_PAYLOAD_SIZE]);
        Self { zone_index, payload }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Hidpp20LedSetZonePayload {
    zone_index: u8,
    payload: [u8; crate::driver::hidpp::LED_PAYLOAD_SIZE],
    persist: u8,
    padding: [u8; 3],
}

impl Hidpp20LedSetZonePayload {
    fn into_bytes(self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0] = self.zone_index;
        let p_end = 1 + crate::driver::hidpp::LED_PAYLOAD_SIZE;
        buf[1..p_end].copy_from_slice(&self.payload);
        buf[p_end] = self.persist;
        buf[p_end+1..16].copy_from_slice(&self.padding);
        buf
    }
}

/* HID++ 2.0 Button Binding representation (4 bytes) */
#[derive(Debug, Clone, Copy, Default)]
struct Hidpp20ButtonBinding {
    button_type: u8,
    subtype: u8,
    control_id_or_macro_id: [u8; 2], /* little endian */
}

impl Hidpp20ButtonBinding {
    fn from_bytes(buf: &[u8; 4]) -> Self {
        let button_type = buf[0];
        let subtype = buf[1];
        let mut control_id_or_macro_id = [0u8; 2];
        control_id_or_macro_id.copy_from_slice(&buf[2..4]);
        Self { button_type, subtype, control_id_or_macro_id }
    }
    
    fn into_bytes(self) -> [u8; 4] {
        let mut buf = [0u8; 4];
        buf[0] = self.button_type;
        buf[1] = self.subtype;
        buf[2..4].copy_from_slice(&self.control_id_or_macro_id);
        buf
    }

    fn to_action(self) -> ActionType {
        match self.button_type {
            BUTTON_TYPE_MACRO => ActionType::Macro,
            BUTTON_TYPE_HID => {
                match self.subtype {
                    BUTTON_SUBTYPE_MOUSE => ActionType::Button,
                    BUTTON_SUBTYPE_KEYBOARD => ActionType::Key,
                    BUTTON_SUBTYPE_CONSUMER => ActionType::Special,
                    _ => ActionType::Unknown,
                }
            }
            BUTTON_TYPE_SPECIAL => ActionType::Special,
            BUTTON_TYPE_DISABLED => ActionType::None,
            _ => ActionType::Unknown,
        }
    }

    fn from_action(action: ActionType, mapping_value: u32) -> Self {
        let mut button_type = BUTTON_TYPE_DISABLED;
        let mut subtype = 0;
        let mut control_id = 0u16;

        match action {
            ActionType::Macro => {
                button_type = BUTTON_TYPE_MACRO;
                control_id = mapping_value as u16;
            }
            ActionType::Button => {
                button_type = BUTTON_TYPE_HID;
                subtype = BUTTON_SUBTYPE_MOUSE;
                /* EEPROM stores a big-endian bit mask: bit (n-1) set = button n.
                 * This matches the C hidpp20_buttons_from_cpu encoding. */
                let mask: u16 = if mapping_value > 0 && mapping_value <= 16 {
                    1u16 << (mapping_value - 1)
                } else {
                    0
                };
                return Self {
                    button_type,
                    subtype,
                    control_id_or_macro_id: mask.to_be_bytes(),
                };
            }
            ActionType::Key => {
                button_type = BUTTON_TYPE_HID;
                subtype = BUTTON_SUBTYPE_KEYBOARD;
                control_id = mapping_value as u16;
            }
            ActionType::Special => {
                button_type = BUTTON_TYPE_SPECIAL;
                control_id = hidpp20_special_to_raw(mapping_value) as u16;
            }
            _ => {}
        }

        Self {
            button_type,
            subtype,
            control_id_or_macro_id: control_id.to_le_bytes(),
        }
    }
}

/* ---------------------------------------------------------------------- */
/* HID++ 2.0 special-action translation tables                            */
/*                                                                        */
/* The hardware stores small raw opcodes (0x01–0x0b) in the button        */
/* binding for BUTTON_TYPE_SPECIAL.  DBus clients (e.g. Piper) expect the */
/* canonical ratbag_button_action_special enum values (base = 1 << 30).   */
/* These two helpers mirror the C hidpp20_profiles_specials[] table.       */
/* ---------------------------------------------------------------------- */

/* Convert a raw HID++ 2.0 special opcode (0x00–0x0b) read from the
 * device into the canonical special_action constant for DBus exposure. */
fn hidpp20_raw_to_special(raw: u8) -> u32 {
    use crate::device::special_action as sa;
    match raw {
        0x01 => sa::WHEEL_LEFT,
        0x02 => sa::WHEEL_RIGHT,
        0x03 => sa::RESOLUTION_UP,
        0x04 => sa::RESOLUTION_DOWN,
        0x05 => sa::RESOLUTION_CYCLE_UP,
        0x06 => sa::RESOLUTION_DEFAULT,
        0x07 => sa::RESOLUTION_ALTERNATE,
        0x08 => sa::PROFILE_UP,
        0x09 => sa::PROFILE_DOWN,
        0x0a => sa::PROFILE_CYCLE_UP,
        0x0b => sa::SECOND_MODE,
        _    => sa::UNKNOWN,
    }
}

/* Convert a canonical special_action constant back to the raw HID++ 2.0
 * opcode that the hardware expects when writing a button binding. */
fn hidpp20_special_to_raw(special: u32) -> u8 {
    use crate::device::special_action as sa;
    match special {
        sa::WHEEL_LEFT            => 0x01,
        sa::WHEEL_RIGHT           => 0x02,
        sa::RESOLUTION_UP         => 0x03,
        sa::RESOLUTION_DOWN       => 0x04,
        sa::RESOLUTION_CYCLE_UP   => 0x05,
        sa::RESOLUTION_DEFAULT    => 0x06,
        sa::RESOLUTION_ALTERNATE  => 0x07,
        sa::PROFILE_UP            => 0x08,
        sa::PROFILE_DOWN          => 0x09,
        sa::PROFILE_CYCLE_UP      => 0x0a,
        sa::SECOND_MODE           => 0x0b,
        _                         => 0x00,
    }
}

/* Feature 0x8100: Onboard Profiles */
#[derive(Debug, Clone, Copy, Default)]
struct Hidpp20OnboardProfilesInfo {
    profile_count: u8,
    profile_count_oob: u8,
    button_count: u8,
    sector_size: [u8; 2],  /* Big Endian u16 */
}

impl Hidpp20OnboardProfilesInfo {
    fn from_bytes(buf: &[u8; 16]) -> Self {
        /* Byte layout (see C struct hidpp20_onboard_profiles_desc):
         *   [0] memory_model      – unused
         *   [1] profile_format_id – unused
         *   [2] macro_format_id   – unused
         *   [3] profile_count
         *   [4] profile_count_oob
         *   [5] button_count
         *   [6] sector_count      – unused
         *   [7..9] sector_size    (BE u16)
         *   [9] mechanical_layout – unused
         *   [10..16] reserved     – unused
         */
        let profile_count = buf[3];
        let profile_count_oob = buf[4];
        let button_count = buf[5];
        let mut sector_size = [0u8; 2];
        sector_size.copy_from_slice(&buf[7..9]);
        Self { profile_count, profile_count_oob, button_count, sector_size }
    }
    fn sector_size(&self) -> u16 {
        u16::from_be_bytes(self.sector_size)
    }
}


pub struct Hidpp20Driver {
    device_index: u8,
    features: FeatureMap,
    cached_onboard_info: Option<Hidpp20OnboardProfilesInfo>,
    /* Cached hardware report rate (in Hz) read at probe time, used to skip
     * redundant setReportRate calls that some firmware rejects. */
    cached_report_rate_hz: u32,
    /* Set when any onboard-profile sector CRC check fails; triggers a full
     * rewrite/rebuild attempt on the next commit. */
    needs_eeprom_repair: bool,
}

impl Hidpp20Driver {
    pub fn new() -> Self {
        Self {
            device_index: DEVICE_IDX_RECEIVER,
            features: FeatureMap::default(),
            cached_onboard_info: None,
            cached_report_rate_hz: 0,
            needs_eeprom_repair: false,
        }
    }

    /* Attempt a HID++ 2.0 protocol version probe at a specific device index. */
    /* Returns `Some((major, minor))` on success, `None` on timeout or error. */
    async fn try_probe_index(
        &self,
        io: &mut DeviceIo,
        idx: u8,
    ) -> Option<(u8, u8)> {
        let request = hidpp::build_hidpp20_request(
            idx,
            ROOT_FEATURE_INDEX,
            ROOT_FN_GET_PROTOCOL_VERSION,
            SW_ID,
            &[],
        );

        io.request(&request, 20, 2, move |buf| {
            let report = HidppReport::parse(buf)?;
            if report.is_error() {
                return None;
            }
            if !report.matches_hidpp20(idx, ROOT_FEATURE_INDEX) {
                return None;
            }
            if let HidppReport::Long { params, .. } = report {
                Some((params[0], params[1]))
            } else {
                None
            }
        })
        .await
        .ok()
    }

    /* Query the Root feature (0x0000, fn 0) to find the runtime index of */
    /* a given feature page. Returns `None` if the device does not support it. */
    async fn get_feature_index(
        &self,
        io: &mut DeviceIo,
        feature_page: u16,
    ) -> Result<Option<u8>> {
        let [hi, lo] = feature_page.to_be_bytes();

        let request = hidpp::build_hidpp20_request(
            self.device_index,
            ROOT_FEATURE_INDEX,
            ROOT_FN_GET_FEATURE,
            SW_ID,
            &[hi, lo],
        );

        let dev_idx = self.device_index;
        io.request(&request, 20, 3, move |buf| {
            match hidpp::match_hidpp20_feature_response(buf, dev_idx, ROOT_FEATURE_INDEX) {
                /* An error from the Root feature means the page is not supported. */
                Hidpp20MatchResult::HidppErr(_) => Some(None),
                Hidpp20MatchResult::Ok(params) => {
                    let index = params[0];
                    Some(if index == 0 { None } else { Some(index) })
                }
                Hidpp20MatchResult::NoMatch => None,
            }
        })
        .await
        .with_context(|| format!("Feature lookup for 0x{feature_page:04X} failed"))
    }

    /* Send a HID++ 2.0 feature request and return the 16-byte response payload. */
    /*                                                                          */
    /* The matcher accepts:                                                     */
    /* - Long responses  → full 16-byte params returned directly.               */
    /* - Short responses → 3-byte params zero-padded to 16 bytes (some SET      */
    /*   commands on wireless devices acknowledge with short reports).           */
    /* - HID++ error responses (both Long 0xFF and Short 0x8F) → surfaced       */
    /*   immediately as `Err` with the decoded error name.                      */
    async fn feature_request(
        &self,
        io: &mut DeviceIo,
        feature_index: u8,
        function: u8,
        params: &[u8],
    ) -> Result<[u8; 16]> {
        let request = hidpp::build_hidpp20_request(
            self.device_index,
            feature_index,
            function,
            SW_ID,
            params,
        );

        let dev_idx = self.device_index;
        let result = io
            .request(&request, 20, 3, move |buf| {
                match hidpp::match_hidpp20_feature_response(buf, dev_idx, feature_index) {
                    Hidpp20MatchResult::NoMatch => None,
                    other => Some(other),
                }
            })
            .await
            .with_context(|| {
                format!(
                    "Feature request (idx=0x{feature_index:02X}, fn={function}) failed"
                )
            })?;

        match result {
            Hidpp20MatchResult::Ok(p) => Ok(p),
            Hidpp20MatchResult::HidppErr(code) => {
                Err(hidpp::hidpp20_feature_error(code, feature_index, function))
            }
            Hidpp20MatchResult::NoMatch => unreachable!(),
        }
    }

    /* Send a HID++ 2.0 short (7-byte) feature request with no parameters. */
    /*                                                                      */
    /* Used for commands like MEMORY_WRITE_END that the C driver sends as   */
    /* `REPORT_ID_SHORT` with zero payload bytes.  The response matcher     */
    /* accepts both Short and Long replies and HID++ errors.                */
    async fn short_feature_request(
        &self,
        io: &mut DeviceIo,
        feature_index: u8,
        function: u8,
    ) -> Result<()> {
        let request = hidpp::build_hidpp20_short_request(
            self.device_index,
            feature_index,
            function,
            SW_ID,
        );

        let dev_idx = self.device_index;
        let result = io
            .request(&request, 20, 3, move |buf| {
                match hidpp::match_hidpp20_feature_response(buf, dev_idx, feature_index) {
                    Hidpp20MatchResult::NoMatch => None,
                    other => Some(other),
                }
            })
            .await
            .with_context(|| {
                format!(
                    "Short feature request (idx=0x{feature_index:02X}, fn={function}) failed"
                )
            })?;

        match result {
            Hidpp20MatchResult::Ok(_) => Ok(()),
            Hidpp20MatchResult::HidppErr(code) => {
                Err(hidpp::hidpp20_feature_error(code, feature_index, function))
            }
            Hidpp20MatchResult::NoMatch => unreachable!(),
        }
    }

    /* Send a HID++ 2.0 short (7-byte) feature request with parameters.
     *
     * The C driver sends SET_CURRENT_PROFILE and SET_CURRENT_DPI_INDEX as
     * short reports.  Some firmware silently drops long reports for these
     * commands, so matching the C behaviour is essential for compatibility. */
    async fn short_feature_request_with_params(
        &self,
        io: &mut DeviceIo,
        feature_index: u8,
        function: u8,
        params: &[u8],
    ) -> Result<()> {
        let request = hidpp::build_hidpp20_short_request_with_params(
            self.device_index, feature_index, function, SW_ID, params,
        );

        let dev_idx = self.device_index;
        let result = io
            .request(&request, 20, 3, move |buf| {
                match hidpp::match_hidpp20_feature_response(buf, dev_idx, feature_index) {
                    Hidpp20MatchResult::NoMatch => None,
                    other => Some(other),
                }
            })
            .await
            .with_context(|| {
                format!(
                    "Short feature request with params (idx=0x{feature_index:02X}, fn={function}) failed"
                )
            })?;

        match result {
            Hidpp20MatchResult::Ok(_) => Ok(()),
            Hidpp20MatchResult::HidppErr(code) => {
                Err(hidpp::hidpp20_feature_error(code, feature_index, function))
            }
            Hidpp20MatchResult::NoMatch => unreachable!(),
        }
    }

    /* Discover all supported features and cache their runtime indices. */
    async fn discover_features(&mut self, io: &mut DeviceIo) -> Result<()> {
        const FEATURE_QUERIES: &[(u16, &str)] = &[
            (PAGE_ADJUSTABLE_DPI, "Adjustable DPI"),
            (PAGE_ONBOARD_PROFILES, "Onboard Profiles"),
            (PAGE_COLOR_LED_EFFECTS, "Color LED Effects"),
            (PAGE_RGB_EFFECTS, "RGB Effects"),
            (PAGE_ADJUSTABLE_REPORT_RATE, "Adjustable Report Rate"),
        ];

        let mut found: Vec<String> = Vec::new();
        for &(page, name) in FEATURE_QUERIES {
            match self.get_feature_index(io, page).await {
                Ok(Some(idx)) => {
                    info!("  Feature {name} (0x{page:04X}) at index 0x{idx:02X}");
                    self.features.insert(page, idx);
                    found.push(format!("{name}=0x{idx:02X}"));
                }
                Ok(None) => {
                    info!("  Feature {name} (0x{page:04X}) not supported");
                }
                Err(e) => {
                    warn!("  Feature {name} (0x{page:04X}) query failed: {e}");
                }
            }
        }

        info!(
            "HID++ 2.0: discovered {} features: [{}]",
            found.len(),
            found.join(", ")
        );

        Ok(())
    }

    /* ---------------------------------------------------------------------- */
    /* Sector Memory Operations (PAGE_ONBOARD_PROFILES 0x8100)                */
    /* ---------------------------------------------------------------------- */

    /* Verify the CRC-CCITT checksum stored in the last two bytes (big-endian)
     * of a sector buffer, matching the C hidpp20_onboard_profiles_is_sector_valid.
     * Returns true when the CRC matches; logs a warning when it does not.
     * A mismatch is non-fatal — callers log it and continue with the data,
     * the same behaviour the legacy C driver exhibited. */
    fn verify_sector_crc(sector: u16, data: &[u8]) -> bool {
        if data.len() < 2 {
            warn!(
                "HID++ 2.0: sector 0x{sector:04X}: too short to validate CRC ({} bytes)",
                data.len()
            );
            return false;
        }
        let crc_offset = data.len() - 2;
        let computed = hidpp::compute_ccitt_crc(&data[..crc_offset]);
        let stored = u16::from_be_bytes([data[crc_offset], data[crc_offset + 1]]);
        if computed != stored {
            warn!(
                "HID++ 2.0: sector 0x{sector:04X}: CRC mismatch \
                 (stored 0x{stored:04X}, computed 0x{computed:04X})"
            );
            false
        } else {
            debug!("HID++ 2.0: sector 0x{sector:04X}: CRC OK (0x{stored:04X})");
            true
        }
    }

    async fn read_sector(
        &self,
        io: &mut DeviceIo,
        idx: u8,
        sector_index: u16,
        read_offset: u16,
        size: u16,
    ) -> Result<Vec<u8>> {
        let mut result = Vec::with_capacity(size as usize);
        let mut current_offset = read_offset;
        let end_offset = read_offset + size;

        while current_offset < end_offset {
            /* Firmware returns ERR_INVALID_ARGUMENT when a read would start within
             * the last 16 bytes of the sector but extend beyond it.  Rewind to
             * sector_size - 16 for the final partial chunk (mirrors C behaviour). */
            let chunk_size = (end_offset - current_offset).min(16);
            let effective_offset = if chunk_size < 16 {
                end_offset.saturating_sub(16)
            } else {
                current_offset
            };

            trace!(
                "HID++ 2.0: read_sector 0x{sector_index:04X} \
                 offset=0x{effective_offset:04X} chunk={chunk_size}B"
            );

            let mut bytes = [0u8; 16];
            bytes[0..2].copy_from_slice(&sector_index.to_be_bytes());
            bytes[2..4].copy_from_slice(&effective_offset.to_be_bytes());

            let response = self
                .feature_request(io, idx, PROFILES_FN_MEMORY_READ, &bytes)
                .await
                .context("Failed to read sector chunk")?;

            if effective_offset == current_offset {
                result.extend_from_slice(&response[..chunk_size as usize]);
            } else {
                let start_idx = 16 - chunk_size as usize;
                result.extend_from_slice(&response[start_idx..]);
            }
            current_offset += chunk_size;
        }
        
        Ok(result)
    }

    async fn write_sector(
        &self,
        io: &mut DeviceIo,
        idx: u8,
        sector_index: u16,
        write_offset: u16,
        data: &[u8],
    ) -> Result<()> {
        const WRITE_RETRIES: usize = 3;

        for attempt in 0..WRITE_RETRIES {
            let res = self
                .write_sector_once(io, idx, sector_index, write_offset, data)
                .await;

            match res {
                Ok(()) => return Ok(()),
                Err(e) if attempt + 1 < WRITE_RETRIES => {
                    warn!(
                        "HID++ 2.0: write_sector 0x{sector_index:04X} failed (attempt {} of {}): {e}",
                        attempt + 1,
                        WRITE_RETRIES
                    );
                    /* Some receivers reject rapid successive memWrite bursts; brief backoff mirrors C driver's retry behaviour. */
                    sleep(Duration::from_millis(15 * (attempt as u64 + 1))).await;
                }
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    async fn write_sector_once(
        &self,
        io: &mut DeviceIo,
        idx: u8,
        sector_index: u16,
        write_offset: u16,
        data: &[u8],
    ) -> Result<()> {
        let size = data.len() as u16;

        /* Step 1: Write Start command */
        let mut start_bytes = [0u8; 16];
        start_bytes[0..2].copy_from_slice(&sector_index.to_be_bytes());
        start_bytes[2..4].copy_from_slice(&write_offset.to_be_bytes()); // usually 0 for a full sector
        start_bytes[4..6].copy_from_slice(&size.to_be_bytes());

        /* 1. Initiate Write Sequence */
        self.feature_request(io, idx, PROFILES_FN_MEMORY_ADDR_WRITE, &start_bytes)
            .await
            .context("Failed to start sector write")?;

        /* 2. Iterate and Write Data Chunks (16 bytes at a time) */
        for chunk in data.chunks(16) {
            let mut payload = [0u8; 16];
            payload[..chunk.len()].copy_from_slice(chunk);
            self.feature_request(io, idx, PROFILES_FN_MEMORY_WRITE, &payload)
                .await
                .context("Failed to write sector chunk")?;
        }

        /* 3. Finalize Write — C sends a SHORT report with no parameters. */
        self.short_feature_request(io, idx, PROFILES_FN_MEMORY_WRITE_END)
            .await
            .context("Failed to end sector write")?;

        Ok(())
    }

    /* Query the supported DPI list from feature 0x2201 (getSensorCount +
     * getSensorDPIList).  Returns the expanded list, or an empty Vec if
     * the sensor count is zero or the list parse fails. */
    async fn query_sensor_dpi_list(
        &self,
        io: &mut DeviceIo,
        idx: u8,
    ) -> Result<Vec<u32>> {
        let sensor_info = self
            .feature_request(io, idx, DPI_FN_GET_SENSOR_COUNT, &[0])
            .await?;
        if sensor_info[0] == 0 {
            return Ok(Vec::new());
        }

        let list_data = self
            .feature_request(io, idx, DPI_FN_GET_SENSOR_DPI_LIST, &[0])
            .await?;

        let dpi_list = match parse_dpi_list_entries(&list_data[1..]) {
            Ok(list) => list,
            Err(e) => {
                warn!("HID++ 2.0: {e:#}, falling back to empty DPI list");
                Vec::new()
            }
        };

        if dpi_list.len() <= 1 {
            debug!(
                "HID++ 2.0: sensor 0 DPI list trivial ({} values), keeping defaults",
                dpi_list.len()
            );
        } else {
            debug!(
                "HID++ 2.0: sensor 0 DPI list ({} values): first={}, last={}",
                dpi_list.len(),
                dpi_list.first().unwrap_or(&0),
                dpi_list.last().unwrap_or(&0)
            );
        }

        Ok(dpi_list)
    }

    /* Read DPI sensor information using feature 0x2201. */
    async fn read_dpi_info(
        &self,
        io: &mut DeviceIo,
        profile: &mut ProfileInfo,
    ) -> Result<()> {
        let Some(idx) = self.features.adjustable_dpi else {
            return Ok(());
        };

        let dpi_list = self.query_sensor_dpi_list(io, idx).await?;

        /* Read current DPI (fn=2, getSensorDPI). */
        let dpi_data = self
            .feature_request(io, idx, DPI_FN_GET_SENSOR_DPI, &[0])
            .await?;

        let payload = Hidpp20DpiPayload::from_bytes(&dpi_data);
        let current_dpi = payload.current_dpi();
        let default_dpi = payload.default_dpi();

        /* Apply the queried DPI list and current value to all resolutions. */
        for res in &mut profile.resolutions {
            if !dpi_list.is_empty() {
                res.dpi_list = dpi_list.clone();
            }
            if res.is_active {
                res.dpi = Dpi::Unified(u32::from(current_dpi));
            }
        }

        debug!("HID++ 2.0: sensor 0 current DPI = {current_dpi} (default = {default_dpi})");
        Ok(())
    }

    /* Read report rate using feature 0x8060. */
    async fn read_report_rate(
        &mut self,
        io: &mut DeviceIo,
        profile: &mut ProfileInfo,
    ) -> Result<()> {
        let Some(idx) = self.features.report_rate else {
            return Ok(());
        };

        let list_data = self
            .feature_request(io, idx, RATE_FN_GET_REPORT_RATE_LIST, &[])
            .await?;

        profile.report_rates = hidpp::decode_report_rate_bitmap(list_data[0]);

        let rate_data = self
            .feature_request(io, idx, RATE_FN_GET_REPORT_RATE, &[])
            .await?;
        let current_rate_ms = u32::from(rate_data[0]);
        if current_rate_ms > 0 {
            profile.report_rate = 1000 / current_rate_ms;
            self.cached_report_rate_hz = profile.report_rate;
        }
        Ok(())
    }

    /* Read LED zone effect from the device using feature 0x8070. */
    async fn read_led_info(
        &self,
        io: &mut DeviceIo,
        profile: &mut ProfileInfo,
    ) -> Result<()> {
        let Some(idx) = self.features.color_led_effects else {
            return Ok(());
        };

        for led in &mut profile.leds {
            let zone_index = led.index as u8;
            let response = self
                .feature_request(io, idx, LED_FN_GET_ZONE_EFFECT, &[zone_index])
                .await?;

            let parsed = Hidpp20LedGetZonePayload::from_bytes(&response);

            if parsed.zone_index != zone_index {
                warn!("LED read: zone mismatch (expected {zone_index}, got {})", parsed.zone_index);
                continue;
            }

            let decoded = hidpp::parse_led_payload(&parsed.payload);
            led.mode = decoded.mode;
            led.color = decoded.color;
            led.secondary_color = decoded.secondary_color;
            led.effect_duration = decoded.effect_duration;
            led.brightness = decoded.brightness;

            debug!("LED zone {zone_index}: mode={:?}", led.mode);
        }

        Ok(())
    }

    /* Write LED zone effect to the device using feature 0x8070. */
    /* TriColor mode is routed through feature 0x8071 (RGB Effects) instead. */
    async fn write_led_info(
        &self,
        io: &mut DeviceIo,
        profile: &ProfileInfo,
    ) -> Result<()> {
        for led in &profile.leds {
            let zone_index = led.index as u8;

            /* Select the target feature and function based on mode.
             * TriColor uses 0x8071 (RGB Effects, fn 0x02); everything
             * else uses 0x8070 (Color LED Effects, fn SET_ZONE_EFFECT). */
            let (feature_idx, function, context_msg) = if led.mode == LedMode::TriColor {
                let Some(idx) = self.features.rgb_effects else {
                    warn!("TriColor requested but device lacks RGB Effects (0x8071)");
                    continue;
                };
                (idx, 0x02u8, "TriColor multi-LED cluster pattern")
            } else {
                let Some(idx) = self.features.color_led_effects else {
                    warn!("Device lacks Color LED Effects (0x8070)");
                    continue;
                };
                (idx, LED_FN_SET_ZONE_EFFECT, "LED zone effect")
            };

            let led_payload = hidpp::build_led_payload(led);
            let mut req_payload = Hidpp20LedSetZonePayload {
                zone_index,
                payload: [0; 11],
                persist: 0x01,
                padding: [0; 3],
            };
            req_payload.payload.copy_from_slice(&led_payload);

            let bytes = req_payload.into_bytes();
            self.feature_request(io, feature_idx, function, &bytes[0..13])
                .await
                .with_context(|| format!("Failed to write {context_msg}"))?;

            debug!("HID++ 2.0: committed LED zone {zone_index} mode={:?}", led.mode);
        }

        Ok(())
    }

    /* Write DPI sensor information using feature 0x2201. */
    async fn write_dpi_info(
        &self,
        io: &mut DeviceIo,
        profile: &ProfileInfo,
    ) -> Result<()> {
        let Some(idx) = self.features.adjustable_dpi else {
            return Ok(());
        };

        if let Some(res) = profile.resolutions.iter().find(|r| r.is_active)
            && let Dpi::Unified(dpi_val) = res.dpi
        {
            let mut payload = Hidpp20DpiPayload {
                sensor_index: 0,
                current_dpi: [0; 2],
                default_dpi: [0; 2],
                padding: [0; 11],
            };
            payload.set_current_dpi(dpi_val as u16);

            let bytes = payload.into_bytes();
            /* setSensorDPI is fn=3; only sensor_index + dpi_hi + dpi_lo are needed */
            let response = self.feature_request(io, idx, DPI_FN_SET_SENSOR_DPI, &bytes[0..3])
                .await
                .context("Failed to write DPI")?;
            let ack_payload = Hidpp20DpiPayload::from_bytes(&response);
            let actual_dpi = ack_payload.current_dpi();
            debug!("HID++ 2.0: committed DPI = {} (device ack: {})", dpi_val, actual_dpi);
        }
        Ok(())
    }

    /* Write report rate using feature 0x8060. */
    async fn write_report_rate(
        &self,
        io: &mut DeviceIo,
        profile: &ProfileInfo,
    ) -> Result<()> {
        const RATE_FN_SET_REPORT_RATE: u8 = 0x02;

        let Some(idx) = self.features.report_rate else {
            return Ok(());
        };

        if profile.report_rate > 0 {
            /* Some firmware returns INVALID_ARGUMENT when asked to set the
             * rate that is already active. Skip the write when unchanged. */
            if profile.report_rate == self.cached_report_rate_hz {
                debug!("HID++ 2.0: report rate unchanged at {} Hz, skipping write", profile.report_rate);
                return Ok(());
            }
            let rate_ms = (1000 / profile.report_rate) as u8;
            self.feature_request(io, idx, RATE_FN_SET_REPORT_RATE, &[rate_ms])
                .await
                .context("Failed to write report rate")?;
            debug!("HID++ 2.0: committed report rate = {} Hz", profile.report_rate);
        }
        Ok(())
    }

    /* ---------------------------------------------------------------------- */
    /* Helpers: query device-wide capabilities for UI validation               */
    /* ---------------------------------------------------------------------- */

    /* Query the DPI sensor range/list via feature 0x2201 (Adjustable DPI).
     * Returns the expanded list of supported DPI values, or `None` if the
     * feature is absent.  This is device-wide information used for the UI
     * (Piper) — it does NOT read the current DPI setting. */
    async fn query_dpi_sensor_range(
        &self,
        io: &mut DeviceIo,
    ) -> Option<Vec<u32>> {
        let idx = self.features.adjustable_dpi?;
        let dpi_list = self.query_sensor_dpi_list(io, idx).await.ok()?;
        if dpi_list.is_empty() { None } else { Some(dpi_list) }
    }

    /* Query the supported report rate list via feature 0x8060.
     * Returns the list of supported rates in Hz, or `None` if absent. */
    async fn query_report_rate_list(
        &self,
        io: &mut DeviceIo,
    ) -> Option<Vec<u32>> {
        let idx = self.features.report_rate?;

        let list_data = self
            .feature_request(io, idx, RATE_FN_GET_REPORT_RATE_LIST, &[])
            .await
            .ok()?;

        let rates = hidpp::decode_report_rate_bitmap(list_data[0]);
        debug!("HID++ 2.0: report rate list query → {:?}", rates);

        if rates.is_empty() { None } else { Some(rates) }
    }

    /* ---------------------------------------------------------------------- */
    /* Helpers: parse / serialize EEPROM LED structs                           */
    /* ---------------------------------------------------------------------- */

    /* Parse a single 11-byte `hidpp20_internal_led` from the EEPROM sector
     * into a `LedInfo`.  Layout (from hidpp20.h):
     *   byte 0:    mode (LED_HW_MODE_*)
     *   bytes 1-10: mode-specific effect union */
    fn parse_eeprom_led(led_bytes: &[u8], led_index: usize) -> crate::device::LedInfo {
        /* Standard LED modes supported by HID++ 2.0 onboard-profile devices.
         * These match the mode bytes the firmware accepts in the EEPROM LED
         * struct.  Without this list, Piper sees an empty `Modes` property
         * and only shows the currently-active mode. */
        let standard_modes = vec![
            LedMode::Off,
            LedMode::Solid,
            LedMode::Cycle,
            LedMode::Breathing,
            LedMode::ColorWave,
            LedMode::Starlight,
        ];

        let decoded = hidpp::parse_led_payload(led_bytes);

        let led = crate::device::LedInfo {
            index: led_index as u32,
            mode: decoded.mode,
            modes: standard_modes,
            color: decoded.color,
            secondary_color: decoded.secondary_color,
            tertiary_color: Color::default(),
            color_depth: 8, /* 8-bit RGB */
            effect_duration: decoded.effect_duration,
            brightness: decoded.brightness,
        };

        debug!("EEPROM LED {led_index}: mode={:?} color={:?}", led.mode, led.color);
        led
    }

    /* Serialize a `LedInfo` into an 11-byte EEPROM LED struct for writing
     * back to the profile sector (offset 208). */
    fn serialize_eeprom_led(led: &crate::device::LedInfo) -> [u8; 11] {
        let mut buf = [0u8; 11];

        match led.mode {
            LedMode::Off => {
                buf[0] = LED_HW_MODE_OFF;
            }
            LedMode::Solid => {
                buf[0] = LED_HW_MODE_FIXED;
                let c = led.color.to_rgb();
                buf[1] = c.r;
                buf[2] = c.g;
                buf[3] = c.b;
            }
            LedMode::Cycle => {
                buf[0] = LED_HW_MODE_CYCLE;
                let period = led.effect_duration as u16;
                buf[6..8].copy_from_slice(&period.to_be_bytes());
                buf[8] = (led.brightness * 100 / 255) as u8;
            }
            LedMode::ColorWave => {
                buf[0] = LED_HW_MODE_COLOR_WAVE;
                let period = led.effect_duration as u16;
                buf[6..8].copy_from_slice(&period.to_be_bytes());
                buf[8] = (led.brightness * 100 / 255) as u8;
            }
            LedMode::Starlight => {
                buf[0] = LED_HW_MODE_STARLIGHT;
                let c = led.color.to_rgb();
                buf[1] = c.r;
                buf[2] = c.g;
                buf[3] = c.b;
                let sc = led.secondary_color.to_rgb();
                buf[4] = sc.r;
                buf[5] = sc.g;
                buf[6] = sc.b;
            }
            LedMode::Breathing => {
                buf[0] = LED_HW_MODE_BREATHING;
                let c = led.color.to_rgb();
                buf[1] = c.r;
                buf[2] = c.g;
                buf[3] = c.b;
                let period = led.effect_duration as u16;
                buf[4..6].copy_from_slice(&period.to_be_bytes());
                /* byte 6 = waveform, keep 0 */
                buf[7] = (led.brightness * 100 / 255) as u8;
            }
            _ => {
                /* TriColor or unknown — leave as OFF */
                buf[0] = LED_HW_MODE_OFF;
            }
        }

        buf
    }
}

#[async_trait]
impl super::DeviceDriver for Hidpp20Driver {
    fn name(&self) -> &str {
        "Logitech HID++ 2.0"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Try the wireless receiver index first (most gaming mice are wireless), */
        /* then fall back to the corded device index.                             */
        const PROBE_INDICES: &[u8] = &[DEVICE_IDX_RECEIVER, DEVICE_IDX_CORDED];

        for &idx in PROBE_INDICES {
            if let Some((major, minor)) = self.try_probe_index(io, idx).await {
                self.device_index = idx;
                info!(
                    "HID++ 2.0 device detected at index 0x{idx:02X} (protocol {major}.{minor})"
                );
                self.discover_features(io).await?;
                return Ok(());
            }
            debug!("HID++ 2.0 probe at index 0x{idx:02X}: no response");
        }

        anyhow::bail!(
            "HID++ 2.0 protocol version probe failed (tried indices: {:02X?})",
            PROBE_INDICES
        );
    }

    async fn load_profiles(
        &mut self,
        io: &mut DeviceIo,
        info: &mut DeviceInfo,
    ) -> Result<()> {
        let has_g305_quirk = info
            .driver_config
            .quirks
            .iter()
            .any(|q| q == "G305");

        /* If the device has PAGE_ONBOARD_PROFILES (0x8100), we initialize based on hardware capacity */
        if let Some(idx) = self.features.onboard_profiles {
            info!("HID++ 2.0: onboard_profiles feature found at index 0x{idx:02X}");

            let desc_data = self
                .feature_request(io, idx, PROFILES_FN_GET_PROFILES_DESCR, &[])
                .await
                .context("Failed to get Onboard Profiles Description")?;

            info!(
                "HID++ 2.0: raw descriptor bytes: {:02X?}",
                &desc_data[..16]
            );

            let desc = Hidpp20OnboardProfilesInfo::from_bytes(&desc_data);
            self.cached_onboard_info = Some(desc);

            let mut profile_count = desc.profile_count as usize;
            if profile_count <= 1 && desc.profile_count_oob > 1 {
                warn!(
                    "HID++ 2.0: descriptor reports profile_count={} but profile_count_oob={} — using oob count",
                    desc.profile_count,
                    desc.profile_count_oob
                );
                profile_count = desc.profile_count_oob as usize;
            }
            if profile_count == 0 {
                profile_count = 1;
            }

            let button_count = desc.button_count as usize;

            info!(
                "HID++ 2.0: Hardware described profiles={} (oob={}) buttons={} sector_size={}",
                profile_count,
                desc.profile_count_oob,
                button_count,
                desc.sector_size()
            );

            /* ----------------------------------------------------------------
             * Ensure the device is in onboard mode before reading profiles.
             * The C driver calls hidpp20_onboard_profiles_get_onboard_mode()
             * and switches to HIDPP20_ONBOARD_MODE (1) if it is not already
             * there.  Without this step some firmware may return stale or
             * unexpected data from sector reads.
             * ---------------------------------------------------------------- */
            match self
                .feature_request(io, idx, PROFILES_FN_GET_MODE, &[])
                .await
            {
                Ok(mode_resp) => {
                    let current_mode = mode_resp[0];
                    info!("HID++ 2.0: current onboard mode = {current_mode}");
                    if current_mode != ONBOARD_MODE_ONBOARD {
                        info!("HID++ 2.0: switching to onboard mode (was {current_mode})");
                        if let Err(e) = self
                            .feature_request(io, idx, PROFILES_FN_SET_MODE, &[ONBOARD_MODE_ONBOARD])
                            .await
                        {
                            warn!("HID++ 2.0: failed to set onboard mode: {e}");
                        }
                    }
                }
                Err(e) => {
                    warn!("HID++ 2.0: failed to get onboard mode: {e} (continuing)");
                }
            }

            /* Resize the Ratbag device abstraction to exactly match the hardware capabilities */
            info.profiles.resize_with(profile_count, ProfileInfo::default);
            for (i, p) in info.profiles.iter_mut().enumerate() {
                p.index = i as u32;
                p.buttons.resize_with(button_count, crate::device::ButtonInfo::default);
                for (b_idx, b) in p.buttons.iter_mut().enumerate() {
                    b.index = b_idx as u32;
                }
            }

            let sector_size = desc.sector_size();

            /* ----------------------------------------------------------------
             * Read the root profile directory sector (0x0000).
             *
             * The G305 has a firmware bug where it throws ERR_INVALID_ARGUMENT
             * when the user sector has never been written.  The C driver
             * handles this via HIDPP20_QUIRK_G305: on error, it sets
             * read_userdata = false and reads ROM profiles instead.  We
             * replicate this fallback here.
             * ---------------------------------------------------------------- */
            let (root_sector_data, read_userdata) = match self
                .read_sector(io, idx, USER_PROFILES_BASE, 0, sector_size)
                .await
            {
                Ok(data) => {
                    let crc_ok = Self::verify_sector_crc(USER_PROFILES_BASE, &data);
                    if !crc_ok {
                        self.needs_eeprom_repair = true;
                        warn!(
                            "HID++ 2.0: profile dictionary CRC invalid; \
                             will read ROM profiles instead of corrupted EEPROM"
                        );
                    }
                    (Some(data), crc_ok)
                }
                Err(e) => {
                    if has_g305_quirk {
                        info!(
                            "HID++ 2.0: G305 quirk — root sector read failed ({e}), \
                             falling back to ROM profiles"
                        );
                    } else {
                        warn!(
                            "HID++ 2.0: root sector read failed ({e}), \
                             falling back to ROM profiles"
                        );
                    }
                    (None, false)
                }
            };

            /* Build per-profile address/enabled metadata.
             * Default to user-EEPROM addressing (sector = USER_PROFILES_BASE | (i + 1)),
             * then override from the dictionary entries when they look valid. */
            let mut profile_addrs: Vec<u16> =
                (0..profile_count).map(|i| USER_PROFILES_BASE | ((i as u16) + 1)).collect();
            let mut profile_enabled: Vec<bool> = vec![true; profile_count];

            if read_userdata {
                if let Some(ref root_data) = root_sector_data {
                    for i in 0..profile_count {
                        let offset = i * 4;
                        if offset + 4 > root_data.len() {
                            break;
                        }

                        let addr = u16::from_be_bytes([
                            root_data[offset],
                            root_data[offset + 1],
                        ]);
                        if addr == 0xFFFF {
                            break;
                        }
                        if addr != 0 {
                            profile_addrs[i] = addr;
                        }
                        profile_enabled[i] = root_data[offset + 2] != 0;
                    }
                }
            } else {
                /* No valid user directory — use ROM profile addresses.
                 * The C driver uses HIDPP20_ROM_PROFILES_G402 + i + 1, and
                 * when i >= num_rom_profiles it reuses the first ROM profile. */
                let num_rom = desc.profile_count_oob as usize;
                for i in 0..profile_count {
                    let rom_idx = if num_rom > 0 && i < num_rom { i } else { 0 };
                    profile_addrs[i] = ROM_PROFILES_BASE | ((rom_idx as u16) + 1);
                    profile_enabled[i] = true;
                }
                info!(
                    "HID++ 2.0: using ROM profile addresses: {:04X?}",
                    profile_addrs
                );
            }

            for i in 0..profile_count {
                let addr = profile_addrs[i];
                let enabled = profile_enabled[i];

                if addr == 0xFFFF || addr == 0 {
                    continue;
                }

                /* Read the profile payload from EEPROM or ROM. */
                let profile_data = match self.read_sector(io, idx, addr, 0, sector_size).await {
                    Ok(data) => data,
                    Err(e) => {
                        warn!(
                            "HID++ 2.0: failed to read profile sector 0x{addr:04X}: {e}; \
                             skipping profile {i}"
                        );
                        continue;
                    }
                };

                /* Validate profile sector CRC.  When it fails, skip parsing
                 * the corrupted data — matching the C driver which returns
                 * -EAGAIN and falls back to ROM for that profile. */
                let crc_ok = Self::verify_sector_crc(addr, &profile_data);
                if !crc_ok {
                    self.needs_eeprom_repair = true;
                    warn!(
                        "HID++ 2.0: profile {i} sector 0x{addr:04X} has bad CRC; \
                         skipping EEPROM data, will use live hardware state"
                    );
                    continue;
                }

                let p = &mut info.profiles[i];
                p.is_enabled = enabled;

                /* --- Report rate (byte 0): stored as ms-interval, convert to Hz --- */
                if !profile_data.is_empty() && profile_data[0] > 0 {
                    p.report_rate = 1000 / (profile_data[0] as u32);
                    debug!("HID++ 2.0: profile {i} EEPROM report rate = {} Hz (interval {}ms)",
                           p.report_rate, profile_data[0]);
                }

                /* --- DPI list (bytes 3-12): 5 entries × 2 bytes LE --- */
                /* A raw value of 0 means the resolution slot is disabled  */
                /* but the slot must still appear on DBus with             */
                /* IsDisabled = true, matching the C daemon's behaviour.   */
                /* Piper expects to see all slots so users can enable them. */
                let mut eeprom_dpis: Vec<(u32, bool)> = Vec::new();
                for d_idx in 0..EEPROM_DPI_COUNT {
                    let d_off = EEPROM_DPI_OFFSET + d_idx * EEPROM_DPI_ENTRY_SIZE;
                    if d_off + 2 <= profile_data.len() {
                        let raw = u16::from_le_bytes([profile_data[d_off], profile_data[d_off + 1]]);
                        if raw == 0 || raw == 0xFFFF {
                            eeprom_dpis.push((0, true));
                        } else {
                            eeprom_dpis.push((u32::from(raw), false));
                        }
                    }
                }

                /* Default-DPI index (byte 1) */
                let default_dpi_idx = if profile_data.len() > 1 {
                    profile_data[1] as usize
                } else {
                    0
                };

                if !eeprom_dpis.is_empty() {
                    debug!("HID++ 2.0: profile {i} EEPROM DPIs: {:?} (default idx {})",
                           eeprom_dpis, default_dpi_idx);

                    /* Rebuild the resolutions list to match the EEPROM entries. */
                    p.resolutions.clear();
                    for (r_idx, &(dpi_val, disabled)) in eeprom_dpis.iter().enumerate() {
                        p.resolutions.push(crate::device::ResolutionInfo {
                            index: r_idx as u32,
                            dpi: crate::device::Dpi::Unified(dpi_val),
                            dpi_list: Vec::new(), /* filled later by read_dpi_info */
                            capabilities: Vec::new(),
                            is_active: !disabled && r_idx == default_dpi_idx,
                            is_default: !disabled && r_idx == default_dpi_idx,
                            is_disabled: disabled,
                        });
                    }
                }

                /* --- Buttons (offset 32, 4 bytes each) --- */
                let max_buttons = button_count.min(16);
                for b_idx in 0..max_buttons {
                    let btn_offset = EEPROM_BUTTON_OFFSET + (b_idx * EEPROM_BUTTON_SIZE);
                    if btn_offset + EEPROM_BUTTON_SIZE <= profile_data.len() {
                        let mut binding_bytes = [0u8; 4];
                        binding_bytes.copy_from_slice(&profile_data[btn_offset..btn_offset + 4]);
                        let binding = Hidpp20ButtonBinding::from_bytes(&binding_bytes);

                        p.buttons[b_idx].action_type = binding.to_action();

                        /* EEPROM mouse buttons are stored as a big-endian bit mask
                         * (matching the C hidpp20_buttons_to_cpu / buttons_from_cpu).
                         * ffs(mask) gives the 1-based button ordinal. */
                        let mapping_value = if binding.button_type == BUTTON_TYPE_HID
                            && binding.subtype == BUTTON_SUBTYPE_MOUSE
                        {
                            let mask = u16::from_be_bytes(binding.control_id_or_macro_id);
                            if mask > 0 {
                                u32::from(mask.trailing_zeros()) + 1
                            } else {
                                0
                            }
                        } else if binding.button_type == BUTTON_TYPE_SPECIAL {
                            /* Translate the raw HID++ special opcode to the
                             * canonical special_action constant for DBus. */
                            let raw = u16::from_be_bytes(binding.control_id_or_macro_id) as u8;
                            hidpp20_raw_to_special(raw)
                        } else {
                            u16::from_be_bytes(binding.control_id_or_macro_id) as u32
                        };
                        p.buttons[b_idx].mapping_value = mapping_value;

                        debug!(
                            "HID++ 2.0: profile {i} button {b_idx}: \
                             type=0x{:02X} sub=0x{:02X} raw=[{:02X},{:02X}] \
                             → action={:?} mapping={mapping_value}",
                            binding.button_type,
                            binding.subtype,
                            binding.control_id_or_macro_id[0],
                            binding.control_id_or_macro_id[1],
                            p.buttons[b_idx].action_type
                        );
                    }
                }

                /* --- LEDs (offset 208, 2 × 11 bytes) --- *
                 * The C struct places leds[HIDPP20_LED_COUNT] at offset 208
                 * inside the 256-byte packed union.  Each LED is 11 bytes
                 * (hidpp20_internal_led).  Parse them into the profile. */
                p.leds.clear();
                for led_idx in 0..EEPROM_LED_COUNT {
                    let off = EEPROM_LED_OFFSET + led_idx * EEPROM_LED_SIZE;
                    if off + EEPROM_LED_SIZE <= profile_data.len() {
                        let led = Self::parse_eeprom_led(
                            &profile_data[off..off + EEPROM_LED_SIZE],
                            led_idx,
                        );
                        p.leds.push(led);
                    }
                }
            }
        } else {
            /* No onboard profiles feature — create a single host-managed profile. */
            info!(
                "HID++ 2.0: no onboard profiles feature; using single host-managed profile"
            );
            if info.profiles.is_empty() {
                info.profiles.push(ProfileInfo::default());
            }
        }

        /* Query the hardware for which profile is currently active rather
         * than blindly assuming profile 0.  The C driver uses
         * hidpp20_onboard_profiles_get_current_profile() which returns a
         * 1-based sector index in parameters[1].  Fall back to profile 0
         * if the query fails (e.g. non-onboard-profiles device). */
        let active_profile_idx: u32 = if let Some(idx) = self.features.onboard_profiles {
            match self
                .feature_request(io, idx, PROFILES_FN_GET_CURRENT_PROFILE, &[])
                .await
            {
                Ok(resp) => {
                    /* resp[1] is the 1-based profile sector, convert to 0-based */
                    let sector = resp[1];
                    let zero_based = if sector > 0 { u32::from(sector) - 1 } else { 0 };
                    info!("HID++ 2.0: hardware reports active profile sector={sector}, index={zero_based}");
                    zero_based
                }
                Err(e) => {
                    warn!("HID++ 2.0: failed to get current profile: {e}, defaulting to 0");
                    0
                }
            }
        } else {
            0
        };

        for profile in &mut info.profiles {
            profile.is_active = profile.index == active_profile_idx;
        }
        if !info.profiles.iter().any(|p| p.is_active) {
            if let Some(first) = info.profiles.first_mut() {
                first.is_active = true;
            } else {
                warn!("HID++ 2.0: no profiles available after load");
            }
        }

        /* For the active profile, override the default_dpi_idx with the
         * hardware-reported current DPI index.  The EEPROM byte 1 is the
         * *default* index (the starting one after profile load), but the
         * user may have physically cycled DPIs via the mouse button.
         * C: hidpp20_onboard_profiles_get_current_dpi_index(). */
        if let Some(idx) = self.features.onboard_profiles {
            if let Some(active_p) = info.profiles.iter_mut().find(|p| p.is_active) {
                match self
                    .feature_request(io, idx, PROFILES_FN_GET_CURRENT_DPI_INDEX, &[])
                    .await
                {
                    Ok(resp) => {
                        let hw_dpi_idx = resp[0] as usize;
                        debug!(
                            "HID++ 2.0: hardware current DPI index = {} for active profile {}",
                            hw_dpi_idx, active_p.index
                        );
                        for res in &mut active_p.resolutions {
                            res.is_active = res.index as usize == hw_dpi_idx;
                        }
                    }
                    Err(e) => {
                        debug!("HID++ 2.0: failed to get current DPI index: {e}");
                    }
                }
            }
        }

        /* When onboard profiles are present, all per-profile values (DPI,
         * report rate, LEDs, buttons) were already read from the EEPROM
         * sectors above.  We only query the live features for:
         *   - DPI sensor list/range → used for UI validation in Piper
         *   - Report rate list → used for UI validation in Piper
         *
         * When onboard profiles are absent, we fall back to reading
         * everything from the live features instead. */
        if self.features.onboard_profiles.is_some() {
            /* Query sensor DPI list/range once and apply to all profiles
             * (the sensor capabilities are device-wide, not per-profile). */
            let dpi_range = self.query_dpi_sensor_range(io).await;
            let rate_list = self.query_report_rate_list(io).await;

            for profile in &mut info.profiles {
                if let Some(ref range) = dpi_range {
                    for res in &mut profile.resolutions {
                        res.dpi_list = range.clone();
                    }
                }
                if let Some(ref rates) = rate_list {
                    profile.report_rates = rates.clone();
                }
            }
        } else {
            /* Fallback: no onboard profiles — read everything from live
             * feature requests.  This only works for the single default
             * profile since live features reflect hardware state, not
             * stored profile state. */
            for profile in &mut info.profiles {
                if let Err(e) = self.read_dpi_info(io, profile).await {
                    warn!("Failed to read DPI for profile {}: {e}", profile.index);
                }
                if let Err(e) = self.read_report_rate(io, profile).await {
                    warn!("Failed to read report rate for profile {}: {e}", profile.index);
                }
                if let Err(e) = self.read_led_info(io, profile).await {
                    warn!("Failed to read LEDs for profile {}: {e}", profile.index);
                }
            }
        }

        info!("HID++ 2.0: loaded {} profiles", info.profiles.len());
        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        /* When onboard profiles (0x8100) are present the firmware reads all
         * per-profile settings (DPI, report rate, LEDs) from the EEPROM
         * sectors.  We must NOT call the live feature set commands
         * (setSensorDPI 0x2201, setReportRate 0x8060, setZoneEffect 0x8070)
         * because those immediately change hardware state — making it look
         * like a DPI switch instead of a profile switch.
         *
         * When onboard profiles are ABSENT we are in host-managed mode and
         * the live feature calls are the only way to change settings. */
        if self.features.onboard_profiles.is_none() {
            if let Some(profile) = info.profiles.iter().find(|p| p.is_active) {
                if let Err(e) = self.write_dpi_info(io, profile).await {
                    warn!("Failed to commit DPI for profile {}: {e:#}", profile.index);
                }
                if let Err(e) = self.write_report_rate(io, profile).await {
                    warn!("Failed to commit report rate for profile {}: {e:#}", profile.index);
                }
                if let Err(e) = self.write_led_info(io, profile).await {
                    warn!("Failed to commit LEDs for profile {}: {e:#}", profile.index);
                }
            }
        }

        /* Onboard Profiles (0x8100) EEPROM commit logic */
        if let Some(idx) = self.features.onboard_profiles {
            if let Some(desc) = self.cached_onboard_info {
                let sector_size = desc.sector_size();
                let force_repair = self.needs_eeprom_repair;

                /* Switch to host mode before writing EEPROM. Firmware rejects
                 * memWrite calls while in onboard mode (INVALID_ARGUMENT). */
                if let Err(e) = self
                    .feature_request(io, idx, PROFILES_FN_SET_MODE, &[ONBOARD_MODE_HOST])
                    .await
                {
                    warn!("Failed to switch to host mode: {e:#}");
                }
                
                /* Write each dirty profile to its sector.  Like the legacy C
                 * driver (hidpp20_onboard_profiles_write_profile), the sector
                 * address is simply `profile_index + 1` (0-based index → sector
                 * 1, 2, 3 …).  We do NOT rely on the directory sector (0x0000)
                 * being valid before the first write — the G305 may have an
                 * uninitialised directory that throws ERR_INVALID_ARGUMENT. */
                let mut any_written = false;
                let mut last_err: Option<anyhow::Error> = None;
                for profile in &info.profiles {
                    if !profile.is_dirty && !force_repair {
                        continue;
                    }

                    /* C: sector = index + 1 */
                    let addr = (profile.index + 1) as u16;

                    /* Read existing sector to preserve unknown fields, then
                     * patch the fields ratbag manages.  If the read fails
                     * (e.g., uninitialised flash), start from an all-0xFF
                     * buffer matching C's memset approach.
                     *
                     * When force_repair is true the sector data is known-
                     * corrupted so there is nothing worth preserving — skip
                     * the read entirely and start from a clean 0xFF template.
                     * This saves sector_size/16 USB round-trips per profile. */
                    let mut profile_data = if force_repair {
                        vec![0xFFu8; sector_size as usize]
                    } else {
                        let mut data = self
                            .read_sector(io, idx, addr, 0, sector_size)
                            .await
                            .unwrap_or_else(|_| vec![0xFFu8; sector_size as usize]);
                        if data.len() < sector_size as usize {
                            data.resize(sector_size as usize, 0xFF);
                        }
                        data
                    };

                    /* 1. Report rate (byte 0): stored as ms-interval */
                    if profile.report_rate > 0 {
                        profile_data[0] = (1000 / profile.report_rate) as u8;
                    }

                    /* 2. Default-DPI index (byte 1) */
                    if let Some(def_idx) = profile.resolutions.iter().position(|r| r.is_default) {
                        profile_data[1] = def_idx as u8;
                    }

                    /* 3. DPI list (bytes 3-12, 5 × LE u16) */
                    for (i, res) in profile.resolutions.iter().enumerate().take(EEPROM_DPI_COUNT) {
                        if let Dpi::Unified(val) = res.dpi {
                            let dpi_bytes = (val as u16).to_le_bytes();
                            let d_off = EEPROM_DPI_OFFSET + i * EEPROM_DPI_ENTRY_SIZE;
                            profile_data[d_off] = dpi_bytes[0];
                            profile_data[d_off + 1] = dpi_bytes[1];
                        }
                    }

                    /* 4. Buttons (offset 32, 4 bytes each) */
                    let max_buttons = desc.button_count.min(16) as usize;
                    for btn in &profile.buttons {
                        let b_idx = btn.index as usize;
                        if b_idx < max_buttons {
                            let btn_offset = EEPROM_BUTTON_OFFSET + b_idx * EEPROM_BUTTON_SIZE;
                            if btn_offset + EEPROM_BUTTON_SIZE <= profile_data.len() {
                                let binding = Hidpp20ButtonBinding::from_action(
                                    btn.action_type,
                                    btn.mapping_value,
                                );
                                profile_data[btn_offset..btn_offset + 4]
                                    .copy_from_slice(&binding.into_bytes());
                            }
                        }
                    }

                    /* 5. LEDs (offset 208, 2 × 11 bytes) */
                    {
                        for led in &profile.leds {
                            let led_idx = led.index as usize;
                            if led_idx < 2 {
                                let off = EEPROM_LED_OFFSET + led_idx * EEPROM_LED_SIZE;
                                if off + EEPROM_LED_SIZE <= profile_data.len() {
                                    let led_data = Self::serialize_eeprom_led(led);
                                    profile_data[off..off + EEPROM_LED_SIZE]
                                        .copy_from_slice(&led_data);
                                }
                            }
                        }
                    }

                    /* 6. Recompute CRC (last 2 bytes, BE) */
                    let crc_offset = profile_data.len() - 2;
                    let crc = hidpp::compute_ccitt_crc(&profile_data[..crc_offset]);
                    let crc_bytes = crc.to_be_bytes();
                    profile_data[crc_offset] = crc_bytes[0];
                    profile_data[crc_offset + 1] = crc_bytes[1];

                    /* 7. Write sector */
                    match self.write_sector(io, idx, addr, 0, &profile_data).await {
                        Ok(()) => {
                            debug!(
                                "HID++ 2.0: committed profile {} → sector 0x{addr:04X}",
                                profile.index
                            );
                            any_written = true;
                        }
                        Err(e) => {
                            warn!("Failed to write EEPROM sector 0x{addr:04X} for profile {}: {e}", profile.index);
                            last_err = Some(e);
                        }
                    }
                }

                /* After writing profile sectors, rebuild the directory (sector
                 * 0x0000) — mirrors C's hidpp20_onboard_profiles_write_dict.
                 * Format: 4 bytes per profile [0x00, i+1, enabled, 0x00],
                 * followed by [0xFF, 0xFF, 0x00, 0x00], rest padded 0xFF,
                 * then CRC-CCITT in the last two bytes. */
                if any_written {
                    let mut dir = vec![0xFFu8; sector_size as usize];
                    let mut pos = 0usize;
                    for profile in &info.profiles {
                        if pos + 4 > dir.len().saturating_sub(2) { break; }
                        dir[pos]     = 0x00;
                        dir[pos + 1] = (profile.index + 1) as u8;
                        dir[pos + 2] = u8::from(profile.is_enabled);
                        dir[pos + 3] = 0x00;
                        pos += 4;
                    }
                    /* End-of-directory marker */
                    if pos + 4 <= dir.len().saturating_sub(2) {
                        dir[pos]     = 0xFF;
                        dir[pos + 1] = 0xFF;
                        dir[pos + 2] = 0x00;
                        dir[pos + 3] = 0x00;
                    }
                    /* CRC over the whole sector minus the last 2 bytes */
                    let dir_crc_off = dir.len() - 2;
                    let dir_crc = hidpp::compute_ccitt_crc(&dir[..dir_crc_off]);
                    let dir_crc_bytes = dir_crc.to_be_bytes();
                    dir[dir_crc_off]     = dir_crc_bytes[0];
                    dir[dir_crc_off + 1] = dir_crc_bytes[1];

                    if let Err(e) = self.write_sector(io, idx, 0x0000, 0, &dir).await {
                        warn!("HID++ 2.0: failed to write profile directory: {e}");
                        last_err = Some(e);
                    } else {
                        debug!("HID++ 2.0: wrote profile directory (sector 0x0000)");
                    }
                }

                /* Switch back to onboard mode after EEPROM writes. */
                if let Err(e) = self
                    .feature_request(io, idx, PROFILES_FN_SET_MODE, &[ONBOARD_MODE_ONBOARD])
                    .await
                {
                    warn!("Failed to switch back to onboard mode: {e:#}");
                }

                if let Some(e) = last_err {
                    /* Keep the flag set so we retry on the next commit. */
                    self.needs_eeprom_repair = true;
                    return Err(e);
                }

                /* Successful rewrite clears the repair flag. */
                self.needs_eeprom_repair = false;

                /* Tell the hardware which profile is now active.  The C driver
                 * calls hidpp20_onboard_profiles_set_current_profile() which
                 * uses function 0x03 with parameters[1] = 1-based sector.
                 * Without this, the device stays on whichever profile the
                 * firmware last selected and Piper's profile switching has no
                 * effect on the actual hardware output. */
                if let Some(active) = info.profiles.iter().find(|p| p.is_active) {
                    let sector = (active.index + 1) as u8;  /* 0-based → 1-based */
                    /* C driver uses REPORT_ID_SHORT for this command.
                     * Some firmware silently drops long reports here. */
                    if let Err(e) = self
                        .short_feature_request_with_params(
                            io,
                            idx,
                            PROFILES_FN_SET_CURRENT_PROFILE,
                            &[0x00, sector],
                        )
                        .await
                    {
                        warn!(
                            "HID++ 2.0: failed to set current profile to {} (sector {sector}): {e}",
                            active.index
                        );
                    } else {
                        debug!(
                            "HID++ 2.0: set current profile = {} (sector {sector})",
                            active.index
                        );
                    }

                    /* Also set the active DPI index within the profile.
                     * C: hidpp20_onboard_profiles_set_current_dpi_index()
                     * uses function 0x0C with parameters[0] = resolution index. */
                    if let Some(res) = active.resolutions.iter().find(|r| r.is_active) {
                        let dpi_idx = res.index as u8;
                        /* C driver uses REPORT_ID_SHORT for this command too. */
                        if let Err(e) = self
                            .short_feature_request_with_params(
                                io,
                                idx,
                                PROFILES_FN_SET_CURRENT_DPI_INDEX,
                                &[dpi_idx],
                            )
                            .await
                        {
                            warn!(
                                "HID++ 2.0: failed to set DPI index to {dpi_idx}: {e}"
                            );
                        } else {
                            debug!("HID++ 2.0: set current DPI index = {dpi_idx}");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /* Handle unsolicited HID++ 2.0 hardware events.
     *
     * The most important event is a profile-switch notification from feature
     * 0x8100 (Onboard Profiles).  When the user presses a physical profile
     * button, the hardware sends an unsolicited report with the new active
     * profile sector.  We parse this and update `DeviceInfo` accordingly.
     *
     * Returns `true` if the event caused a state change that the actor
     * should propagate via DBus signals. */
    async fn handle_event(
        &mut self,
        report: &[u8],
        info: &mut DeviceInfo,
    ) -> Result<bool> {
        let parsed = match HidppReport::parse(report) {
            Some(r) => r,
            None => return Ok(false),
        };

        /* We only care about reports addressed to our device. */
        let (dev_idx, sub_id, params) = match &parsed {
            HidppReport::Long { device_index, sub_id, params, .. } =>
                (*device_index, *sub_id, &params[..]),
            HidppReport::Short { device_index, sub_id, params, .. } =>
                (*device_index, *sub_id, &params[..]),
        };

        if dev_idx != self.device_index {
            return Ok(false);
        }

        /* Check if this is a notification from the Onboard Profiles feature. */
        if let Some(_onboard_idx) = self
            .features
            .onboard_profiles
            .filter(|&idx| sub_id == idx)
        {
            /* The function nibble is in the address byte (byte [3]).
             * For a profile-change notification, we expect the
             * GET_CURRENT_PROFILE function (0x04) as the response
             * function, with params[1] = 1-based sector index. */
            let function = (report[3] >> 4) & 0x0F;

            if function == PROFILES_FN_GET_CURRENT_PROFILE
                || function == PROFILES_FN_SET_CURRENT_PROFILE
            {
                let sector = if params.len() > 1 { params[1] } else { params[0] };
                if sector == 0 {
                    return Ok(false);
                }
                let new_profile_index = (sector - 1) as u32;

                let mut changed = false;
                for profile in &mut info.profiles {
                    let should_be_active = profile.index == new_profile_index;
                    if profile.is_active != should_be_active {
                        profile.is_active = should_be_active;
                        changed = true;
                    }
                }

                if changed {
                    debug!(
                        "HID++ 2.0: hardware profile switch detected -> profile {new_profile_index}"
                    );
                }

                return Ok(changed);
            }

            /* DPI index change notification. */
            if function == PROFILES_FN_GET_CURRENT_DPI_INDEX
                || function == PROFILES_FN_SET_CURRENT_DPI_INDEX
            {
                let dpi_idx = params[0] as u32;
                let mut changed = false;

                if let Some(active_profile) = info.profiles.iter_mut().find(|p| p.is_active) {
                    for res in &mut active_profile.resolutions {
                        let should_be_active = res.index == dpi_idx;
                        if res.is_active != should_be_active {
                            res.is_active = should_be_active;
                            changed = true;
                        }
                    }
                }

                if changed {
                    debug!("HID++ 2.0: hardware DPI index change detected -> index {dpi_idx}");
                }

                return Ok(changed);
            }
        }

        Ok(false)
    }
}
