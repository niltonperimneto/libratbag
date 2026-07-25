/// G.Skill gaming mouse driver.
///
/// Targets G.Skill Ripjaws mice (MX780 and similar).
/// Protocol features: 5 profiles, up to 5 DPI slots, 10 buttons,
/// 3 LED zones (logo, wheel, tail) plus a DPI LED, and complex macro support.
///
/// # Status
/// **Stub** — protocol constants and data layout are complete, but
/// `probe`/`load_profiles`/`commit` are not yet implemented.
///
/// Reference implementation: `src/driver-gskill.c`.
use anyhow::Result;
use async_trait::async_trait;

use crate::engine::device::DeviceInfo;
use crate::hal::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

const GSKILL_PROFILE_MAX: usize = 5;

/* HID commands */
const GSKILL_GET_CURRENT_PROFILE_NUM: u8 = 0x03;

/* Report sizes */
const GSKILL_REPORT_SIZE_PROFILE: usize = 644;
const GSKILL_REPORT_SIZE_CMD: usize = 9;

/* Command status codes returned by the device */
const GSKILL_CMD_SUCCESS: u8 = 0xb0;
const GSKILL_CMD_IDLE: u8 = 0xb3;

/* ------------------------------------------------------------------ */
/* Cached hardware state                                                */
/* ------------------------------------------------------------------ */

/* Populated during probe; read only once load_profiles/commit are ported. */
#[allow(dead_code)]
#[derive(Debug)]
struct GskillData {
    /// Raw profile reports read from hardware. `None` = not yet loaded.
    profiles: [Option<Box<[u8; GSKILL_REPORT_SIZE_PROFILE]>>; GSKILL_PROFILE_MAX],
    active_profile: u8,
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct GskillDriver {
    data: Option<GskillData>,
}

impl GskillDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

#[async_trait]
impl DeviceDriver for GskillDriver {
    fn name(&self) -> &str {
        "G.Skill"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Query current profile number to confirm device presence. */
        let mut cmd = [0u8; GSKILL_REPORT_SIZE_CMD];
        cmd[0] = GSKILL_GET_CURRENT_PROFILE_NUM;
        io.get_feature_report(&mut cmd)
            .map_err(anyhow::Error::from)?;

        let status = cmd[1];
        if status != GSKILL_CMD_SUCCESS && status != GSKILL_CMD_IDLE {
            anyhow::bail!("G.Skill probe: unexpected status byte {status:#04x}");
        }

        self.data = Some(GskillData {
            profiles: Default::default(),
            active_profile: cmd[2] & 0x0f,
        });

        // TODO: read all profiles using GSKILL_GET_SET_PROFILE.
        anyhow::bail!("G.Skill driver: load_profiles not yet implemented in the Rust port");
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, _info: &mut DeviceInfo) -> Result<()> {
        // TODO: parse cached profile reports and fill info.profiles.
        anyhow::bail!("G.Skill driver: load_profiles not yet implemented in the Rust port");
    }

    async fn commit(&mut self, _io: &mut DeviceIo, _info: &DeviceInfo) -> Result<()> {
        // TODO: write dirty profiles back using GSKILL_GET_SET_PROFILE.
        anyhow::bail!("G.Skill driver: commit not yet implemented in the Rust port");
    }
}
