/* Logitech HID++ 1.0 driver implementation. */
/*  */
/* HID++ 1.0 is the older protocol used by devices like the G500, G700, G9. */
/* It uses register-based commands with short (7-byte) reports. */

use anyhow::{Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::device::DeviceInfo;
use crate::driver::DeviceIo;

use super::hidpp::{self, HidppReport, DEVICE_IDX_WIRED};

/* HID++ 1.0 register addresses */
const REG_PROTOCOL_VERSION: u8 = 0x00;
const REG_CURRENT_PROFILE: u8 = 0x0F;

/* HID++ 1.0 sub-IDs for register access */
const SUB_ID_GET_REGISTER: u8 = 0x81;

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
            device_index: DEVICE_IDX_WIRED,
            version: ProtocolVersion::default(),
        }
    }

    /* Send a short GET_REGISTER request and return the 3 response bytes. */
    async fn get_register(
        &self,
        io: &mut DeviceIo,
        register: u8,
        params: [u8; 2],
    ) -> Result<[u8; 3]> {
        let request = hidpp::build_short_report(
            self.device_index,
            SUB_ID_GET_REGISTER,
            [register, params[0], params[1]],
        );

        let dev_idx = self.device_index;
        io.request(&request, 3, move |buf| {
            let report = HidppReport::parse(buf)?;
            if report.is_error() {
                return None;
            }
            match report {
                HidppReport::Short {
                    device_index,
                    sub_id,
                    params,
                } if device_index == dev_idx && sub_id == SUB_ID_GET_REGISTER => Some(params),
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
        params: [u8; 2],
    ) -> Result<[u8; 3]> {
        const SUB_ID_SET_REGISTER: u8 = 0x80;
        let request = hidpp::build_short_report(
            self.device_index,
            SUB_ID_SET_REGISTER,
            [register, params[0], params[1]],
        );

        let dev_idx = self.device_index;
        io.request(&request, 3, move |buf| {
            let report = HidppReport::parse(buf)?;
            if report.is_error() {
                return None;
            }
            match report {
                HidppReport::Short {
                    device_index,
                    sub_id,
                    params,
                } if device_index == dev_idx && sub_id == SUB_ID_SET_REGISTER => Some(params),
                _ => None,
            }
        })
        .await
        .context("HID++ 1.0 SET_REGISTER failed")
    }
}

#[async_trait]
impl super::DeviceDriver for Hidpp10Driver {
    fn name(&self) -> &str {
        "Logitech HID++ 1.0"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        let params = self
            .get_register(io, REG_PROTOCOL_VERSION, [0x00, 0x00])
            .await
            .context("Protocol version query failed")?;

        self.version = ProtocolVersion {
            major: params[0],
            minor: params[1],
        };

        info!(
            "HID++ 1.0 device detected (protocol {}.{})",
            self.version.major, self.version.minor
        );
        Ok(())
    }

    async fn load_profiles(&mut self, io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let active_idx = self
            .get_register(io, REG_CURRENT_PROFILE, [0x00, 0x00])
            .await
            .map(|p| u32::from(p[0]))
            .unwrap_or_else(|e| {
                warn!("Failed to read current profile: {e}");
                0
            });

        for profile in &mut info.profiles {
            profile.is_active = profile.index == active_idx;
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
            /* Write the new active profile index */
            self.set_register(io, REG_CURRENT_PROFILE, [idx, 0x00])
                .await
                .context("Failed to commit active profile")?;
            debug!("HID++ 1.0: committed active profile = {idx}");
        }
        Ok(())
    }
}
