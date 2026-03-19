/// G.Skill gaming mouse driver.
///
/// Targets G.Skill Ripjaws mice (MX780 and similar).
/// Protocol features: 5 profiles, up to 5 DPI slots, 10 buttons,
/// 3 LED zones (logo, wheel, tail) plus a DPI LED, and complex macro support.
///
/// The protocol uses HID feature reports for all communication. A general
/// command system (report ID 0x0C, 9 bytes) provides device control with a
/// status-polling retry loop, while profile data (report ID 0x05, 644 bytes)
/// and macros (report ID 0x04, 2052 bytes) use direct feature report reads
/// and writes after selecting the target index via a general command.
///
/// Reference implementation: `src/driver-gskill.c`.
use anyhow::Result;
use async_trait::async_trait;
use tokio::time::{sleep, Duration};
use tracing::{debug, warn};

use crate::device::{ActionType, DeviceInfo, Dpi, special_action};
use crate::driver::{DeviceDriver, DeviceIo};

/* ------------------------------------------------------------------ */
/* Protocol constants                                                   */
/* ------------------------------------------------------------------ */

const GSKILL_PROFILE_MAX: usize = 5;
const GSKILL_NUM_DPI: usize = 5;
const GSKILL_BUTTON_MAX: usize = 10;

const GSKILL_MAX_POLLING_RATE: u32 = 1000;

const GSKILL_MIN_DPI: u32 = 100;
const GSKILL_MAX_DPI: u32 = 8200;
const GSKILL_DPI_UNIT: u32 = 50;

/* HID report IDs */
const GSKILL_GET_SET_PROFILE: u8 = 0x05;
const GSKILL_GENERAL_CMD: u8 = 0x0c;

/* Report sizes */
const GSKILL_REPORT_SIZE_PROFILE: usize = 644;
const GSKILL_REPORT_SIZE_CMD: usize = 9;

/* Byte offset of the checksum in profile reports. */
const GSKILL_CHECKSUM_OFFSET: usize = 3;

/* Command status codes returned by the device */
const GSKILL_CMD_SUCCESS: u8 = 0xb0;
const GSKILL_CMD_IN_PROGRESS: u8 = 0xb1;
const GSKILL_CMD_FAILURE: u8 = 0xb2;

/* Command-status retry parameters */
const GSKILL_CMD_MAX_RETRIES: u8 = 10;
const GSKILL_CMD_RETRY_MS: u64 = 20;

/* Profile report byte offsets (from __attribute__((packed)) struct) */
const OFF_PROFILE_NUM: usize = 2;
const OFF_CHECKSUM: usize = 3;
const OFF_RATE_SNAP: usize = 4;
const OFF_DPI_INFO: usize = 7;
const OFF_DPI_LEVELS: usize = 8;
const OFF_BTN_CFGS: usize = 311;
const OFF_NAME: usize = 388;
const NAME_SIZE: usize = 256;

/* Button function type values from the hardware protocol. */
const BTN_FUNC_WHEEL: u8 = 0x00;
const BTN_FUNC_MOUSE: u8 = 0x01;
const BTN_FUNC_KBD: u8 = 0x02;
const BTN_FUNC_CONSUMER: u8 = 0x03;
const BTN_FUNC_MACRO: u8 = 0x06;
const BTN_FUNC_DPI_UP: u8 = 0x09;
const BTN_FUNC_DPI_DOWN: u8 = 0x0a;
const BTN_FUNC_CYCLE_DPI_UP: u8 = 0x0b;
const BTN_FUNC_CYCLE_PROFILE_UP: u8 = 0x18;
const BTN_FUNC_CYCLE_PROFILE_DOWN: u8 = 0x19;
const BTN_FUNC_DISABLE: u8 = 0xff;

/* Mouse button mask values in params[0] for BTN_FUNC_MOUSE. */
const BTN_MASK_LEFT: u8 = 1 << 0;
const BTN_MASK_RIGHT: u8 = 1 << 1;
const BTN_MASK_MIDDLE: u8 = 1 << 2;
const BTN_MASK_SIDE: u8 = 1 << 3;
const BTN_MASK_EXTRA: u8 = 1 << 4;

