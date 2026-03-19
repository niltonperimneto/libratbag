/// OpenInput protocol driver.
///
/// Targets mice implementing the OpenInput HID protocol, an open-source
/// hardware configuration protocol for gaming peripherals.
///
/// The protocol uses raw HID output/input reports (NOT feature reports).
/// Communication follows a request-response pattern: write a short (8B)
/// or long (32B) report, then read back the device's response which may
/// arrive in either format.
///
/// At this stage the protocol implementation covers discovery only:
/// version query, firmware info, and function page enumeration.
/// No configuration writes are supported — `commit()` is a no-op.
///
/// Reference implementation: `src/driver-openinput.c`.
use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, info, warn};

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
/// Byte offset where payload data begins.
const OI_REPORT_DATA_INDEX: usize = 3;
const OI_REPORT_DATA_MAX_SIZE: usize = OI_REPORT_LONG_SIZE - OI_REPORT_DATA_INDEX;

/* ------------------------------------------------------------------ */
/* Protocol function pages                                              */
/* ------------------------------------------------------------------ */

const OI_PAGE_INFO: u8 = 0x00;
const OI_PAGE_ERROR: u8 = 0xFF;

/* Info page (0x00) functions */
const OI_FUNCTION_VERSION: u8 = 0x00;
const OI_FUNCTION_FW_INFO: u8 = 0x01;
const OI_FUNCTION_SUPPORTED_PAGES: u8 = 0x02;
const OI_FUNCTION_SUPPORTED_FUNCTIONS: u8 = 0x03;

/* Firmware info field IDs */
const OI_FW_INFO_VENDOR: u8 = 0x00;
const OI_FW_INFO_VERSION: u8 = 0x01;
const OI_FW_INFO_DEVICE_NAME: u8 = 0x02;

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
        let len = OI_REPORT_SHORT_SIZE - OI_REPORT_DATA_INDEX;
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
        buf[OI_REPORT_DATA_INDEX..OI_REPORT_LONG_SIZE]
            .copy_from_slice(&self.data[..OI_REPORT_DATA_MAX_SIZE]);
        buf
    }

    /// Parse from a raw buffer (short or long format).
    fn from_buf(buf: &[u8]) -> Option<Self> {
        if buf.len() < OI_REPORT_SHORT_SIZE {
            return None;
        }
        let mut report = OiReport {
            id: buf[0],
            function_page: buf[1],
            function: buf[2],
            data: [0u8; OI_REPORT_DATA_MAX_SIZE],
        };
        let data_len = (buf.len() - OI_REPORT_DATA_INDEX).min(OI_REPORT_DATA_MAX_SIZE);
        report.data[..data_len].copy_from_slice(&buf[OI_REPORT_DATA_INDEX..OI_REPORT_DATA_INDEX + data_len]);
        Some(report)
    }
}

