/* Logitech HID++ 2.0 driver implementation. */
/*  */
/* HID++ 2.0 is the modern feature-based protocol used by most current */
/* Logitech gaming mice. Each capability is exposed as a numbered "feature" */
/* that must be discovered at probe time via the Root feature (0x0000). */

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::device::{DeviceInfo, Dpi, ProfileInfo};
use crate::driver::DeviceIo;

use super::hidpp::{
    self, HidppReport, DEVICE_IDX_WIRED, PAGE_ADJUSTABLE_DPI, PAGE_ADJUSTABLE_REPORT_RATE,
    PAGE_COLOR_LED_EFFECTS, PAGE_DEVICE_NAME, PAGE_ONBOARD_PROFILES, PAGE_RGB_EFFECTS,
    PAGE_SPECIAL_KEYS_BUTTONS, ROOT_FEATURE_INDEX, ROOT_FN_GET_FEATURE,
    ROOT_FN_GET_PROTOCOL_VERSION,
};

/* Software ID used in all our requests (arbitrary, identifies us) */
const SW_ID: u8 = 0x04;

/* Adjustable DPI (0x2201) function IDs */
const DPI_FN_GET_SENSOR_COUNT: u8 = 0x00;
const DPI_FN_GET_SENSOR_DPI: u8 = 0x01;

/* Adjustable Report Rate (0x8060) function IDs */
const RATE_FN_GET_REPORT_RATE_LIST: u8 = 0x00;
const RATE_FN_GET_REPORT_RATE: u8 = 0x01;

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
}

impl Hidpp20Driver {
    pub fn new() -> Self {
        Self {
            device_index: DEVICE_IDX_WIRED,
            version: ProtocolVersion::default(),
            features: FeatureMap::default(),
        }
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
        io.request(&request, 3, move |buf| {
            let report = HidppReport::parse(buf)?;
            if report.is_error() {
                return Some(None);
            }
            if !report.matches_hidpp20(dev_idx, ROOT_FEATURE_INDEX) {
                return None;
            }
            if let HidppReport::Long { params, .. } = report {
                let index = params[0];
                Some(if index == 0 { None } else { Some(index) })
            } else {
                None
            }
        })
        .await
        .with_context(|| format!("Feature lookup for 0x{feature_page:04X} failed"))
    }

    /* Send a HID++ 2.0 feature request and return the 16-byte response payload. */
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
        io.request(&request, 3, move |buf| {
            let report = HidppReport::parse(buf)?;
            if report.matches_hidpp20(dev_idx, feature_index)
                && let HidppReport::Long { params, .. } = report
            {
                return Some(params);
            }
            None
        })
        .await
        .with_context(|| {
            format!("Feature request (idx=0x{feature_index:02X}, fn={function}) failed")
        })
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

        let dpi_data = self
            .feature_request(io, idx, DPI_FN_GET_SENSOR_DPI, &[0])
            .await?;
        let current_dpi = u16::from_be_bytes([dpi_data[1], dpi_data[2]]);
        let default_dpi = u16::from_be_bytes([dpi_data[3], dpi_data[4]]);

        if let Some(res) = profile.resolutions.first_mut() {
            res.dpi = Dpi::Unified(u32::from(current_dpi));
        }

        debug!("HID++ 2.0: sensor 0 DPI = {current_dpi} (default = {default_dpi})");
        Ok(())
    }

    /* Read report rate using feature 0x8060. */
    async fn read_report_rate(
        &self,
        io: &mut DeviceIo,
        profile: &mut ProfileInfo,
    ) -> Result<()> {
        let Some(idx) = self.features.report_rate else {
            return Ok(());
        };

        let list_data = self
            .feature_request(io, idx, RATE_FN_GET_REPORT_RATE_LIST, &[])
            .await?;
        let rate_bitmap = list_data[0];

        profile.report_rates = (0..8u32)
            .filter(|bit| rate_bitmap & (1 << bit) != 0)
            .map(|bit| 1000 / (bit + 1))
            .collect();

        let rate_data = self
            .feature_request(io, idx, RATE_FN_GET_REPORT_RATE, &[])
            .await?;
        let current_rate_ms = u32::from(rate_data[0]);
        if current_rate_ms > 0 {
            profile.report_rate = 1000 / current_rate_ms;
        }
        Ok(())
    }

    /* Write DPI sensor information using feature 0x2201. */
    async fn write_dpi_info(
        &self,
        io: &mut DeviceIo,
        profile: &ProfileInfo,
    ) -> Result<()> {
        const DPI_FN_SET_SENSOR_DPI: u8 = 0x02;

        let Some(idx) = self.features.adjustable_dpi else {
            return Ok(());
        };

        if let Some(res) = profile.resolutions.iter().find(|r| r.is_active)
            && let Dpi::Unified(dpi_val) = res.dpi
        {
            let bytes = (dpi_val as u16).to_be_bytes();
            /* Param layout: sensor (1 byte), DPI uint16 (2 bytes) */
            self.feature_request(io, idx, DPI_FN_SET_SENSOR_DPI, &[0, bytes[0], bytes[1]])
                .await
                .context("Failed to write DPI")?;
            debug!("HID++ 2.0: committed DPI = {}", dpi_val);
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
        let request = hidpp::build_hidpp20_request(
            self.device_index,
            ROOT_FEATURE_INDEX,
            ROOT_FN_GET_PROTOCOL_VERSION,
            SW_ID,
            &[],
        );

        let dev_idx = self.device_index;
        let (major, minor) = io
            .request(&request, 3, move |buf| {
                let report = HidppReport::parse(buf)?;
                if report.is_error() {
                    return None;
                }
                if !report.matches_hidpp20(dev_idx, ROOT_FEATURE_INDEX) {
                    return None;
                }
                if let HidppReport::Long { params, .. } = report {
                    Some((params[0], params[1]))
                } else {
                    None
                }
            })
            .await
            .context("HID++ 2.0 protocol version probe failed")?;

        self.version = ProtocolVersion { major, minor };
        info!("HID++ 2.0 device detected (protocol {major}.{minor})");

        self.discover_features(io).await?;
        Ok(())
    }

    async fn load_profiles(
        &mut self,
        io: &mut DeviceIo,
        info: &mut DeviceInfo,
    ) -> Result<()> {
        for profile in &mut info.profiles {
            if let Err(e) = self.read_dpi_info(io, profile).await {
                warn!("Failed to read DPI for profile {}: {e}", profile.index);
            }
            if let Err(e) = self.read_report_rate(io, profile).await {
                warn!("Failed to read report rate for profile {}: {e}", profile.index);
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
        }
        Ok(())
    }
}