/* Wheel direction values in params[0] for BTN_FUNC_WHEEL. */
const WHEEL_SCROLL_UP: u8 = 0;
const WHEEL_SCROLL_DOWN: u8 = 1;

/* Supported polling rates (Hz). */
const REPORT_RATES: &[u32] = &[500, 1000];

/* ------------------------------------------------------------------ */
/* Cached hardware state                                                */
/* ------------------------------------------------------------------ */

#[derive(Debug)]
struct GskillData {
    profile_count: u8,
    active_profile: u8,
    firmware_version: u8,
    /* Raw 644-byte profile reports. LED and unknown fields are preserved
     * verbatim across read/modify/write cycles — only DPI, polling rate,
     * buttons, and name are parsed/rewritten. */
    profiles: [Option<Box<[u8; GSKILL_REPORT_SIZE_PROFILE]>>; GSKILL_PROFILE_MAX],
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

/* ------------------------------------------------------------------ */
/* General command helper                                               */
/* ------------------------------------------------------------------ */

/* Send a 9-byte general command via set_feature_report (report ID 0x0C),
 * then poll via get_feature_report up to GSKILL_CMD_MAX_RETRIES times
 * with GSKILL_CMD_RETRY_MS delays. The status byte at buf[1] is checked:
 *   SUCCESS (0xB0) or blank (0x00): success
 *   IN_PROGRESS (0xB1): keep polling
 *   FAILURE (0xB2) / IDLE (0xB3) / other: error
 *
 * On success, `buf` contains the device's response. */
async fn gskill_general_cmd(io: &DeviceIo, buf: &mut [u8; GSKILL_REPORT_SIZE_CMD]) -> Result<()> {
    buf[0] = GSKILL_GENERAL_CMD;

    io.set_feature_report(buf)
        .map_err(anyhow::Error::from)?;

    for retry in 0..GSKILL_CMD_MAX_RETRIES {
        sleep(Duration::from_millis(GSKILL_CMD_RETRY_MS)).await;

        /* The get_feature_report call requires buf[0] to be the report ID.
         * Some devices return a short/blank buffer on success (rc < 9). */
        buf[0] = GSKILL_GENERAL_CMD;
        let n = io.get_feature_report(buf).map_err(anyhow::Error::from)?;

        /* Short or blank response: treat as success (C driver behavior). */
        if n < GSKILL_REPORT_SIZE_CMD {
            return Ok(());
        }

        match buf[1] {
            0x00 | GSKILL_CMD_SUCCESS => return Ok(()),
            GSKILL_CMD_IN_PROGRESS => {
                debug!("G.Skill: command in progress, retry {}", retry + 1);
                continue;
            }
            GSKILL_CMD_FAILURE => {
                anyhow::bail!("G.Skill: command failed (status FAILURE)");
            }
            status => {
                anyhow::bail!("G.Skill: unknown command status {status:#04x}");
            }
        }
    }

    anyhow::bail!(
        "G.Skill: command timed out after {} retries",
        GSKILL_CMD_MAX_RETRIES
    );
}

/* Send a general command but do NOT poll for status (set_feature_report only).
 * Used for profile/macro selection where polling breaks subsequent reads. */
fn gskill_cmd_no_poll(io: &DeviceIo, buf: &mut [u8; GSKILL_REPORT_SIZE_CMD]) -> Result<()> {
    buf[0] = GSKILL_GENERAL_CMD;
    io.set_feature_report(buf)
        .map_err(anyhow::Error::from)?;
    Ok(())
}

/* ------------------------------------------------------------------ */
/* Device queries                                                       */
/* ------------------------------------------------------------------ */

async fn gskill_get_firmware_version(io: &DeviceIo) -> Result<u8> {
    let mut buf = [0u8; GSKILL_REPORT_SIZE_CMD];
    buf[0] = GSKILL_GENERAL_CMD;
    buf[1] = 0xc4;
    buf[2] = 0x08;

    gskill_general_cmd(io, &mut buf).await?;
    Ok(buf[4])
}

async fn gskill_get_profile_count(io: &DeviceIo) -> Result<u8> {
    let mut buf = [0u8; GSKILL_REPORT_SIZE_CMD];
    buf[0] = GSKILL_GENERAL_CMD;
    buf[1] = 0xc4;
    buf[2] = 0x12;
    buf[3] = 0x00;
    buf[4] = 0x01;

    gskill_general_cmd(io, &mut buf).await?;
    let count = buf[3];
    debug!("G.Skill: profile count = {count}");
    Ok(count)
}

async fn gskill_get_active_profile(io: &DeviceIo) -> Result<u8> {
    let mut buf = [0u8; GSKILL_REPORT_SIZE_CMD];
    buf[0] = GSKILL_GENERAL_CMD;
    buf[1] = 0xc4;
    buf[2] = 0x07;
    buf[3] = 0x00;
    buf[4] = 0x01;

    gskill_general_cmd(io, &mut buf).await?;
    Ok(buf[3])
}

/* Select a profile for reading (write=false) or writing (write=true).
 * This is a fire-and-forget command — no status polling, because polling
 * would interfere with the subsequent profile read/write. */
fn gskill_select_profile(io: &DeviceIo, index: u8, write: bool) -> Result<()> {
    let mut buf = [0u8; GSKILL_REPORT_SIZE_CMD];
    buf[0] = GSKILL_GENERAL_CMD;
    buf[1] = 0xc4;
    buf[2] = 0x0c;
    buf[3] = index;
    buf[4] = if write { 1 } else { 0 };

    gskill_cmd_no_poll(io, &mut buf)
}

/* Read a single profile report (644 bytes) from the device. Retries up
 * to 3 times if the device returns the wrong profile index. */
async fn gskill_read_profile(
    io: &DeviceIo,
    index: u8,
) -> Result<Box<[u8; GSKILL_REPORT_SIZE_PROFILE]>> {
    for retry in 0..3u8 {
        gskill_select_profile(io, index, false)?;
        sleep(Duration::from_millis(100)).await;

        let mut report = Box::new([0u8; GSKILL_REPORT_SIZE_PROFILE]);
        report[0] = GSKILL_GET_SET_PROFILE;
        io.get_feature_report(report.as_mut())
            .map_err(anyhow::Error::from)?;

        /* Verify we got the right profile. */
        if report[OFF_PROFILE_NUM] == index {
            /* Verify checksum. */
            let expected = compute_checksum(report.as_ref());
            let actual = report[OFF_CHECKSUM];
            if expected != actual {
                warn!(
                    "G.Skill: profile {index} checksum mismatch (expected {expected:#04x}, got {actual:#04x})"
                );
                /* Continue anyway — the C driver logs a warning but doesn't abort. */
            }
            return Ok(report);
        }

        debug!(
            "G.Skill: profile read returned index {}, expected {} (retry {})",
            report[OFF_PROFILE_NUM],
            index,
            retry + 1
        );
    }

    anyhow::bail!("G.Skill: failed to read profile {index} after 3 retries");
}

/* Instruct the device to reload profile data after writes. */
async fn gskill_reload(io: &DeviceIo) -> Result<()> {
    let mut buf = [0u8; GSKILL_REPORT_SIZE_CMD];
    buf[0] = GSKILL_GENERAL_CMD;
    buf[1] = 0xc4;
    buf[2] = 0x00;

    debug!("G.Skill: asking device to reload profile data");
    gskill_general_cmd(io, &mut buf).await
}

/* ------------------------------------------------------------------ */
/* Profile parsing helpers                                              */
/* ------------------------------------------------------------------ */

/* Parse the polling rate from byte 4 of the profile report.
 * On x86 Linux with GCC packed bitfields, `polling_rate :4` occupies the
 * low nibble. The formula is: hz = 1000 / (raw + 1). */
fn parse_polling_rate(report: &[u8; GSKILL_REPORT_SIZE_PROFILE]) -> u32 {
    let raw = report[OFF_RATE_SNAP] & 0x0f;
    GSKILL_MAX_POLLING_RATE / (u32::from(raw) + 1)
}

/* Encode a polling rate to the 4-bit raw value.
 * raw = (1000 / hz) - 1, clamped to [0, 15]. */
fn encode_polling_rate(hz: u32) -> u8 {
    if hz == 0 {
        return 0;
    }
    let raw = (GSKILL_MAX_POLLING_RATE / hz).saturating_sub(1);
    (raw as u8) & 0x0f
}

/* Parse DPI level info from the profile report.
 * Byte 7: current_dpi_level (low nibble), dpi_num (high nibble).
 * DPI levels at bytes 8-17: 5 x (x, y) pairs. */
fn parse_dpi(report: &[u8; GSKILL_REPORT_SIZE_PROFILE]) -> (u8, u8, Vec<(u32, u32)>) {
    let info = report[OFF_DPI_INFO];
    let current = info & 0x0f;
    let count = (info >> 4) & 0x0f;

    let mut levels = Vec::with_capacity(count as usize);
    for i in 0..(count as usize).min(GSKILL_NUM_DPI) {
        let x = u32::from(report[OFF_DPI_LEVELS + i * 2]) * GSKILL_DPI_UNIT;
        let y = u32::from(report[OFF_DPI_LEVELS + i * 2 + 1]) * GSKILL_DPI_UNIT;
        levels.push((x, y));
    }

    (current, count, levels)
}

/* Parse a single button config (5 bytes) from the profile report.
 * Returns (ActionType, mapping_value). */
fn parse_button(report: &[u8; GSKILL_REPORT_SIZE_PROFILE], btn_idx: usize) -> (ActionType, u32) {
    if btn_idx >= GSKILL_BUTTON_MAX {
        return (ActionType::None, 0);
    }

    let offset = OFF_BTN_CFGS + btn_idx * 5;
    let func_type = report[offset];
    let params = &report[offset + 1..offset + 5];

    match func_type {
        BTN_FUNC_WHEEL => {
            let special = if params[0] == WHEEL_SCROLL_UP {
                special_action::WHEEL_UP
            } else {
                special_action::WHEEL_DOWN
            };
            (ActionType::Special, special)
        }
        BTN_FUNC_MOUSE => {
            let button = match params[0] {
                BTN_MASK_LEFT => 1,
                BTN_MASK_RIGHT => 3,
                BTN_MASK_MIDDLE => 2,
                BTN_MASK_SIDE => 15,
                BTN_MASK_EXTRA => 14,
                _ => 0,
            };
            (ActionType::Button, button)
        }
        BTN_FUNC_KBD => {
            /* params[0] = modifier_mask, params[1] = hid_code */
            (ActionType::Key, u32::from(params[1]))
        }
        BTN_FUNC_CONSUMER => {
            let code = u16::from_le_bytes([params[0], params[1]]);
            (ActionType::Key, u32::from(code))
        }
        BTN_FUNC_MACRO => (ActionType::Macro, 0),
        BTN_FUNC_DPI_UP => (ActionType::Special, special_action::RESOLUTION_UP),
        BTN_FUNC_DPI_DOWN => (ActionType::Special, special_action::RESOLUTION_DOWN),
        BTN_FUNC_CYCLE_DPI_UP => (ActionType::Special, special_action::RESOLUTION_CYCLE_UP),
        BTN_FUNC_CYCLE_PROFILE_UP => (ActionType::Special, special_action::PROFILE_CYCLE_UP),
        BTN_FUNC_CYCLE_PROFILE_DOWN => (ActionType::Special, special_action::PROFILE_DOWN),
        BTN_FUNC_DISABLE => (ActionType::None, 0),
        _ => {
            debug!("G.Skill: unknown button function type {func_type:#04x}");
            (ActionType::Unknown, u32::from(func_type))
        }
    }
}

/* Encode a button action back into the 5-byte hardware format. */
fn encode_button(
    action_type: ActionType,
    value: u32,
    report: &mut [u8; GSKILL_REPORT_SIZE_PROFILE],
    btn_idx: usize,
) {
    if btn_idx >= GSKILL_BUTTON_MAX {
        return;
    }

    let offset = OFF_BTN_CFGS + btn_idx * 5;
    /* Clear previous config. */
    report[offset..offset + 5].fill(0);

    match action_type {
        ActionType::Button => {
            report[offset] = BTN_FUNC_MOUSE;
            report[offset + 1] = match value {
                1 => BTN_MASK_LEFT,
                3 => BTN_MASK_RIGHT,
                2 => BTN_MASK_MIDDLE,
                15 => BTN_MASK_SIDE,
                14 => BTN_MASK_EXTRA,
                _ => BTN_MASK_LEFT,
            };
        }
        ActionType::Special => match value {
            special_action::WHEEL_UP => {
                report[offset] = BTN_FUNC_WHEEL;
                report[offset + 1] = WHEEL_SCROLL_UP;
            }
            special_action::WHEEL_DOWN => {
                report[offset] = BTN_FUNC_WHEEL;
                report[offset + 1] = WHEEL_SCROLL_DOWN;
            }
            special_action::RESOLUTION_UP => {
                report[offset] = BTN_FUNC_DPI_UP;
            }
            special_action::RESOLUTION_DOWN => {
                report[offset] = BTN_FUNC_DPI_DOWN;
            }
            special_action::RESOLUTION_CYCLE_UP => {
                report[offset] = BTN_FUNC_CYCLE_DPI_UP;
            }
            special_action::PROFILE_CYCLE_UP => {
                report[offset] = BTN_FUNC_CYCLE_PROFILE_UP;
            }
            special_action::PROFILE_DOWN => {
                report[offset] = BTN_FUNC_CYCLE_PROFILE_DOWN;
            }
            _ => {
                report[offset] = BTN_FUNC_DISABLE;
            }
        },
        ActionType::Key => {
            /* Use keyboard HID code encoding. */
            report[offset] = BTN_FUNC_KBD;
            report[offset + 2] = value as u8;
        }
        ActionType::Macro => {
            report[offset] = BTN_FUNC_MACRO;
        }
        ActionType::None | ActionType::Unknown => {
            report[offset] = BTN_FUNC_DISABLE;
        }
    }
}

/* Decode a UTF-16LE profile name from raw bytes. */
fn decode_utf16le_name(data: &[u8]) -> String {
    let u16s: Vec<u16> = data
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&u16s)
}

