/// SinoWealth-based gaming mouse driver.
///
/// Covers mice using the SinoWealth HID protocol: Glorious Model O/O-,
/// G-Wolves Skoll, Genesis Xenon 770, DreamMachines DM5, and similar devices.
///
/// # Status
/// **Stub** — protocol constants and data layout are complete, but
/// `probe`/`load_profiles`/`commit` are not yet implemented.
///
/// Reference implementation: `src/driver-sinowealth.c`.
use anyhow::Result;
use async_trait::async_trait;

use crate::device::DeviceInfo;
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Report IDs                                                           */
/* ------------------------------------------------------------------ */

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportId {
    Config = 0x04,
    Cmd = 0x05,
    ConfigLong = 0x06,
}

/* ------------------------------------------------------------------ */
/* Command IDs                                                          */
/* ------------------------------------------------------------------ */

#[repr(u8)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandId {
    FirmwareVersion = 0x01,
    Profile = 0x02,
    GetConfig = 0x11,
    GetButtons = 0x12,
    Debounce = 0x1a,
    LongAngleSnappingAndLod = 0x1b,
    GetConfig2 = 0x21,
    GetButtons2 = 0x22,
    Macro = 0x30,
    GetConfig3 = 0x31,
    GetButtons3 = 0x32,
    Dfu = 0x75,
}

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

/// Size of the short command report (5 bytes + report ID).
pub const SINOWEALTH_CMD_SIZE: usize = 6;
/// Full config report size (bytes).
pub const SINOWEALTH_CONFIG_REPORT_SIZE: usize = 520;
/// Maximum configuration payload (bytes).
pub const SINOWEALTH_CONFIG_SIZE_MAX: usize = 167;
/// Minimum configuration payload for devices with shorter config data.
pub const SINOWEALTH_CONFIG_SIZE_MIN: usize = 123;
/// Button report payload size.
pub const SINOWEALTH_BUTTON_SIZE: usize = 88;
/// Macro report size.
pub const SINOWEALTH_MACRO_SIZE: usize = 515;

/// Minimum DPI supported by SinoWealth-based PMW3360 devices.
pub const SINOWEALTH_DPI_MIN: u32 = 100;
/// DPI increment step.
pub const SINOWEALTH_DPI_STEP: u32 = 100;
/// Fallback DPI when the device doesn't report a range.
pub const SINOWEALTH_DPI_FALLBACK: u32 = 2000;

/// Debounce time range (milliseconds).
pub const SINOWEALTH_DEBOUNCE_MIN: u32 = 4;
pub const SINOWEALTH_DEBOUNCE_MAX: u32 = 16;

/// Number of DPI slots per profile.
pub const SINOWEALTH_NUM_DPIS: usize = 8;
/// Maximum number of profiles (modes in the OEM software).
pub const SINOWEALTH_NUM_PROFILES_MAX: usize = 3;
/// Maximum number of programmable buttons.
pub const SINOWEALTH_NUM_BUTTONS_MAX: usize = 64;
/// Maximum macro length (real events).
pub const SINOWEALTH_MACRO_LENGTH_MAX: usize = 168;
/// Maximum macro timeout that fits in one byte (milliseconds).
pub const SINOWEALTH_MACRO_MAX_TIMEOUT: u8 = 0xff;

/// Valid debounce times (ms).
pub const SINOWEALTH_DEBOUNCE_TIMES: &[u32] = &[4, 6, 8, 10, 12, 14, 16];
/// Valid polling rates (Hz).
pub const SINOWEALTH_REPORT_RATES: &[u32] = &[125, 250, 500, 1000];

/* ------------------------------------------------------------------ */
/* Sensor IDs                                                           */
/* ------------------------------------------------------------------ */

#[repr(u8)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensor {
    Pmw3360 = 0x06,
    Pmw3212 = 0x08,
    Pmw3327 = 0x0e,
    Pmw3389 = 0x0f,
}

/* ------------------------------------------------------------------ */
/* RGB effect modes                                                     */
/* ------------------------------------------------------------------ */

#[repr(u8)]
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RgbEffect {
    Off = 0x00,
    Glorious = 0x01,
    /// Single constant color.
    Single = 0x02,
    /// Breathing with seven user-defined colors.
    Breathing7 = 0x03,
    Tail = 0x04,
    /// Full-RGB breathing.
    Breathing = 0x05,
    /// Each LED gets its own static color.
    Constant = 0x06,
    Rave = 0x07,
    Random = 0x08,
    Wave = 0x09,
    /// Single-color breathing (not available on all devices).
    Breathing1 = 0x0a,
    /// Value on devices without LEDs — do **not** overwrite.
    NotSupported = 0xff,
}

