/* Logitech HID++ 2.0 driver implementation. */
/*  */
/* HID++ 2.0 is the modern feature-based protocol used by most current */
/* Logitech gaming mice. Each capability is exposed as a numbered "feature" */
/* that must be discovered at probe time via the Root feature (0x0000). */

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::time::{sleep, Duration};
use tracing::{debug, info, warn};

use crate::device::{Color, DeviceInfo, Dpi, LedMode, ProfileInfo, RgbColor};
use crate::driver::DeviceIo;

use super::hidpp::{
     self, HidppReport, DEVICE_IDX_CORDED, DEVICE_IDX_RECEIVER,
    LED_HW_MODE_BREATHING, LED_HW_MODE_COLOR_WAVE,
    LED_HW_MODE_CYCLE, LED_HW_MODE_FIXED, LED_HW_MODE_OFF, LED_HW_MODE_STARLIGHT,
    PAGE_ADJUSTABLE_DPI, PAGE_ADJUSTABLE_REPORT_RATE,
    PAGE_COLOR_LED_EFFECTS, PAGE_DEVICE_NAME, PAGE_ONBOARD_PROFILES, PAGE_RGB_EFFECTS,
    PAGE_SPECIAL_KEYS_BUTTONS, ROOT_FEATURE_INDEX, ROOT_FN_GET_FEATURE,
    ROOT_FN_GET_PROTOCOL_VERSION,
};

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

/* Color LED Effects (0x8070) function IDs */
const LED_FN_GET_ZONE_EFFECT: u8 = 0x01;
const LED_FN_SET_ZONE_EFFECT: u8 = 0x02;

/* Onboard Profiles (0x8100) function IDs */
const PROFILES_FN_GET_PROFILES_DESCR: u8 = 0x00;
const PROFILES_FN_SET_MODE: u8 = 0x01;
const PROFILES_FN_MEMORY_READ: u8 = 0x04;
const PROFILES_FN_MEMORY_ADDR_WRITE: u8 = 0x05;
const PROFILES_FN_MEMORY_WRITE: u8 = 0x06;
const PROFILES_FN_MEMORY_WRITE_END: u8 = 0x07;

/* Onboard profile mode values for PROFILES_FN_SET_MODE */
const ONBOARD_MODE_ONBOARD: u8 = 0x01;
const ONBOARD_MODE_HOST: u8 = 0x02;

/* A feature page → runtime index mapping for a known set of capabilities. */
#[derive(Debug, Default)]
struct FeatureMap {
    adjustable_dpi: Option<u8>,
    special_keys: Option<u8>,
    onboard_profiles: Option<u8>,
    color_led_effects: Option<u8>,
    rgb_effects: Option<u8>,
    report_rate: Option<u8>,
    device_name: Option<u8>,
}

impl FeatureMap {
    /* Store a discovered feature index based on its page ID. */
    fn insert(&mut self, page: u16, index: u8) {
        match page {
            PAGE_ADJUSTABLE_DPI => self.adjustable_dpi = Some(index),
            PAGE_SPECIAL_KEYS_BUTTONS => self.special_keys = Some(index),
            PAGE_ONBOARD_PROFILES => self.onboard_profiles = Some(index),
            PAGE_COLOR_LED_EFFECTS => self.color_led_effects = Some(index),
            PAGE_RGB_EFFECTS => self.rgb_effects = Some(index),
            PAGE_ADJUSTABLE_REPORT_RATE => self.report_rate = Some(index),
            PAGE_DEVICE_NAME => self.device_name = Some(index),
            _ => {}
        }
    }
}

/* Feature 0x2201 (Adjustable DPI): Payload for Get/Set Sensor DPI */
#[derive(Debug, Clone, Copy)]
pub struct Hidpp20DpiPayload {
    pub sensor_index: u8,
    pub current_dpi: [u8; 2], // Big Endian u16
    pub default_dpi: [u8; 2], // Big Endian u16
    pub padding: [u8; 11],
}