/* Encode a profile name as UTF-16LE into a fixed-size buffer. */
fn encode_utf16le_name(name: &str, buf: &mut [u8]) {
    buf.fill(0);
    let mut pos = 0;
    for ch in name.encode_utf16() {
        if pos + 2 > buf.len() {
            break;
        }
        let bytes = ch.to_le_bytes();
        buf[pos] = bytes[0];
        buf[pos + 1] = bytes[1];
        pos += 2;
    }
}

/* ------------------------------------------------------------------ */
/* Checksum                                                             */
/* ------------------------------------------------------------------ */

/* Compute the one-byte checksum expected at GSKILL_CHECKSUM_OFFSET.
 *
 * The checksum covers bytes (GSKILL_CHECKSUM_OFFSET + 1)..end of the
 * report. The algorithm sums every byte in that range with wrapping
 * arithmetic and returns the two's-complement negation of the result. */
pub fn compute_checksum(report: &[u8]) -> u8 {
    let sum = report[GSKILL_CHECKSUM_OFFSET + 1..]
        .iter()
        .fold(0u8, |acc, &b| acc.wrapping_add(b));
    (!sum).wrapping_add(1)
}

/* ------------------------------------------------------------------ */
/* DPI conversion (matching the C driver: raw * 50, NOT (raw+1)*50)     */
/* ------------------------------------------------------------------ */

