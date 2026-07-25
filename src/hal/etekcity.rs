/// Etekcity/Redragon gaming mouse driver.
///
/// Targets mice using the Etekcity USB HID protocol: Redragon M709, Etekcity
/// Scroll 1, and similar devices.
///
/// # Status
/// **Stub** — protocol constants and data layout are complete, but
/// `probe`/`load_profiles`/`commit` are not yet implemented.
///
/// Reference implementation: `src/driver-etekcity.c`.
use anyhow::Result;
use async_trait::async_trait;

use crate::engine::device::DeviceInfo;
use crate::hal::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                  */
/* ------------------------------------------------------------------ */

/// Maximum profile index (0-based).
const ETEKCITY_PROFILE_MAX: u8 = 4;
/// Number of programmable buttons.
const ETEKCITY_BUTTON_MAX: usize = 10;
/// Number of DPI slots per profile.
const ETEKCITY_NUM_DPI: usize = 6;

/* HID report IDs */
const ETEKCITY_REPORT_ID_PROFILE: u8 = 0x05;

/* Report sizes in bytes */
const ETEKCITY_REPORT_SIZE_PROFILE: usize = 50;

/// Maximum number of keycode events in a single macro.
const ETEKCITY_MAX_MACRO_LENGTH: usize = 50;

/* ------------------------------------------------------------------ */
/* In-memory device state (mirrors C `etekcity_data`)                  */
/* ------------------------------------------------------------------ */

/// Packed HID settings report (40 bytes) for a single profile.
#[derive(Debug, Default, Clone)]
pub struct SettingsReport {
    /// Report ID 0x06 (settings).
    pub report_id: u8,
    pub twenty_eight: u8,
    pub profile_id: u8,
    pub x_sensitivity: u8, /* 0x0a = 0 */
    pub y_sensitivity: u8,
    pub dpi_mask: u8,
    pub xres: [u8; ETEKCITY_NUM_DPI],
    pub yres: [u8; ETEKCITY_NUM_DPI],
    pub current_dpi: u8,
    pub _padding1: [u8; 7],
    pub report_rate: u8,
    pub _padding2: [u8; 4],
    pub light: u8,
    pub light_heartbeat: u8,
    pub _padding3: [u8; 5],
}

/// Macro entry: one (keycode, flag) pair within a macro sequence.
#[derive(Debug, Default, Clone, Copy)]
pub struct MacroKey {
    pub keycode: u8,
    pub flag: u8,
}

/// Full device state cached after `probe()`.
/// Populated during probe; read only once load_profiles/commit are ported.
#[allow(dead_code)]
#[derive(Debug)]
struct EtekcityData {
    /// Raw profile key-mapping reports. Index = profile number.
    profiles: Vec<[u8; ETEKCITY_REPORT_SIZE_PROFILE]>,
    /// Parsed settings for each profile.
    settings: Vec<SettingsReport>,
    /// Macro state: `[profile][button][key_index]`.
    macros: Vec<Vec<[MacroKey; ETEKCITY_MAX_MACRO_LENGTH]>>,
    /// Current speed-setting report.
    speed_setting: [u8; 6],
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct EtekcityDriver {
    data: Option<EtekcityData>,
}

impl EtekcityDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

#[async_trait]
impl DeviceDriver for EtekcityDriver {
    fn name(&self) -> &str {
        "Etekcity"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Query the current profile to confirm the device responds. */
        let mut buf = [0u8; 3];
        buf[0] = ETEKCITY_REPORT_ID_PROFILE;
        io.get_feature_report(&mut buf)
            .map_err(anyhow::Error::from)?;

        let num_profiles = (ETEKCITY_PROFILE_MAX + 1) as usize;
        self.data = Some(EtekcityData {
            profiles: vec![[0u8; ETEKCITY_REPORT_SIZE_PROFILE]; num_profiles],
            settings: vec![SettingsReport::default(); num_profiles],
            macros: vec![
                vec![[MacroKey::default(); ETEKCITY_MAX_MACRO_LENGTH]; ETEKCITY_BUTTON_MAX + 1];
                num_profiles
            ],
            speed_setting: [0u8; 6],
        });

        // TODO: read all profiles, settings and macros from hardware.
        anyhow::bail!("Etekcity driver: load_profiles not yet implemented in the Rust port");
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, _info: &mut DeviceInfo) -> Result<()> {
        // TODO: parse `self.data` and fill `info.profiles`.
        anyhow::bail!("Etekcity driver: load_profiles not yet implemented in the Rust port");
    }

    async fn commit(&mut self, _io: &mut DeviceIo, _info: &DeviceInfo) -> Result<()> {
        // TODO: write dirty profiles back to hardware.
        anyhow::bail!("Etekcity driver: commit not yet implemented in the Rust port");
    }
}
