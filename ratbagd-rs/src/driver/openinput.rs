/// OpenInput protocol driver.
///
/// Targets mice implementing the OpenInput HID protocol, an open-source
/// hardware configuration protocol for gaming peripherals.
///
/// # Status
/// **Stub** — protocol constants and data layout are complete, but
/// `probe`/`load_profiles`/`commit` are not yet implemented.
///
/// Reference implementation: `src/driver-openinput.c`.
use anyhow::Result;
use async_trait::async_trait;

use crate::device::DeviceInfo;
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Report IDs and sizes                                                 */
/* ------------------------------------------------------------------ */

/// Short report ID (8 bytes total).
const OI_REPORT_SHORT: u8 = 0x20;
/// Long report ID (32 bytes total).
const OI_REPORT_LONG: u8 = 0x21;

const OI_REPORT_SHORT_SIZE: usize = 8;
const OI_REPORT_LONG_SIZE: usize = 32;
const OI_REPORT_MAX_SIZE: usize = OI_REPORT_LONG_SIZE;
/// Byte offset where payload data begins.
const OI_REPORT_DATA_INDEX: usize = 3;
const OI_REPORT_DATA_MAX_SIZE: usize = OI_REPORT_LONG_SIZE - OI_REPORT_DATA_INDEX;

/* ------------------------------------------------------------------ */
/* Protocol function pages                                              */
/* ------------------------------------------------------------------ */

const OI_PAGE_INFO: u8 = 0x00;
#[allow(dead_code)]
const OI_PAGE_GIMMICKS: u8 = 0xFD;
#[allow(dead_code)]
const OI_PAGE_DEBUG: u8 = 0xFE;
const OI_PAGE_ERROR: u8 = 0xFF;

/* Info page (0x00) functions */
const OI_FUNCTION_VERSION: u8 = 0x00;
#[allow(dead_code)]
const OI_FUNCTION_FW_INFO: u8 = 0x01;
#[allow(dead_code)]
const OI_FUNCTION_SUPPORTED_PAGES: u8 = 0x02;
#[allow(dead_code)]
const OI_FUNCTION_SUPPORTED_FUNCTIONS: u8 = 0x03;

/* Error page (0xFF) codes */
const OI_ERROR_INVALID_VALUE: u8 = 0x01;
#[allow(dead_code)]
const OI_ERROR_UNSUPPORTED_FUNCTION: u8 = 0x02;
#[allow(dead_code)]
const OI_ERROR_CUSTOM: u8 = 0xFE;

/// Valid polling rates (Hz).
const REPORT_RATES: &[u32] = &[125, 250, 500, 750, 1000];

/* ------------------------------------------------------------------ */
/* Report payload layout                                                */
/* ------------------------------------------------------------------ */

/// A packed OpenInput HID report.
#[derive(Debug, Default, Clone)]
pub struct OiReport {
    /// Report ID (`OI_REPORT_SHORT` or `OI_REPORT_LONG`).
    pub id: u8,
    /// Function page.
    pub function_page: u8,
    /// Function number within the page.
    pub function: u8,
    /// Payload bytes.
    pub data: [u8; OI_REPORT_DATA_MAX_SIZE],
}

impl OiReport {
    /// Serialize into a short (8-byte) buffer.
    pub fn to_short_buf(&self) -> [u8; OI_REPORT_SHORT_SIZE] {
        let mut buf = [0u8; OI_REPORT_SHORT_SIZE];
        buf[0] = self.id;
        buf[1] = self.function_page;
        buf[2] = self.function;
        let len = (OI_REPORT_SHORT_SIZE - OI_REPORT_DATA_INDEX).min(self.data.len());
        buf[OI_REPORT_DATA_INDEX..OI_REPORT_DATA_INDEX + len]
            .copy_from_slice(&self.data[..len]);
        buf
    }