/* Convert a raw DPI byte to actual DPI. C: dpi = raw * GSKILL_DPI_UNIT. */
fn raw_to_dpi(raw: u8) -> u32 {
    u32::from(raw) * GSKILL_DPI_UNIT
}

/* Encode a DPI value to the 1-byte hardware representation.
 * C: raw = dpi / GSKILL_DPI_UNIT. */
fn dpi_to_raw(dpi: u32) -> Option<u8> {
    if dpi < GSKILL_MIN_DPI || dpi > GSKILL_MAX_DPI || dpi % GSKILL_DPI_UNIT != 0 {
        return None;
    }
    u8::try_from(dpi / GSKILL_DPI_UNIT).ok()
}

/* ------------------------------------------------------------------ */
/* DeviceDriver implementation                                          */
/* ------------------------------------------------------------------ */

#[async_trait]
impl DeviceDriver for GskillDriver {
    fn name(&self) -> &str {
        "G.Skill"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        /* Query firmware version to confirm device presence. */
        let fw = gskill_get_firmware_version(io).await?;
        debug!("G.Skill: firmware version = {fw}");

        /* Get profile count (how many profiles the device currently has enabled). */
        let profile_count = gskill_get_profile_count(io).await?;

        /* Get active profile index. */
        let active_profile = gskill_get_active_profile(io).await?;
        debug!("G.Skill: active profile = {active_profile}");

        /* Read all profiles. */
        let mut profiles: [Option<Box<[u8; GSKILL_REPORT_SIZE_PROFILE]>>; GSKILL_PROFILE_MAX] =
            Default::default();

        for i in 0..GSKILL_PROFILE_MAX {
            match gskill_read_profile(io, i as u8).await {
                Ok(report) => {
                    profiles[i] = Some(report);
                }
                Err(e) => {
                    /* Profiles beyond the enabled count may fail to read;
                     * this is expected and not fatal. */
                    if (i as u8) < profile_count {
                        warn!("G.Skill: failed to read profile {i}: {e}");
                    }
                }
            }
        }

        self.data = Some(GskillData {
            profile_count,
            active_profile,
            firmware_version: fw,
            profiles,
        });

        Ok(())
    }

