/* Logitech HID++ 1.0 driver implementation. */
/*  */
/* HID++ 1.0 is the older protocol used by devices like the G500, G700, G9. */
/* It uses register-based commands with short (7-byte) reports. */

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::device::DeviceInfo;
use crate::driver::DeviceIo;

use super::hidpp::{self, Hidpp10MatchResult, DEVICE_IDX_CORDED, DEVICE_IDX_RECEIVER};

/* HID++ 1.0 register addresses */
const REG_PROTOCOL_VERSION: u8 = 0x00;
const REG_CURRENT_PROFILE: u8 = 0x0F;

/* HID++ 1.0 sub-IDs for register access */
const SUB_ID_GET_REGISTER: u8 = 0x81;
const SUB_ID_SET_REGISTER: u8 = 0x80;
const SUB_ID_GET_LONG_REGISTER: u8 = 0x83;
const SUB_ID_SET_LONG_REGISTER: u8 = 0x82;

/* Feature Payloads */

#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp10RefreshRatePayload {
    pub rate: u8,
    pub param2: u8,
    pub param3: u8,
}

impl Hidpp10RefreshRatePayload {
    pub fn from_bytes(buf: &[u8; 3]) -> Self {
        Self {
            rate: buf[0],
            param2: buf[1],
            param3: buf[2],
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp10LedColorPayload {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Hidpp10LedColorPayload {
    pub fn from_bytes(buf: &[u8; 3]) -> Self {
        Self {
            r: buf[0],
            g: buf[1],
            b: buf[2],
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Hidpp10ResolutionLongPayload {
    pub xres: [u8; 2], // Little Endian
    pub yres: [u8; 2], // Little Endian
    pub padding: [u8; 12],
}

impl Hidpp10ResolutionLongPayload {
    pub fn from_bytes(buf: &[u8; 16]) -> Self {
        let mut xres = [0u8; 2];
        xres.copy_from_slice(&buf[0..2]);
        let mut yres = [0u8; 2];
        yres.copy_from_slice(&buf[2..4]);
        let mut padding = [0u8; 12];
        padding.copy_from_slice(&buf[4..16]);
        Self { xres, yres, padding }
    }
    pub fn xres(&self) -> u16 { u16::from_le_bytes(self.xres) }
    #[allow(dead_code)]
    pub fn yres(&self) -> u16 { u16::from_le_bytes(self.yres) }
    pub fn set_xres(&mut self, res: u16) { self.xres = res.to_le_bytes(); }
    pub fn set_yres(&mut self, res: u16) { self.yres = res.to_le_bytes(); }
}


/* Protocol version stored after a successful probe. */
#[derive(Debug, Clone, Copy, Default)]
struct ProtocolVersion {
    major: u8,
    minor: u8,
}

pub struct Hidpp10Driver {
    device_index: u8,
    version: ProtocolVersion,
}

impl Hidpp10Driver {
    pub fn new() -> Self {
        Self {
            device_index: DEVICE_IDX_RECEIVER,
            version: ProtocolVersion::default(),
        }
    }

    /* Attempt a HID++ 1.0 protocol version probe at a specific device index. */
    /* Returns `Some(params)` on success, `None` on timeout or error.         */
    async fn try_probe_index(
        &self,
        io: &mut DeviceIo,
        idx: u8,
    ) -> Option<[u8; 3]> {
        let request = hidpp::build_short_report(
            idx,
            SUB_ID_GET_REGISTER,
            REG_PROTOCOL_VERSION,
            [0x00, 0x00, 0x00],
        );

        io.request(&request, 20, 2, move |buf| {
            match hidpp::match_hidpp10_register(buf, idx, SUB_ID_GET_REGISTER, REG_PROTOCOL_VERSION) {
                Hidpp10MatchResult::Short(p) => Some(p),
                _ => None,
            }
        })
        .await
        .ok()
    }

    /* Send a short GET_REGISTER request and return the 3 response bytes. */
    async fn get_register(
        &self,
        io: &mut DeviceIo,
        register: u8,
        params: [u8; 3],
    ) -> Result<[u8; 3]> {
        let request = hidpp::build_short_report(
            self.device_index,
            SUB_ID_GET_REGISTER,
            register,
            params,
        );

        let dev_idx = self.device_index;
        io.request(&request, 20, 3, move |buf| {
            match hidpp::match_hidpp10_register(buf, dev_idx, SUB_ID_GET_REGISTER, register) {
                Hidpp10MatchResult::Short(p) => Some(p),
                _ => None,
            }
        })
        .await
        .context("HID++ 1.0 GET_REGISTER failed")
    }

    /* Send a short SET_REGISTER request and return the 3 response bytes. */
    async fn set_register(
        &self,
        io: &mut DeviceIo,
        register: u8,
        params: [u8; 3],
    ) -> Result<[u8; 3]> {
        let request = hidpp::build_short_report(
            self.device_index,
            SUB_ID_SET_REGISTER,
            register,
            params,
        );

        let dev_idx = self.device_index;
        io.request(&request, 20, 3, move |buf| {
            match hidpp::match_hidpp10_register(buf, dev_idx, SUB_ID_SET_REGISTER, register) {
                Hidpp10MatchResult::Short(p) => Some(p),
                _ => None,
            }
        })
        .await
        .context("HID++ 1.0 SET_REGISTER failed")
    }

    async fn get_long_register(
        &self,
        io: &mut DeviceIo,
        register: u8,
    ) -> Result<[u8; 16]> {
        let request = hidpp::build_long_report(
            self.device_index,
            SUB_ID_GET_LONG_REGISTER,
            register,
            [0; 16],
        );

        let dev_idx = self.device_index;
        io.request(&request, 20, 3, move |buf| {
            match hidpp::match_hidpp10_register(buf, dev_idx, SUB_ID_GET_LONG_REGISTER, register) {
                Hidpp10MatchResult::Long(p) => Some(p),
                _ => None,
            }
        })
        .await
        .context("HID++ 1.0 GET_LONG_REGISTER failed")
    }

    async fn set_long_register(
        &self,
        io: &mut DeviceIo,
        register: u8,
        payload: [u8; 16],
    ) -> Result<[u8; 16]> {
        let request = hidpp::build_long_report(
            self.device_index,
            SUB_ID_SET_LONG_REGISTER,
            register,
            payload,
        );

        let dev_idx = self.device_index;
        io.request(&request, 20, 3, move |buf| {
            match hidpp::match_hidpp10_register(buf, dev_idx, SUB_ID_SET_LONG_REGISTER, register) {
                Hidpp10MatchResult::Long(p) => Some(p),
                _ => None,
            }
        })
        .await
        .context("HID++ 1.0 SET_LONG_REGISTER failed")
    }

    async fn read_resolution(&self, io: &mut DeviceIo, profile: &mut crate::device::ProfileInfo) -> Result<()> {
        const REG_CURRENT_RESOLUTION: u8 = 0x63;
        let payload = self.get_long_register(io, REG_CURRENT_RESOLUTION).await?;
        let res_payload = Hidpp10ResolutionLongPayload::from_bytes(&payload);
        
        // Resolution is scaled by 50 in standard ratbag parsing
        if let Some(res) = profile.resolutions.first_mut() {
            let x_dpi = (res_payload.xres() as u32).saturating_mul(50);
            res.dpi = crate::device::Dpi::Unified(x_dpi);
        }
        Ok(())
    }

    async fn write_resolution(&self, io: &mut DeviceIo, profile: &crate::device::ProfileInfo) -> Result<()> {
        const REG_CURRENT_RESOLUTION: u8 = 0x63;
        if let Some(res) = profile.resolutions.iter().find(|r| r.is_active)
            && let crate::device::Dpi::Unified(val) = res.dpi
        {
            let mut req_payload = Hidpp10ResolutionLongPayload::default();
            req_payload.set_xres((val / 50) as u16);
            req_payload.set_yres((val / 50) as u16);
            
            // `into_bytes` missing dynamically so mapping directly to byte array payload abstraction safely
            let mut bytes = [0; 16];
            bytes[0..2].copy_from_slice(&req_payload.xres);
            bytes[2..4].copy_from_slice(&req_payload.yres);

            self.set_long_register(io, REG_CURRENT_RESOLUTION, bytes).await?;
            tracing::debug!("HID++ 1.0: committed DPI = {}", val);
        }
        Ok(())
    }

    async fn read_refresh_rate(&self, io: &mut DeviceIo, profile: &mut crate::device::ProfileInfo) -> Result<()> {
        const REG_USB_REFRESH_RATE: u8 = 0x64;
        let params = self.get_register(io, REG_USB_REFRESH_RATE, [0, 0, 0]).await?;
        let payload = Hidpp10RefreshRatePayload::from_bytes(&params);
        if payload.rate > 0 {
            profile.report_rate = 1000 / (payload.rate as u32);
        }
        Ok(())
    }

    async fn write_refresh_rate(&self, io: &mut DeviceIo, profile: &crate::device::ProfileInfo) -> Result<()> {
        const REG_USB_REFRESH_RATE: u8 = 0x64;
        if profile.report_rate > 0 {
            let rate = ((1000 / profile.report_rate).min(255)) as u8;
            self.set_register(io, REG_USB_REFRESH_RATE, [rate, 0, 0]).await?;
            tracing::debug!("HID++ 1.0: committed report rate = {} Hz", profile.report_rate);
        }
        Ok(())
    }

    async fn read_led_color(&self, io: &mut DeviceIo, profile: &mut crate::device::ProfileInfo) -> Result<()> {
        const REG_LED_COLOR: u8 = 0x57;
        let color_params = self.get_register(io, REG_LED_COLOR, [0, 0, 0]).await?;
        let color_payload = Hidpp10LedColorPayload::from_bytes(&color_params);
        
        for led in &mut profile.leds {
            led.color = crate::device::Color::from_rgb(crate::device::RgbColor {
                r: color_payload.r,
                g: color_payload.g,
                b: color_payload.b,
            });
            // Solid mapping for HID++ 1.0 logic baseline since status logic controls full mode.
            led.mode = crate::device::LedMode::Solid;
        }
        Ok(())
    }

    async fn write_led_color(&self, io: &mut DeviceIo, profile: &crate::device::ProfileInfo) -> Result<()> {
        const REG_LED_COLOR: u8 = 0x57;
        if let Some(first_led) = profile.leds.first() {
            let rgb = first_led.color.to_rgb();
            self.set_register(io, REG_LED_COLOR, [rgb.r, rgb.g, rgb.b]).await?; 
            tracing::debug!("HID++ 1.0: committed LED Color");
        }
        Ok(())
    }
}

#[async_trait]
impl super::DeviceDriver for Hidpp10Driver {
    fn name(&self) -> &str {
        "Logitech HID++ 1.0"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Try the wireless receiver index first, then fall back to corded. */
        const PROBE_INDICES: &[u8] = &[DEVICE_IDX_RECEIVER, DEVICE_IDX_CORDED];

        for &idx in PROBE_INDICES {
            if let Some(params) = self.try_probe_index(io, idx).await {
                self.device_index = idx;
                self.version = ProtocolVersion {
                    major: params[0],
                    minor: params[1],
                };
                info!(
                    "HID++ 1.0 device detected at index 0x{idx:02X} (protocol {}.{})",
                    self.version.major, self.version.minor
                );
                return Ok(());
            }
            debug!("HID++ 1.0 probe at index 0x{idx:02X}: no response");
        }

        anyhow::bail!(
            "HID++ 1.0 protocol version probe failed (tried indices: {:02X?})",
            PROBE_INDICES
        );
    }

    async fn load_profiles(&mut self, io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let active_idx = self
            .get_register(io, REG_CURRENT_PROFILE, [0x00, 0x00, 0x00])
            .await
            .map(|p| u32::from(p[0]))
            .unwrap_or_else(|e| {
                warn!("Failed to read current profile: {e}");
                0
            });

        for profile in &mut info.profiles {
            profile.is_active = profile.index == active_idx;
            
            if let Err(e) = self.read_resolution(io, profile).await {
                warn!("Failed to read DPI for profile {}: {}", profile.index, e);
            }
            if let Err(e) = self.read_refresh_rate(io, profile).await {
                warn!("Failed to read report rate for profile {}: {}", profile.index, e);
            }
            if let Err(e) = self.read_led_color(io, profile).await {
                warn!("Failed to read LED color for profile {}: {}", profile.index, e);
            }
        }

        debug!(
            "HID++ 1.0: loaded {} profiles, active = {active_idx}",
            info.profiles.len()
        );
        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        if let Some(profile) = info.profiles.iter().find(|p| p.is_active)
            && let Ok(idx) = u8::try_from(profile.index)
        {
            if let Err(e) = self.write_resolution(io, profile).await {
                warn!("Failed to commit DPI for profile {}: {}", profile.index, e);
            }
            if let Err(e) = self.write_refresh_rate(io, profile).await {
                warn!("Failed to commit report rate for profile {}: {}", profile.index, e);
            }
            if let Err(e) = self.write_led_color(io, profile).await {
                warn!("Failed to commit LED color for profile {}: {}", profile.index, e);
            }

            /* Write the new active profile index */
            self.set_register(io, REG_CURRENT_PROFILE, [idx, 0x00, 0x00])
                .await
                .context("Failed to commit active profile")?;
            debug!("HID++ 1.0: committed active profile = {idx}");
        }
        Ok(())
    }
}