impl Hidpp20DpiPayload {
    pub fn from_bytes(buf: &[u8; 16]) -> Self {
        let sensor_index = buf[0];
        let mut current_dpi = [0u8; 2];
        current_dpi.copy_from_slice(&buf[1..3]);
        let mut default_dpi = [0u8; 2];
        default_dpi.copy_from_slice(&buf[3..5]);
        let mut padding = [0u8; 11];
        padding.copy_from_slice(&buf[5..16]);
        Self { sensor_index, current_dpi, default_dpi, padding }
    }
    pub fn into_bytes(self) -> [u8; 16] {
        let mut buf = [0u8; 16];
        buf[0] = self.sensor_index;
        buf[1..3].copy_from_slice(&self.current_dpi);
        buf[3..5].copy_from_slice(&self.default_dpi);
        buf[5..16].copy_from_slice(&self.padding);
        buf
    }
    pub fn current_dpi(&self) -> u16 {
        u16::from_be_bytes(self.current_dpi)
    }
    pub fn default_dpi(&self) -> u16 {
        u16::from_be_bytes(self.default_dpi)
    }
    pub fn set_current_dpi(&mut self, dpi: u16) {
        self.current_dpi = dpi.to_be_bytes();
    }
}

/* Feature 0x8060 (Adjustable Report Rate) */
#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp20ReportRatePayload {
    pub data: u8, // Used for rate_bitmap or rate_ms
    pub padding: [u8; 15],
}

impl Hidpp20ReportRatePayload {
    pub fn from_bytes(buf: &[u8; 16]) -> Self {
        let data = buf[0];
        let mut padding = [0u8; 15];
        padding.copy_from_slice(&buf[1..16]);
        Self { data, padding }
    }
}

/* Feature 0x8070 & 0x8071 (Color LED / RGB) */
#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp20LedGetZonePayload {
    pub zone_index: u8,
    pub payload: [u8; crate::driver::hidpp::LED_PAYLOAD_SIZE], // 11 bytes
    pub padding: [u8; 4],
}