    async fn load_profiles(&mut self, _io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("G.Skill: probe() was not called"))?;

        info.firmware_version = format!("v{}", data.firmware_version);

        /* DPI range for all profiles. */
        let dpi_list: Vec<u32> = (GSKILL_MIN_DPI..=GSKILL_MAX_DPI)
            .step_by(GSKILL_DPI_UNIT as usize)
            .collect();

        for profile in &mut info.profiles {
            let idx = profile.index as usize;
            profile.report_rates = REPORT_RATES.to_vec();

            /* Mark profiles beyond the enabled count as disabled. */
            if idx >= data.profile_count as usize {
                profile.is_enabled = false;
                continue;
            }

            let report = match &data.profiles[idx] {
                Some(r) => r,
                None => continue,
            };

            profile.is_active = idx == data.active_profile as usize;

            /* Parse polling rate. */
            profile.report_rate = parse_polling_rate(report);

            /* Parse DPI levels. */
            let (current_dpi, _dpi_count, levels) = parse_dpi(report);

            for (ri, res) in profile.resolutions.iter_mut().enumerate() {
                if ri < levels.len() {
                    let (x, y) = levels[ri];
                    res.dpi = if x == y {
                        Dpi::Unified(x)
                    } else {
                        Dpi::Separate { x, y }
                    };
                    res.is_active = ri == current_dpi as usize;
                    res.is_disabled = false;
                } else {
                    res.is_disabled = true;
                }
                res.dpi_list = dpi_list.clone();
            }

            /* Parse profile name. */
            let name_bytes = &report[OFF_NAME..OFF_NAME + NAME_SIZE];
            let name = decode_utf16le_name(name_bytes);
            if !name.is_empty() {
                profile.name = name;
                debug!("G.Skill: profile {idx} name: \"{}\"", profile.name);
            }

            /* Parse buttons. */
            for button in &mut profile.buttons {
                let bi = button.index as usize;
                let (action_type, mapping_value) = parse_button(report, bi);
                button.action_type = action_type;
                button.mapping_value = mapping_value;
            }
        }

        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let data = self
            .data
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("G.Skill: probe() was not called"))?;

        let mut need_reload = false;

        for profile in &info.profiles {
            if !profile.is_dirty || !profile.is_enabled {
                continue;
            }

            let idx = profile.index as usize;
            if idx >= GSKILL_PROFILE_MAX {
                continue;
            }

            let report = match &mut data.profiles[idx] {
                Some(r) => r,
                None => continue,
            };

            debug!("G.Skill: committing profile {idx}");
            need_reload = true;

            /* Update polling rate. */
            let rate_raw = encode_polling_rate(profile.report_rate);
            report[OFF_RATE_SNAP] = (report[OFF_RATE_SNAP] & 0xf0) | rate_raw;

            /* Update DPI levels. */
            let mut dpi_count: u8 = 0;
            let mut current_dpi: u8 = 0;
            for res in &profile.resolutions {
                if res.is_disabled {
                    continue;
                }
                let ri = dpi_count as usize;
                if ri >= GSKILL_NUM_DPI {
                    break;
                }

                let (x, y) = match res.dpi {
                    Dpi::Unified(d) => (d, d),
                    Dpi::Separate { x, y } => (x, y),
                    Dpi::Unknown => continue,
                };

                let rx = dpi_to_raw(x).unwrap_or(0);
                let ry = dpi_to_raw(y).unwrap_or(0);
                report[OFF_DPI_LEVELS + ri * 2] = rx;
                report[OFF_DPI_LEVELS + ri * 2 + 1] = ry;

                if res.is_active {
                    current_dpi = dpi_count;
                }
                dpi_count += 1;
            }
            report[OFF_DPI_INFO] = (current_dpi & 0x0f) | ((dpi_count & 0x0f) << 4);

            /* Update buttons. */
            for button in &profile.buttons {
                encode_button(
                    button.action_type,
                    button.mapping_value,
                    report,
                    button.index as usize,
                );
            }

            /* Update profile name. */
            if !profile.name.is_empty() {
                encode_utf16le_name(
                    &profile.name,
                    &mut report[OFF_NAME..OFF_NAME + NAME_SIZE],
                );
            } else {
                /* G.Skill software doesn't handle blank names; default to something. */
                let default_name = format!("Ratbag profile {}", report[OFF_PROFILE_NUM]);
                encode_utf16le_name(
                    &default_name,
                    &mut report[OFF_NAME..OFF_NAME + NAME_SIZE],
                );
            }

            /* Recompute checksum. */
            report[OFF_CHECKSUM] = compute_checksum(report.as_ref());

            /* Select profile for writing, wait, then write 644B report. */
            gskill_select_profile(io, idx as u8, true)?;
            sleep(Duration::from_millis(200)).await;

            io.set_feature_report(report.as_ref())
                .map_err(anyhow::Error::from)?;
        }

        /* Ask the device to reload if any profiles were written. */
        if need_reload {
            gskill_reload(io).await?;
        }

        Ok(())
    }
}