    /// Serialize into a long (32-byte) buffer.
    pub fn to_long_buf(&self) -> [u8; OI_REPORT_LONG_SIZE] {
        let mut buf = [0u8; OI_REPORT_LONG_SIZE];
        buf[0] = self.id;
        buf[1] = self.function_page;
        buf[2] = self.function;
        let len = OI_REPORT_DATA_MAX_SIZE.min(self.data.len());
        buf[OI_REPORT_DATA_INDEX..OI_REPORT_DATA_INDEX + len]
            .copy_from_slice(&self.data[..len]);
        buf
    }
}

/* ------------------------------------------------------------------ */
/* Capability bitmask                                                   */
/* ------------------------------------------------------------------ */

/// Bitmask of supported feature pages discovered via `SUPPORTED_PAGES`.
pub type SupportedPages = u64;

/* ------------------------------------------------------------------ */
/* Cached state                                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct OiData {
    fw_major: u8,
    fw_minor: u8,
    fw_patch: u8,
    num_profiles: u32,
    num_resolutions: u32,
    num_buttons: u32,
    num_leds: u32,
    supported: SupportedPages,
}

/* ------------------------------------------------------------------ */
/* Driver                                                               */
/* ------------------------------------------------------------------ */

pub struct OpenInputDriver {
    data: Option<OiData>,
}

impl OpenInputDriver {
    pub fn new() -> Self {
        Self { data: None }
    }
}

#[async_trait]
impl DeviceDriver for OpenInputDriver {
    fn name(&self) -> &str {
        "OpenInput"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Query protocol version from the info page. */
        let req = OiReport {
            id: OI_REPORT_SHORT,
            function_page: OI_PAGE_INFO,
            function: OI_FUNCTION_VERSION,
            data: [0u8; OI_REPORT_DATA_MAX_SIZE],
        };
        let mut response = [0u8; OI_REPORT_SHORT_SIZE];

        let buf = req.to_short_buf();
        let mut resp_buf = response;
        resp_buf[0] = OI_REPORT_SHORT;

        io.write_report(&buf).await?;
        io.read_report(&mut resp_buf).await?;

        /* Check for error response */
        if resp_buf[1] == OI_PAGE_ERROR {
            anyhow::bail!(
                "OpenInput device returned error on version query: code={:#04x}",
                resp_buf[OI_REPORT_DATA_INDEX]
            );
        }

        self.data = Some(OiData {
            fw_major: resp_buf[OI_REPORT_DATA_INDEX],
            fw_minor: resp_buf[OI_REPORT_DATA_INDEX + 1],
            fw_patch: resp_buf[OI_REPORT_DATA_INDEX + 2],
            num_profiles: 1,
            num_resolutions: 1,
            num_buttons: 0,
            num_leds: 0,
            supported: 0,
        });

        let _ = &response;

        // TODO: query supported function pages and device capabilities.
        anyhow::bail!("OpenInput driver: load_profiles not yet implemented in the Rust port");
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, _info: &mut DeviceInfo) -> Result<()> {
        anyhow::bail!("OpenInput driver: load_profiles not yet implemented in the Rust port");
    }

    async fn commit(&mut self, _io: &mut DeviceIo, _info: &DeviceInfo) -> Result<()> {
        anyhow::bail!("OpenInput driver: commit not yet implemented in the Rust port");
    }
}

/* ------------------------------------------------------------------ */
/* Helpers                                                              */
/* ------------------------------------------------------------------ */

/// Build a short OpenInput feature request.
#[allow(dead_code)]
pub fn build_request(page: u8, function: u8) -> OiReport {
    OiReport {
        id: OI_REPORT_SHORT,
        function_page: page,
        function,
        data: [0u8; OI_REPORT_DATA_MAX_SIZE],
    }
}

/// Return a human-readable name for a function page.
#[allow(dead_code)]
pub fn page_name(page: u8) -> &'static str {
    match page {
        0x00 => "INFO",
        0x01 => "SETTINGS",
        0x02 => "DPI",
        0x03 => "BUTTONS",
        0x04 => "LEDS",
        0xFD => "GIMMICKS",
        0xFE => "DEBUG",
        0xFF => "ERROR",
        _ => "UNKNOWN",
    }
}