impl Hidpp20LedGetZonePayload {
    pub fn from_bytes(buf: &[u8; 16]) -> Self {
        let zone_index = buf[0];
        let mut payload = [0u8; crate::driver::hidpp::LED_PAYLOAD_SIZE];
        payload.copy_from_slice(&buf[1..1+crate::driver::hidpp::LED_PAYLOAD_SIZE]);
        let mut padding = [0u8; 4];
        padding.copy_from_slice(&buf[1+crate::driver::hidpp::LED_PAYLOAD_SIZE..16]);
        Self { zone_index, payload, padding }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp20LedSetZonePayload {
    pub zone_index: u8,
    pub payload: [u8; crate::driver::hidpp::LED_PAYLOAD_SIZE],
    pub persist: u8,
    pub padding: [u8; 3],
}

impl Hidpp20LedSetZonePayload {
    pub fn into_bytes(self) -> [u8; 16] {
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
pub struct Hidpp20ButtonBinding {
    pub button_type: u8,
    pub subtype: u8,
    pub control_id_or_macro_id: [u8; 2], // little endian
}

impl Hidpp20ButtonBinding {
    pub fn from_bytes(buf: &[u8; 4]) -> Self {
        let button_type = buf[0];
        let subtype = buf[1];
        let mut control_id_or_macro_id = [0u8; 2];
        control_id_or_macro_id.copy_from_slice(&buf[2..4]);
        Self { button_type, subtype, control_id_or_macro_id }
    }
    
    pub fn into_bytes(self) -> [u8; 4] {
        let mut buf = [0u8; 4];
        buf[0] = self.button_type;
        buf[1] = self.subtype;
        buf[2..4].copy_from_slice(&self.control_id_or_macro_id);
        buf
    }

    pub fn to_action(self) -> crate::device::ActionType {
        match self.button_type {
            crate::driver::hidpp::BUTTON_TYPE_MACRO => crate::device::ActionType::Macro,
            crate::driver::hidpp::BUTTON_TYPE_HID => {
                match self.subtype {
                    crate::driver::hidpp::BUTTON_SUBTYPE_MOUSE => crate::device::ActionType::Button,
                    crate::driver::hidpp::BUTTON_SUBTYPE_KEYBOARD => crate::device::ActionType::Key,
                    crate::driver::hidpp::BUTTON_SUBTYPE_CONSUMER => crate::device::ActionType::Special,
                    _ => crate::device::ActionType::Unknown,
                }
            }
            crate::driver::hidpp::BUTTON_TYPE_SPECIAL => crate::device::ActionType::Special,
            crate::driver::hidpp::BUTTON_TYPE_DISABLED => crate::device::ActionType::None,
            _ => crate::device::ActionType::Unknown,
        }
    }

    pub fn from_action(action: crate::device::ActionType, mapping_value: u32) -> Self {
        let mut button_type = crate::driver::hidpp::BUTTON_TYPE_DISABLED;
        let mut subtype = 0;
        let mut control_id = 0u16;

        match action {
            crate::device::ActionType::Macro => {
                button_type = crate::driver::hidpp::BUTTON_TYPE_MACRO;
                control_id = mapping_value as u16;
            }
            crate::device::ActionType::Button => {
                button_type = crate::driver::hidpp::BUTTON_TYPE_HID;
                subtype = crate::driver::hidpp::BUTTON_SUBTYPE_MOUSE;
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
            crate::device::ActionType::Key => {
                button_type = crate::driver::hidpp::BUTTON_TYPE_HID;
                subtype = crate::driver::hidpp::BUTTON_SUBTYPE_KEYBOARD;
                control_id = mapping_value as u16;
            }
            crate::device::ActionType::Special => {
                button_type = crate::driver::hidpp::BUTTON_TYPE_SPECIAL;
                control_id = mapping_value as u16;
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

/* Feature 0x8100: Onboard Profiles */
#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp20OnboardProfilesInfo {
    pub memory_model: u8,
    pub profile_format_id: u8,
    pub macro_format_id: u8,
    pub profile_count: u8,
    pub profile_count_oob: u8,
    pub button_count: u8,
    pub sector_count: u8,
    pub sector_size: [u8; 2],  // Big Endian u16
    pub mechanical_layout: u8,
    pub reserved: [u8; 6],
}

impl Hidpp20OnboardProfilesInfo {
    pub fn from_bytes(buf: &[u8; 16]) -> Self {
        let memory_model = buf[0];
        let profile_format_id = buf[1];
        let macro_format_id = buf[2];
        let profile_count = buf[3];
        let profile_count_oob = buf[4];
        let button_count = buf[5];
        let sector_count = buf[6];
        let mut sector_size = [0u8; 2];
        sector_size.copy_from_slice(&buf[7..9]);
        let mechanical_layout = buf[9];
        let mut reserved = [0u8; 6];
        reserved.copy_from_slice(&buf[10..16]);
        Self { memory_model, profile_format_id, macro_format_id, profile_count, profile_count_oob, button_count, sector_count, sector_size, mechanical_layout, reserved }
    }
    pub fn sector_size(&self) -> u16 {
        u16::from_be_bytes(self.sector_size)
    }
}



/* Protocol version stored after a successful probe. */
#[derive(Debug, Clone, Copy, Default)]
struct ProtocolVersion {
    #[allow(dead_code)]
    major: u8,
    #[allow(dead_code)]
    minor: u8,
}

pub struct Hidpp20Driver {
    device_index: u8,
    version: ProtocolVersion,
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
            version: ProtocolVersion::default(),
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
            let report = HidppReport::parse(buf)?;

            /* An error from the Root feature means the page is not supported. */
            if report.hidpp20_error_code(dev_idx, ROOT_FEATURE_INDEX).is_some()
                || report.is_error()
            {
                return Some(None);
            }

            /* Accept both Long and Short responses for the Root feature. */
            match &report {
                HidppReport::Long {
                    device_index,
                    sub_id,
                    params,
                    ..
                } if *device_index == dev_idx && *sub_id == ROOT_FEATURE_INDEX => {
                    let index = params[0];
                    Some(if index == 0 { None } else { Some(index) })
                }
                HidppReport::Short {
                    device_index,
                    sub_id,
                    params,
                    ..
                } if *device_index == dev_idx && *sub_id == ROOT_FEATURE_INDEX => {
                    let index = params[0];
                    Some(if index == 0 { None } else { Some(index) })
                }
                _ => None,
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

        /* Response is either Ok(params) or Err(error_code). */
        enum Resp {
            Ok([u8; 16]),
            HidppErr(u8),
        }

        let dev_idx = self.device_index;
        let resp = io
            .request(&request, 20, 3, move |buf| {
                let report = HidppReport::parse(buf)?;

                /* 1. Check for HID++ error (Long 0xFF or Short 0x8F). */
                if let Some(code) =
                    report.hidpp20_error_code(dev_idx, feature_index)
                {
                    return Some(Resp::HidppErr(code));
                }

                /* 2. Successful Long response. */
                if let HidppReport::Long {
                    device_index,
                    sub_id,
                    params,
                    ..
                } = &report
                {
                    if *device_index == dev_idx && *sub_id == feature_index {
                        return Some(Resp::Ok(*params));
                    }
                }

                /* 3. Successful Short response (SET acknowledgment). */
                if let HidppReport::Short {
                    device_index,
                    sub_id,
                    params,
                    ..
                } = &report
                {
                    if *device_index == dev_idx && *sub_id == feature_index {
                        let mut long_params = [0u8; 16];
                        long_params[..3].copy_from_slice(params);
                        return Some(Resp::Ok(long_params));
                    }
                }

                None
            })
            .await
            .with_context(|| {
                format!(
                    "Feature request (idx=0x{feature_index:02X}, fn={function}) failed"
                )
            })?;

        match resp {
            Resp::Ok(p) => Ok(p),
            Resp::HidppErr(code) => {
                let name = hidpp::hidpp20_error_name(code);
                Err(anyhow::anyhow!(
                    "HID++ error {name} (0x{code:02X}) for feature 0x{feature_index:02X} fn={function}"
                ))
            }
        }
    }

    /* Discover all supported features and cache their runtime indices. */
    async fn discover_features(&mut self, io: &mut DeviceIo) -> Result<()> {
        const FEATURE_QUERIES: &[(u16, &str)] = &[
            (PAGE_ADJUSTABLE_DPI, "Adjustable DPI"),
            (PAGE_SPECIAL_KEYS_BUTTONS, "Special Keys/Buttons"),
            (PAGE_ONBOARD_PROFILES, "Onboard Profiles"),
            (PAGE_COLOR_LED_EFFECTS, "Color LED Effects"),
            (PAGE_RGB_EFFECTS, "RGB Effects"),
            (PAGE_ADJUSTABLE_REPORT_RATE, "Adjustable Report Rate"),
            (PAGE_DEVICE_NAME, "Device Name"),
        ];

        for &(page, name) in FEATURE_QUERIES {
            match self.get_feature_index(io, page).await {
                Ok(Some(idx)) => {
                    debug!("  Feature {name} (0x{page:04X}) at index 0x{idx:02X}");
                    self.features.insert(page, idx);
                }
                Ok(None) => {
                    debug!("  Feature {name} (0x{page:04X}) not supported");
                }
                Err(e) => {
                    warn!("  Feature {name} (0x{page:04X}) query failed: {e}");
                }
            }
        }

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

            debug!(
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

        // Step 1: Write Start command
        let mut start_bytes = [0u8; 16];
        start_bytes[0..2].copy_from_slice(&sector_index.to_be_bytes());
        start_bytes[2..4].copy_from_slice(&write_offset.to_be_bytes()); // usually 0 for a full sector
        start_bytes[4..6].copy_from_slice(&size.to_be_bytes());

        // 1. Initiate Write Sequence
        self.feature_request(io, idx, PROFILES_FN_MEMORY_ADDR_WRITE, &start_bytes)
            .await
            .context("Failed to start sector write")?;

        // 2. Iterate and Write Data Chunks (16 bytes at a time)
        for chunk in data.chunks(16) {
            let mut payload = [0u8; 16];
            payload[..chunk.len()].copy_from_slice(chunk);
            self.feature_request(io, idx, PROFILES_FN_MEMORY_WRITE, &payload)
                .await
                .context("Failed to write sector chunk")?;
        }

        // 3. Finalize Write (using short report behavior emulation with 0 params)
        self.feature_request(io, idx, PROFILES_FN_MEMORY_WRITE_END, &[0; 16])
            .await
            .context("Failed to end sector write")?;

        Ok(())
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

        let sensor_info = self
            .feature_request(io, idx, DPI_FN_GET_SENSOR_COUNT, &[0])
            .await?;
        if sensor_info[0] == 0 {
            return Ok(());
        }

        /* Query the supported DPI list / range (fn=1, getSensorDPIList).
         * The response is 16 bytes starting with [sensorIndex, ...].
         * If the first DPI entry has 0xE0 in its high nibble, it is a
         * A value >= 0xE000 is a range-step marker: step = value & 0x1FFF.
         * The preceding entry is the range minimum and the next entry is
         * the range maximum.  Otherwise the entry is a discrete DPI value.
         * Values are big-endian u16; the list ends at the first 0x0000. */
        let list_data = self
            .feature_request(io, idx, DPI_FN_GET_SENSOR_DPI_LIST, &[0])
            .await?;

        let mut dpi_list: Vec<u32> = Vec::new();
        let list_bytes = &list_data[1..]; /* skip sensor_index byte */

        /* Parse BE u16 entries, stopping at the first zero. */
        let mut entries: Vec<u16> = Vec::new();
        for chunk in list_bytes.chunks_exact(2) {
            let val = u16::from_be_bytes([chunk[0], chunk[1]]);
            if val == 0 {
                break;
            }
            entries.push(val);
        }

        let mut i = 0;
        while i < entries.len() {
            let val = entries[i];
            if val >= 0xE000 {
                /* Range-step marker.  Previous discrete entry is the minimum,
                 * next entry is the maximum. */
                let step = u32::from(val & 0x1FFF);
                let dpi_min = dpi_list.pop().unwrap_or(200);
                let dpi_max = if i + 1 < entries.len() {
                    u32::from(entries[i + 1])
                } else {
                    dpi_min
                };
                if step > 0 && dpi_max >= dpi_min {
                    let mut v = dpi_min;
                    while v <= dpi_max {
                        dpi_list.push(v);
                        v = v.saturating_add(step as u32);
                    }
                }
                debug!(
                    "HID++ 2.0: sensor 0 DPI range {dpi_min}–{dpi_max} step {step} ({} values)",
                    dpi_list.len()
                );
                i += 2; /* skip the step marker and the max entry */
            } else {
                dpi_list.push(u32::from(val));
                i += 1;
            }
        }

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
        let payload = Hidpp20ReportRatePayload::from_bytes(&list_data);
        let rate_bitmap = payload.data;

        profile.report_rates = (0..8u32)
            .filter(|bit| rate_bitmap & (1 << bit) != 0)
            .map(|bit| 1000 / (bit + 1))
            .collect();

        let rate_data = self
            .feature_request(io, idx, RATE_FN_GET_REPORT_RATE, &[])
            .await?;
        let current_rate_payload = Hidpp20ReportRatePayload::from_bytes(&rate_data);
        let current_rate_ms = u32::from(current_rate_payload.data);
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

            let payload = &parsed.payload;
            let mode_byte = payload[0];

            match mode_byte {
                LED_HW_MODE_OFF => {
                    led.mode = LedMode::Off;
                }
                LED_HW_MODE_FIXED => {
                    led.mode = LedMode::Solid;
                    led.color = Color::from_rgb(RgbColor {
                        r: payload[1],
                        g: payload[2],
                        b: payload[3],
                    });
                }
                LED_HW_MODE_CYCLE => {
                    led.mode = LedMode::Cycle;
                    led.effect_duration =
                        u32::from(u16::from_be_bytes([payload[6], payload[7]]));
                    led.brightness = u32::from(payload[8]) * 255 / 100;
                }
                LED_HW_MODE_COLOR_WAVE => {
                    led.mode = LedMode::ColorWave;
                    led.effect_duration =
                        u32::from(u16::from_be_bytes([payload[6], payload[7]]));
                    led.brightness = u32::from(payload[8]) * 255 / 100;
                }
                LED_HW_MODE_STARLIGHT => {
                    led.mode = LedMode::Starlight;
                    led.color = Color::from_rgb(RgbColor {
                        r: payload[1],
                        g: payload[2],
                        b: payload[3],
                    });
                    led.secondary_color = Color::from_rgb(RgbColor {
                        r: payload[4],
                        g: payload[5],
                        b: payload[6],
                    });
                }
                LED_HW_MODE_BREATHING => {
                    led.mode = LedMode::Breathing;
                    led.color = Color::from_rgb(RgbColor {
                        r: payload[1],
                        g: payload[2],
                        b: payload[3],
                    });
                    led.effect_duration =
                        u32::from(u16::from_be_bytes([payload[4], payload[5]]));
                    led.brightness = u32::from(payload[7]) * 255 / 100;
                }
                _ => {
                    debug!("LED zone {zone_index}: unknown mode 0x{mode_byte:02X}");
                }
            }

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

            if led.mode == LedMode::TriColor {
                /* TriColor uses 0x8071 RGB Effects with the multi-LED cluster pattern command. */
                let Some(idx) = self.features.rgb_effects else {
                    warn!("TriColor requested but device lacks RGB Effects (0x8071)");
                    continue;
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
                /* Function 0x02 = setMultiLEDRGBClusterPattern on 0x8071. Note: C passes 13 bytes */
                self.feature_request(io, idx, 0x02, &bytes[0..13])
                    .await
                    .context("Failed to write TriColor multi-LED cluster pattern")?;
            } else {
                let Some(idx) = self.features.color_led_effects else {
                    warn!("Device lacks Color LED Effects (0x8070)");
                    continue;
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
                self.feature_request(io, idx, LED_FN_SET_ZONE_EFFECT, &bytes[0..13])
                    .await
                    .context("Failed to write LED zone effect")?;
            }

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
                self.version = ProtocolVersion { major, minor };
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
        /* If the device has PAGE_ONBOARD_PROFILES (0x8100), we initialize based on hardware capacity */
        if let Some(idx) = self.features.onboard_profiles {
            let desc_data = self
                .feature_request(io, idx, PROFILES_FN_GET_PROFILES_DESCR, &[])
                .await
                .context("Failed to get Onboard Profiles Description")?;

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

            // Resize the Ratbag device abstraction to exactly match the hardware capabilities
            info.profiles.resize_with(profile_count, ProfileInfo::default);
            for (i, p) in info.profiles.iter_mut().enumerate() {
                p.index = i as u32;
                p.buttons.resize_with(button_count, crate::device::ButtonInfo::default);
                for (b_idx, b) in p.buttons.iter_mut().enumerate() {
                    b.index = b_idx as u32;
                }
            }

            let sector_size = desc.sector_size();
            let root_sector_data = self.read_sector(io, idx, 0x0000, 0, sector_size).await?;
            let root_crc_ok = Self::verify_sector_crc(0x0000, &root_sector_data);
            if !root_crc_ok {
                self.needs_eeprom_repair = true;
            }

            /* Build per-profile address/enabled metadata.
             * Default to legacy C addressing (sector = profile_index + 1), then
             * override from dictionary entries only when they look valid. */
            let mut profile_addrs: Vec<u16> =
                (0..profile_count).map(|i| (i as u16) + 1).collect();
            let mut profile_enabled: Vec<bool> = vec![true; profile_count];

            if root_crc_ok {
                for i in 0..profile_count {
                    let offset = i * 4;
                    if offset + 4 > root_sector_data.len() {
                        break;
                    }

                    let addr = u16::from_be_bytes([
                        root_sector_data[offset],
                        root_sector_data[offset + 1],
                    ]);
                    if addr == 0xFFFF {
                        break;
                    }
                    if addr != 0 {
                        profile_addrs[i] = addr;
                    }
                    profile_enabled[i] = root_sector_data[offset + 2] != 0;
                }
            } else {
                warn!(
                    "HID++ 2.0: profile dictionary CRC invalid; using default sector mapping (1..N)"
                );
            }

            for i in 0..profile_count {
                let addr = profile_addrs[i];
                let enabled = profile_enabled[i];

                if addr == 0xFFFF || addr == 0 {
                    continue;
                }

                /* Read the profile payload from EEPROM. */
                if let Ok(profile_data) = self.read_sector(io, idx, addr, 0, sector_size).await {
                    /* Validate profile sector CRC (non-fatal: log and use data as-is,
                     * mirroring legacy C driver which also continued on mismatch). */
                    let crc_ok = Self::verify_sector_crc(addr, &profile_data);
                    if !crc_ok {
                        self.needs_eeprom_repair = true;
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
                    let mut eeprom_dpis: Vec<u32> = Vec::new();
                    for d_idx in 0..5usize {
                        let d_off = 3 + d_idx * 2;
                        if d_off + 2 <= profile_data.len() {
                            let raw = u16::from_le_bytes([profile_data[d_off], profile_data[d_off + 1]]);
                            if raw > 0 && raw < 0xFFFF {
                                eeprom_dpis.push(u32::from(raw));
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
                        for (r_idx, &dpi_val) in eeprom_dpis.iter().enumerate() {
                            p.resolutions.push(crate::device::ResolutionInfo {
                                index: r_idx as u32,
                                dpi: crate::device::Dpi::Unified(dpi_val),
                                dpi_list: Vec::new(), /* filled later by read_dpi_info */
                                capabilities: Vec::new(),
                                is_active: r_idx == default_dpi_idx,
                                is_default: r_idx == default_dpi_idx,
                                is_disabled: false,
                            });
                        }
                    }

                    /* --- Buttons (offset 32, 4 bytes each) --- */
                    let max_buttons = button_count.min(16);
                    for b_idx in 0..max_buttons {
                        let btn_offset = 32 + (b_idx * 4);
                        if btn_offset + 4 <= profile_data.len() {
                            let mut binding_bytes = [0u8; 4];
                            binding_bytes.copy_from_slice(&profile_data[btn_offset..btn_offset + 4]);
                            let binding = Hidpp20ButtonBinding::from_bytes(&binding_bytes);

                            p.buttons[b_idx].action_type = binding.to_action();

                            /* EEPROM mouse buttons are stored as a big-endian bit mask
                             * (matching the C hidpp20_buttons_to_cpu / buttons_from_cpu).
                             * ffs(mask) gives the 1-based button ordinal. */
                            let mapping_value = if binding.button_type == crate::driver::hidpp::BUTTON_TYPE_HID
                                && binding.subtype == crate::driver::hidpp::BUTTON_SUBTYPE_MOUSE
                            {
                                let mask = u16::from_be_bytes(binding.control_id_or_macro_id);
                                if mask > 0 {
                                    u32::from(mask.trailing_zeros()) + 1
                                } else {
                                    0
                                }
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
                }
            }
        } else {
            // Just assume 1 profile for now if not overridden
            if info.profiles.is_empty() {
                info.profiles.push(ProfileInfo::default());
            }
        }

        if let Some(first) = info.profiles.first_mut() {
            first.is_active = true;
        } else {
            warn!("HID++ 2.0: no profiles available after load");
        }

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

        debug!("HID++ 2.0: loaded {} profiles", info.profiles.len());
        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
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

        // Onboard Profiles (0x8100) EEPROM commit logic
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
                     * buffer matching C's memset approach. */
                    let mut profile_data = self
                        .read_sector(io, idx, addr, 0, sector_size)
                        .await
                        .unwrap_or_else(|_| vec![0xFFu8; sector_size as usize]);
                    if profile_data.len() < sector_size as usize {
                        profile_data.resize(sector_size as usize, 0xFF);
                    }

                    Self::verify_sector_crc(addr, &profile_data);

                    /* 1. Report rate (byte 0): stored as ms-interval */
                    if profile.report_rate > 0 {
                        profile_data[0] = (1000 / profile.report_rate) as u8;
                    }

                    /* 2. Default-DPI index (byte 1) */
                    if let Some(def_idx) = profile.resolutions.iter().position(|r| r.is_default) {
                        profile_data[1] = def_idx as u8;
                    }

                    /* 3. DPI list (bytes 3-12, 5 × LE u16) */
                    for (i, res) in profile.resolutions.iter().enumerate().take(5) {
                        if let Dpi::Unified(val) = res.dpi {
                            let dpi_bytes = (val as u16).to_le_bytes();
                            profile_data[3 + i * 2] = dpi_bytes[0];
                            profile_data[3 + i * 2 + 1] = dpi_bytes[1];
                        }
                    }

                    /* 4. Buttons (offset 32, 4 bytes each) */
                    let max_buttons = desc.button_count.min(16) as usize;
                    for btn in &profile.buttons {
                        let b_idx = btn.index as usize;
                        if b_idx < max_buttons {
                            let btn_offset = 32 + b_idx * 4;
                            if btn_offset + 4 <= profile_data.len() {
                                let binding = Hidpp20ButtonBinding::from_action(
                                    btn.action_type,
                                    btn.mapping_value,
                                );
                                profile_data[btn_offset..btn_offset + 4]
                                    .copy_from_slice(&binding.into_bytes());
                            }
                        }
                    }

                    /* 5. Recompute CRC (last 2 bytes, BE) */
                    let crc_offset = profile_data.len() - 2;
                    let crc = hidpp::compute_ccitt_crc(&profile_data[..crc_offset]);
                    let crc_bytes = crc.to_be_bytes();
                    profile_data[crc_offset] = crc_bytes[0];
                    profile_data[crc_offset + 1] = crc_bytes[1];

                    /* 6. Write sector */
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
            }
        }

        Ok(())
    }
}