/* ------------------------------------------------------------------ */
/* Configuration bitmask                                                */
/* ------------------------------------------------------------------ */

/// Bit 3 of the config byte: independent X/Y DPI.
pub const SINOWEALTH_XY_INDEPENDENT: u8 = 0b0000_1000;

/* ------------------------------------------------------------------ */
/* Parsed device state                                                  */
/* ------------------------------------------------------------------ */

/// One DPI slot entry as stored in the configuration report.
#[derive(Debug, Default, Clone, Copy)]
pub struct DpiSlot {
    /// Raw DPI byte. Actual DPI = `(raw as u32 + 1) * SINOWEALTH_DPI_STEP`.
    pub raw_x: u8,
    /// Raw Y DPI byte (only used when `SINOWEALTH_XY_INDEPENDENT` is set).
    pub raw_y: u8,
    /// Whether this slot is enabled.
    pub enabled: bool,
}

/// Cached hardware state read during `probe`.
#[derive(Debug)]
struct SinoweathData {
    firmware_version: [u8; 2],
    config_raw: Box<[u8; SINOWEALTH_CONFIG_REPORT_SIZE]>,
    /// Current active profile index (0-based).
    active_profile: u8,
    /// Config size variant detected. Either `SINOWEALTH_CONFIG_SIZE_MIN` or
    /// `SINOWEALTH_CONFIG_SIZE_MAX` depending on firmware.
    config_size: usize,
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct SinowealhDriver {
    data: Option<SinoweathData>,
}

impl SinowealhDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

#[async_trait]
impl DeviceDriver for SinowealhDriver {
    fn name(&self) -> &str {
        "SinoWealth"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Read firmware version to confirm device presence. */
        let mut cmd = [0u8; SINOWEALTH_CMD_SIZE];
        cmd[0] = ReportId::Cmd as u8;
        cmd[1] = CommandId::FirmwareVersion as u8;
        io.get_feature_report(&mut cmd)
            .map_err(anyhow::Error::from)?;

        self.data = Some(SinoweathData {
            firmware_version: [cmd[2], cmd[3]],
            config_raw: Box::new([0u8; SINOWEALTH_CONFIG_REPORT_SIZE]),
            active_profile: 0,
            config_size: SINOWEALTH_CONFIG_SIZE_MAX,
        });

        // TODO: read full config, detect config_size variant, parse profile data.
        anyhow::bail!("SinoWealth driver: load_profiles not yet implemented in the Rust port");
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, _info: &mut DeviceInfo) -> Result<()> {
        // TODO: parse cached config_raw and fill info.profiles.
        anyhow::bail!("SinoWealth driver: load_profiles not yet implemented in the Rust port");
    }

    async fn commit(&mut self, _io: &mut DeviceIo, _info: &DeviceInfo) -> Result<()> {
        // TODO: write dirty profiles back and send save command.
        anyhow::bail!("SinoWealth driver: commit not yet implemented in the Rust port");
    }
}

/* ------------------------------------------------------------------ */
/* Helpers                                                              */
/* ------------------------------------------------------------------ */

/// Convert a raw DPI byte to actual DPI value.
#[allow(dead_code)]
pub fn raw_to_dpi(raw: u8) -> u32 {
    (u32::from(raw) + 1) * SINOWEALTH_DPI_STEP
}

/// Convert an actual DPI value to the raw byte the device expects.
/// Returns `None` if `dpi` is below `SINOWEALTH_DPI_MIN`.
#[allow(dead_code)]
pub fn dpi_to_raw(dpi: u32) -> Option<u8> {
    if dpi < SINOWEALTH_DPI_MIN {
        return None;
    }
    let raw = (dpi / SINOWEALTH_DPI_STEP).saturating_sub(1);
    u8::try_from(raw).ok()
}

/// Build a 6-byte command report ready to be sent as a feature report.
#[allow(dead_code)]
pub fn build_cmd(cmd_id: CommandId) -> [u8; SINOWEALTH_CMD_SIZE] {
    let mut buf = [0u8; SINOWEALTH_CMD_SIZE];
    buf[0] = ReportId::Cmd as u8;
    buf[1] = cmd_id as u8;
    buf
}