/* ------------------------------------------------------------------ */
/* Cached state                                                         */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct OiData {
    fw_major: u8,
    fw_minor: u8,
    fw_patch: u8,
    fw_vendor: String,
    fw_version_str: String,
    device_name: String,
    supported: u64,
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

/* ------------------------------------------------------------------ */
/* I/O helpers                                                          */
/* ------------------------------------------------------------------ */

/* Send a short OpenInput report and read back the response.
 *
 * The protocol uses raw HID output/input reports — NOT feature reports.
 * The response may arrive as either short (8B) or long (32B) format;
 * we allocate a long-size buffer and accept either. If the device
 * returns an error page, the error is translated to an anyhow::Error. */
async fn oi_send(io: &mut DeviceIo, report: &OiReport) -> Result<OiReport> {
    let buf = report.to_short_buf();
    io.write_report(&buf).await?;

    let mut resp_buf = [0u8; OI_REPORT_LONG_SIZE];
    let n = io.read_report(&mut resp_buf).await?;

    /* Validate response length matches a known report size. */
    if n != OI_REPORT_SHORT_SIZE && n != OI_REPORT_LONG_SIZE {
        anyhow::bail!(
            "OpenInput: unexpected response size {n} (expected {OI_REPORT_SHORT_SIZE} or {OI_REPORT_LONG_SIZE})"
        );
    }

    let resp = OiReport::from_buf(&resp_buf[..n])
        .ok_or_else(|| anyhow::anyhow!("OpenInput: failed to parse response"))?;

    /* Check for error page response. */
    if resp.function_page == OI_PAGE_ERROR {
        let err_msg = match resp.function {
            0x01 => format!(
                "Invalid value (in position {})",
                resp.data[2]
            ),
            0x02 => format!(
                "Unsupported function ({:#04x}, {:#04x})",
                resp.data[0], resp.data[1]
            ),
            0xFE => {
                /* Custom error: data contains a string. */
                let end = resp.data.iter().position(|&b| b == 0).unwrap_or(resp.data.len());
                let msg = String::from_utf8_lossy(&resp.data[..end]);
                format!("Custom error ({msg})")
            }
            code => format!("Unknown error ({code})"),
        };
        anyhow::bail!("OpenInput: {err_msg}");
    }

    Ok(resp)
}

/* Query protocol version from the info page. */
async fn oi_query_version(io: &mut DeviceIo) -> Result<(u8, u8, u8)> {
    let req = OiReport {
        id: OI_REPORT_SHORT,
        function_page: OI_PAGE_INFO,
        function: OI_FUNCTION_VERSION,
        ..Default::default()
    };

    let resp = oi_send(io, &req).await?;
    let major = resp.data[0];
    let minor = resp.data[1];
    let patch = resp.data[2];

    info!("OpenInput: protocol version {major}.{minor}.{patch}");
    Ok((major, minor, patch))
}

/* Query firmware info (vendor, version string, or device name). */
async fn oi_query_fw_info(io: &mut DeviceIo, field_id: u8) -> Result<String> {
    let mut req = OiReport {
        id: OI_REPORT_SHORT,
        function_page: OI_PAGE_INFO,
        function: OI_FUNCTION_FW_INFO,
        ..Default::default()
    };
    req.data[0] = field_id;

    let resp = oi_send(io, &req).await?;
    let end = resp.data.iter().position(|&b| b == 0).unwrap_or(resp.data.len());
    let s = String::from_utf8_lossy(&resp.data[..end]).to_string();
    Ok(s)
}

/* Read supported function pages from the device (paginated query).
 *
 * The device returns pages in batches. Each response contains:
 *   data[0] = count of pages in this batch
 *   data[1] = pages remaining after this batch
 *   data[2..2+count] = page IDs
 * We loop until remaining == 0. */
async fn oi_read_supported_pages(io: &mut DeviceIo) -> Result<Vec<u8>> {
    debug!("OpenInput: reading supported function pages...");

    let mut all_pages = Vec::new();
    let mut start_index: u8 = 0;
    let mut expected_total: Option<usize> = None;

    loop {
        let mut req = OiReport {
            id: OI_REPORT_SHORT,
            function_page: OI_PAGE_INFO,
            function: OI_FUNCTION_SUPPORTED_PAGES,
            ..Default::default()
        };
        req.data[0] = start_index;

        let resp = oi_send(io, &req).await?;
        let count = resp.data[0] as usize;
        let remaining = resp.data[1] as usize;

        /* Guard against protocol errors that would cause infinite loops. */
        if count == 0 && remaining > 0 {
            warn!("OpenInput: device reports 0 pages in batch but {remaining} remaining — aborting");
            break;
        }

        /* Validate total consistency across paginated responses
         * (matching the C driver's safety check). */
        let current_total = all_pages.len() + count + remaining;
        match expected_total {
            Some(total) if total != current_total => {
                anyhow::bail!(
                    "OpenInput: inconsistent page count \
                     (expected {total}, got {current_total})"
                );
            }
            None => {
                expected_total = Some(current_total);
            }
            _ => {}
        }

        for i in 0..count {
            if let Some(&page) = resp.data.get(2 + i) {
                all_pages.push(page);
                debug!("OpenInput: found function page {}", page_name(page));
            }
        }

        if remaining == 0 {
            break;
        }
        start_index += count as u8;
    }

    Ok(all_pages)
}

/* Read supported functions for a specific page (paginated query).
 *
 * Same pagination scheme as supported_pages:
 *   data[0] = count in this batch
 *   data[1] = remaining
 *   data[2..2+count] = function IDs */
async fn oi_read_supported_functions(io: &mut DeviceIo, page: u8) -> Result<Vec<u8>> {
    let mut all_functions = Vec::new();
    let mut start_index: u8 = 0;
    let mut expected_total: Option<usize> = None;

    loop {
        let mut req = OiReport {
            id: OI_REPORT_SHORT,
            function_page: OI_PAGE_INFO,
            function: OI_FUNCTION_SUPPORTED_FUNCTIONS,
            ..Default::default()
        };
        req.data[0] = page;
        req.data[1] = start_index;

        let resp = oi_send(io, &req).await?;
        let count = resp.data[0] as usize;
        let remaining = resp.data[1] as usize;

        if count == 0 && remaining > 0 {
            warn!("OpenInput: device reports 0 functions for page {:#04x} but {remaining} remaining — aborting", page);
            break;
        }

        /* Validate total consistency across paginated responses to prevent
         * infinite loops (matching the C driver's safety check). The total
         * (read + count + remaining) must stay constant across all calls. */
        let current_total = all_functions.len() + count + remaining;
        match expected_total {
            Some(total) if total != current_total => {
                anyhow::bail!(
                    "OpenInput: inconsistent function count for page {:#04x} \
                     (expected {total}, got {current_total})",
                    page
                );
            }
            None => {
                expected_total = Some(current_total);
            }
            _ => {}
        }

        for i in 0..count {
            if let Some(&func) = resp.data.get(2 + i) {
                all_functions.push(func);
                debug!(
                    "OpenInput: page {} function {:#04x}",
                    page_name(page),
                    func
                );
            }
        }

        if remaining == 0 {
            break;
        }
        start_index += count as u8;
    }

    Ok(all_functions)
}

/* ------------------------------------------------------------------ */
/* DeviceDriver implementation                                          */
/* ------------------------------------------------------------------ */

#[async_trait]
impl DeviceDriver for OpenInputDriver {
    fn name(&self) -> &str {
        "OpenInput"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Query protocol version. */
        let (fw_major, fw_minor, fw_patch) = oi_query_version(io).await?;

        /* Query firmware info strings. */
        let fw_vendor = oi_query_fw_info(io, OI_FW_INFO_VENDOR).await.unwrap_or_default();
        if !fw_vendor.is_empty() {
            info!("OpenInput: firmware vendor: {fw_vendor}");
        }

        let fw_version_str = oi_query_fw_info(io, OI_FW_INFO_VERSION).await.unwrap_or_default();
        if !fw_version_str.is_empty() {
            info!("OpenInput: firmware version: {fw_version_str}");
        }

        let device_name = oi_query_fw_info(io, OI_FW_INFO_DEVICE_NAME).await.unwrap_or_default();
        if !device_name.is_empty() {
            info!("OpenInput: device: {device_name}");
        }

        /* Enumerate supported function pages and their functions.
         * This builds a bitmask of supported pages for future use. */
        let mut supported: u64 = 0;
        let pages = oi_read_supported_pages(io).await.unwrap_or_default();

        for &page in &pages {
            /* Set bit in supported bitmask for pages < 64. */
            if page < 64 {
                supported |= 1u64 << page;
            }

            /* Read per-page functions (logged but not yet acted upon). */
            let _functions = oi_read_supported_functions(io, page).await.unwrap_or_default();
        }

        self.data = Some(OiData {
            fw_major,
            fw_minor,
            fw_patch,
            fw_vendor,
            fw_version_str,
            device_name,
            supported,
        });

        Ok(())
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("OpenInput: probe() was not called"))?;

        /* Set firmware version string on the DeviceInfo. */
        info.firmware_version = format!("{}.{}.{}", data.fw_major, data.fw_minor, data.fw_patch);

        /* The OpenInput protocol at this stage exposes a single profile
         * with no configurable buttons or LEDs — matching the scope of
         * the C driver. The profile has report rate support but no
         * DPI/button/LED configuration. */
        for profile in &mut info.profiles {
            profile.report_rates = REPORT_RATES.to_vec();
            profile.is_active = true;
        }

        Ok(())
    }

    async fn commit(&mut self, _io: &mut DeviceIo, _info: &DeviceInfo) -> Result<()> {
        /* The OpenInput protocol is read-only at this stage. The C driver
         * has no commit function either — all discovery data is informational. */
        Ok(())
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

/* ------------------------------------------------------------------ */
/* Tests                                                                */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oi_report_short_roundtrip() {
        /* A short report should serialize to 8 bytes with correct layout. */
        let report = OiReport {
            id: OI_REPORT_SHORT,
            function_page: 0x00,
            function: 0x00,
            data: {
                let mut d = [0u8; OI_REPORT_DATA_MAX_SIZE];
                d[0] = 0x01;
                d[1] = 0x02;
                d[2] = 0x03;
                d
            },
        };

        let buf = report.to_short_buf();
        assert_eq!(buf.len(), OI_REPORT_SHORT_SIZE);
        assert_eq!(buf[0], OI_REPORT_SHORT);
        assert_eq!(buf[1], 0x00); /* function_page */
        assert_eq!(buf[2], 0x00); /* function */
        assert_eq!(buf[3], 0x01); /* data[0] */
        assert_eq!(buf[4], 0x02); /* data[1] */
        assert_eq!(buf[5], 0x03); /* data[2] */

        /* Parse it back. */
        let parsed = OiReport::from_buf(&buf).expect("parse failed");
        assert_eq!(parsed.id, OI_REPORT_SHORT);
        assert_eq!(parsed.function_page, 0x00);
        assert_eq!(parsed.function, 0x00);
        assert_eq!(parsed.data[0], 0x01);
        assert_eq!(parsed.data[1], 0x02);
        assert_eq!(parsed.data[2], 0x03);
    }

    #[test]
    fn oi_report_long_roundtrip() {
        /* A long report should serialize to 32 bytes. */
        let mut report = OiReport {
            id: OI_REPORT_LONG,
            function_page: 0xFD,
            function: 0x03,
            ..Default::default()
        };
        report.data[0] = 0xAA;
        report.data[28] = 0xBB; /* last data byte */

        let buf = report.to_long_buf();
        assert_eq!(buf.len(), OI_REPORT_LONG_SIZE);
        assert_eq!(buf[0], OI_REPORT_LONG);
        assert_eq!(buf[1], 0xFD);
        assert_eq!(buf[2], 0x03);
        assert_eq!(buf[3], 0xAA); /* data[0] */
        assert_eq!(buf[31], 0xBB); /* data[28] = last byte */

        /* Parse it back. */
        let parsed = OiReport::from_buf(&buf).expect("parse failed");
        assert_eq!(parsed.id, OI_REPORT_LONG);
        assert_eq!(parsed.function_page, 0xFD);
        assert_eq!(parsed.data[0], 0xAA);
        assert_eq!(parsed.data[28], 0xBB);
    }

    #[test]
    fn oi_report_from_buf_too_short() {
        /* Buffers shorter than 8 bytes should fail to parse. */
        let buf = [0x20, 0x00, 0x00];
        assert!(OiReport::from_buf(&buf).is_none());
    }

    #[test]
    fn page_name_known_pages() {
        assert_eq!(page_name(0x00), "INFO");
        assert_eq!(page_name(0x01), "SETTINGS");
        assert_eq!(page_name(0x02), "DPI");
        assert_eq!(page_name(0x03), "BUTTONS");
        assert_eq!(page_name(0x04), "LEDS");
        assert_eq!(page_name(0xFD), "GIMMICKS");
        assert_eq!(page_name(0xFE), "DEBUG");
        assert_eq!(page_name(0xFF), "ERROR");
        assert_eq!(page_name(0x42), "UNKNOWN");
    }
}