/* ------------------------------------------------------------------ */
/* Tests                                                                */
/* ------------------------------------------------------------------ */

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_twos_complement() {
        /* Verify checksum is two's complement negation of byte sum. */
        let mut report = [0u8; 20];
        /* Fill bytes after checksum offset with known values. */
        report[4] = 0x10;
        report[5] = 0x20;
        report[6] = 0x30;
        /* Sum of bytes 4..20 = 0x10+0x20+0x30 = 0x60 */
        /* Two's complement: ~0x60 + 1 = 0x9F + 1 = 0xA0 */
        let cs = compute_checksum(&report);
        assert_eq!(cs, 0xA0);

        /* Verify: sum of (checksum + payload) should be 0x00 mod 256. */
        let total = report[GSKILL_CHECKSUM_OFFSET + 1..]
            .iter()
            .fold(cs, |acc, &b| acc.wrapping_add(b));
        assert_eq!(total, 0);
    }

    #[test]
    fn dpi_roundtrip() {
        /* C formula: dpi = raw * 50, raw = dpi / 50. */
        assert_eq!(raw_to_dpi(2), 100);
        assert_eq!(raw_to_dpi(164), 8200);
        assert_eq!(dpi_to_raw(100), Some(2));
        assert_eq!(dpi_to_raw(8200), Some(164));

        /* Roundtrip for all valid DPI values. */
        for dpi in (GSKILL_MIN_DPI..=GSKILL_MAX_DPI).step_by(GSKILL_DPI_UNIT as usize) {
            let raw = dpi_to_raw(dpi).expect("valid DPI should encode");
            assert_eq!(raw_to_dpi(raw), dpi, "roundtrip failed for dpi={dpi}");
        }
    }

    #[test]
    fn dpi_invalid_values() {
        /* Below minimum. */
        assert_eq!(dpi_to_raw(0), None);
        assert_eq!(dpi_to_raw(49), None);
        /* Above maximum. */
        assert_eq!(dpi_to_raw(8250), None);
        /* Not aligned to step. */
        assert_eq!(dpi_to_raw(125), None);
    }

    #[test]
    fn polling_rate_roundtrip() {
        /* raw=0 → 1000 Hz, raw=1 → 500 Hz. */
        let mut report = [0u8; GSKILL_REPORT_SIZE_PROFILE];
        report[OFF_RATE_SNAP] = 0x00;
        assert_eq!(parse_polling_rate(&report), 1000);

        report[OFF_RATE_SNAP] = 0x01;
        assert_eq!(parse_polling_rate(&report), 500);

        /* Encode roundtrip. */
        assert_eq!(encode_polling_rate(1000), 0);
        assert_eq!(encode_polling_rate(500), 1);
    }

    #[test]
    fn dpi_info_parsing() {
        let mut report = [0u8; GSKILL_REPORT_SIZE_PROFILE];
        /* current_dpi=2 (low nibble), dpi_num=3 (high nibble) */
        report[OFF_DPI_INFO] = 0x32;
        /* 3 DPI levels: (100,100), (200,200), (400,400) */
        report[OFF_DPI_LEVELS] = 2;     /* x=2*50=100 */
        report[OFF_DPI_LEVELS + 1] = 2; /* y=2*50=100 */
        report[OFF_DPI_LEVELS + 2] = 4; /* x=4*50=200 */
        report[OFF_DPI_LEVELS + 3] = 4; /* y=4*50=200 */
        report[OFF_DPI_LEVELS + 4] = 8; /* x=8*50=400 */
        report[OFF_DPI_LEVELS + 5] = 8; /* y=8*50=400 */

        let (current, count, levels) = parse_dpi(&report);
        assert_eq!(current, 2);
        assert_eq!(count, 3);
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], (100, 100));
        assert_eq!(levels[1], (200, 200));
        assert_eq!(levels[2], (400, 400));
    }

    #[test]
    fn button_action_mapping() {
        let mut report = [0u8; GSKILL_REPORT_SIZE_PROFILE];

        /* Mouse left click: func=0x01, params[0]=0x01 (LEFT mask). */
        report[OFF_BTN_CFGS] = BTN_FUNC_MOUSE;
        report[OFF_BTN_CFGS + 1] = BTN_MASK_LEFT;
        let (at, val) = parse_button(&report, 0);
        assert_eq!(at, ActionType::Button);
        assert_eq!(val, 1);

        /* Wheel up: func=0x00, params[0]=0x00 (SCROLL_UP). */
        report[OFF_BTN_CFGS + 5] = BTN_FUNC_WHEEL;
        report[OFF_BTN_CFGS + 6] = WHEEL_SCROLL_UP;
        let (at, val) = parse_button(&report, 1);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::WHEEL_UP);

        /* DPI up: func=0x09. */
        report[OFF_BTN_CFGS + 10] = BTN_FUNC_DPI_UP;
        let (at, val) = parse_button(&report, 2);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::RESOLUTION_UP);

        /* Disable: func=0xFF. */
        report[OFF_BTN_CFGS + 15] = BTN_FUNC_DISABLE;
        let (at, _) = parse_button(&report, 3);
        assert_eq!(at, ActionType::None);
    }

    #[test]
    fn button_encode_roundtrip() {
        let mut report = [0u8; GSKILL_REPORT_SIZE_PROFILE];

        /* Encode mouse right click, then parse back. */
        encode_button(ActionType::Button, 3, &mut report, 0);
        let (at, val) = parse_button(&report, 0);
        assert_eq!(at, ActionType::Button);
        assert_eq!(val, 3);

        /* Encode wheel down, then parse back. */
        encode_button(ActionType::Special, special_action::WHEEL_DOWN, &mut report, 1);
        let (at, val) = parse_button(&report, 1);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::WHEEL_DOWN);

        /* Encode DPI cycle up, then parse back. */
        encode_button(ActionType::Special, special_action::RESOLUTION_CYCLE_UP, &mut report, 2);
        let (at, val) = parse_button(&report, 2);
        assert_eq!(at, ActionType::Special);
        assert_eq!(val, special_action::RESOLUTION_CYCLE_UP);
    }

    #[test]
    fn utf16le_name_roundtrip() {
        let name = "Test Profile 1";
        let mut buf = [0u8; NAME_SIZE];
        encode_utf16le_name(name, &mut buf);
        let decoded = decode_utf16le_name(&buf);
        assert_eq!(decoded, name);
    }

    #[test]
    fn utf16le_empty_name() {
        let buf = [0u8; NAME_SIZE];
        let decoded = decode_utf16le_name(&buf);
        assert_eq!(decoded, "");
    }
}
