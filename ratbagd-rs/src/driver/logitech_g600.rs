/// Logitech G600 gaming mouse driver.
///
/// Targets the Logitech G600 MMO Gaming Mouse, a 20-button device with
/// 3 profiles, 4 DPI levels, and one RGB LED zone.
///
/// # Status
/// **Stub** — protocol constants and data layout are complete, but
/// `probe`/`load_profiles`/`commit` are not yet implemented.
///
/// Reference implementation: `src/driver-logitech-g600.c`.
use anyhow::Result;
use async_trait::async_trait;

use crate::device::DeviceInfo;
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

const NUM_PROFILES: usize = 3;
const NUM_BUTTONS: usize = 41; /* 20 standard + 20 G-Shift + 1 color buffer */
const NUM_DPI: usize = 4;
const NUM_LED: usize = 1;

const DPI_MIN: u32 = 200;
const DPI_MAX: u32 = 8200;

/* HID report IDs */
const REPORT_ID_GET_ACTIVE: u8 = 0xF0;
const REPORT_ID_SET_ACTIVE: u8 = 0xF0;
const REPORT_ID_PROFILE_0: u8 = 0xF3;
const REPORT_ID_PROFILE_1: u8 = 0xF4;
const REPORT_ID_PROFILE_2: u8 = 0xF5;

/// Size of a full profile report (bytes).
const REPORT_SIZE_PROFILE: usize = 154;

/* LED effect values */
const LED_SOLID: u8 = 0x00;
const LED_BREATHE: u8 = 0x01;
const LED_CYCLE: u8 = 0x02;

/* ------------------------------------------------------------------ */
/* Button action codes                                                  */
/* ------------------------------------------------------------------ */

#[repr(u8)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonCode {
    /// Standard (left/right/middle/side/extra) mouse button.
    MouseButton = 0x00,
    /// Keyboard key press.
    Key = 0x01,
    /// G-Shift modifier held.
    GShift = 0x02,
    /// Disabled.
    Disabled = 0x0f,
}

/* ------------------------------------------------------------------ */
/* Report data layouts                                                  */
/* ------------------------------------------------------------------ */

/// A single button entry in the profile report (3 bytes, packed).
#[derive(Debug, Default, Clone, Copy)]
pub struct ButtonEntry {
    /// Action code — see `ButtonCode`.
    pub code: u8,
    /// Modifier byte (e.g., shift/ctrl for key actions).
    pub modifier: u8,
    /// Keycode or button index.
    pub key: u8,
}

/// Full profile report as it appears in the HID feature report.
///
/// Size must equal `REPORT_SIZE_PROFILE` (154 bytes).
#[derive(Debug, Clone)]
pub struct ProfileReport {
    pub id: u8,
    pub led_red: u8,
    pub led_green: u8,
    pub led_blue: u8,
    pub led_effect: u8,
    /// LED animation duration.
    pub led_duration: u8,
    pub unknown1: [u8; 5],
    /// Polling frequency encoded as `frequency = 1000 / (value + 1)` Hz.
    pub frequency: u8,
    /// DPI Shift mode resolution: `value * 50` from 200 (0x04) to 8200 (0xa4); 0x00 = disabled.
    pub dpi_shift: u8,
    /// Default DPI slot index (1-indexed, 1–4).
    pub dpi_default: u8,
    /// DPI slot values: `value * 50` = actual DPI; 0x00 = disabled.
    pub dpi: [u8; NUM_DPI],
    pub unknown2: [u8; 13],
    pub buttons: [ButtonEntry; 20],
    /// G-Shift mode color (R, G, B).
    pub g_shift_color: [u8; 3],
    pub g_shift_buttons: [ButtonEntry; 20],
}

/// Polled active-profile + resolution report.
#[derive(Debug, Default, Clone, Copy)]
pub struct ActiveProfileReport {
    pub id: u8,
    /// Packed: `unknown1[0:0] | resolution[1:2] | unknown2[3:3] | profile[4:7]`.
    pub packed: u8,
    pub unknown3: u8,
    pub unknown4: u8,
}

impl ActiveProfileReport {
    /// Extract the active profile index (0-based).
    pub fn profile(&self) -> u8 {
        (self.packed >> 4) & 0x0f
    }

    /// Extract the active resolution index (0-based).
    pub fn resolution(&self) -> u8 {
        (self.packed >> 1) & 0x03
    }
}

/* ------------------------------------------------------------------ */
/* DPI helpers                                                          */
/* ------------------------------------------------------------------ */

/// Convert a DPI value to the raw byte sent in the profile report.
/// Raw = `dpi / 50`.  Range: 200 (0x04) – 8200 (0xa4).
#[allow(dead_code)]
pub fn dpi_to_raw(dpi: u32) -> Option<u8> {
    if dpi < DPI_MIN || dpi > DPI_MAX || dpi % 50 != 0 {
        return None;
    }
    u8::try_from(dpi / 50).ok()
}

/// Decode the raw DPI byte to an actual DPI value.
#[allow(dead_code)]
pub fn raw_to_dpi(raw: u8) -> u32 {
    u32::from(raw) * 50
}

/// Decode the frequency byte to Hz.
#[allow(dead_code)]
pub fn raw_to_hz(raw: u8) -> u32 {
    if raw == 0 { 1000 } else { 1000 / (u32::from(raw) + 1) }
}

/* ------------------------------------------------------------------ */
/* Cached state                                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct G600Data {
    profile_reports: [Option<ProfileReport>; NUM_PROFILES],
    active: ActiveProfileReport,
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct LG600Driver {
    data: Option<G600Data>,
}

impl LG600Driver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

/// Report IDs for the three profiles, indexed by profile number.
const PROFILE_REPORT_IDS: [u8; NUM_PROFILES] = [
    REPORT_ID_PROFILE_0,
    REPORT_ID_PROFILE_1,
    REPORT_ID_PROFILE_2,
];

#[async_trait]
impl DeviceDriver for LG600Driver {
    fn name(&self) -> &str {
        "Logitech G600"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Read the active profile report to confirm the device responds. */
        let mut active_buf = [0u8; 4];
        active_buf[0] = REPORT_ID_GET_ACTIVE;
        io.get_feature_report(&mut active_buf)
            .map_err(anyhow::Error::from)?;

        self.data = Some(G600Data {
            profile_reports: Default::default(),
            active: ActiveProfileReport {
                id: active_buf[0],
                packed: active_buf[1],
                unknown3: active_buf[2],
                unknown4: active_buf[3],
            },
        });

        // TODO: read all three profile reports and parse them.
        anyhow::bail!(
            "Logitech G600 driver: load_profiles not yet implemented in the Rust port"
        );
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, _info: &mut DeviceInfo) -> Result<()> {
        // TODO: convert profile_reports → info.profiles.
        anyhow::bail!(
            "Logitech G600 driver: load_profiles not yet implemented in the Rust port"
        );
    }

    async fn commit(&mut self, _io: &mut DeviceIo, _info: &DeviceInfo) -> Result<()> {
        // TODO: write dirty profiles back using SET_FEATURE on REPORT_ID_PROFILE_*.
        anyhow::bail!(
            "Logitech G600 driver: commit not yet implemented in the Rust port"
        );
    }
}
