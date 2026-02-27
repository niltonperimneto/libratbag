/* ASUS ROG gaming mouse driver. */
/*  */
/* Implements the ASUS proprietary HID protocol used by ROG gaming mice. */
/* The protocol uses 64-byte raw HID output/input reports with a simple */
/* command/response pattern: write a 64-byte request, read a 64-byte reply. */
/*  */
/* Protocol overview: */
/*   Request  = [u16 cmd_le] ++ [u8 params[62]] */
/*   Response = [u16 code_le] ++ [u8 results[62]] */
/*  */
/* A response code of 0xAAFF indicates an error (device sleeping or */
/* in an invalid state); all other non-zero codes are treated as success. */

use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tracing::{debug, info, warn};

use crate::device::{ActionType, Color, DeviceInfo, Dpi, LedMode, ProfileInfo, RgbColor};
use crate::driver::DeviceIo;

/* -------------------------------------------------------------------------- */
/* Protocol constants                                                          */
/* -------------------------------------------------------------------------- */

/* Size of every ASUS HID report (both request and response). */
const PACKET_SIZE: usize = 64;

/* Offset within the response packet where result bytes start (after 2-byte code). */
const RESULTS_OFFSET: usize = 2;

/* Timeout waiting for a single read attempt. */
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/* Number of send-and-wait cycles before giving up. */
const MAX_ATTEMPTS: u8 = 3;

/* ASUS command codes (little-endian u16 in bytes [0..2] of every request). */
const CMD_GET_LED_DATA: u16 = 0x0312;
const CMD_GET_SETTINGS: u16 = 0x0412;
const CMD_GET_BUTTON_DATA: u16 = 0x0512;
const CMD_GET_PROFILE_DATA: u16 = 0x0012;
const CMD_SET_LED: u16 = 0x2851;
const CMD_SET_SETTING: u16 = 0x3151;
const CMD_SET_BUTTON: u16 = 0x2151;
const CMD_SET_PROFILE: u16 = 0x0250;
const CMD_SAVE: u16 = 0x0350;

/* Response code returned when the device is in an invalid state. */
const STATUS_ERROR: u16 = 0xaaff;

/* On-wire action type for keyboard-key bindings. */
const ACTION_TYPE_KEY: u8 = 0;
/* On-wire action type for mouse-button / special-action bindings. */
const ACTION_TYPE_BUTTON: u8 = 1;
/* Sentinel ASUS code meaning "button is disabled". */
const BUTTON_CODE_DISABLED: u8 = 0xff;

/* Field offsets within GET_SETTINGS params (added to the DPI count to address */
/* extra fields that come after the DPI presets in the response). */
const FIELD_RATE: u8 = 0;
const FIELD_RESPONSE: u8 = 1;
const FIELD_SNAPPING: u8 = 2;

/* -------------------------------------------------------------------------- */
/* Hardware lookup tables                                                      */
/* -------------------------------------------------------------------------- */

/* Supported polling rates in Hz, indexed by the on-wire rate index. */
const POLLING_RATES: [u32; 4] = [125, 250, 500, 1000];

/* Supported debounce / button-response times in ms, indexed by on-wire index. */
const DEBOUNCE_TIMES: [u32; 8] = [4, 8, 12, 16, 20, 24, 28, 32];

/* Number of distinct LED effect modes the ASUS protocol supports. */
const LED_MODE_COUNT: usize = 7;

/* ASUS key code → Linux input key code mapping. */
/* Index is the ASUS/USB-HID usage code; value is the Linux input code */
/* (from linux/input-event-codes.h). 0 means "not mapped". */
#[rustfmt::skip]
const KEY_MAPPING: [u32; 99] = [
    /*0x00*/  0,   0,   0,   0,   /* 0x00-0x03: unmapped */
    /*0x04*/ 30,  48,  46,  32,   /* a b c d */
    /*0x08*/ 18,  33,  34,  35,   /* e f g h */
    /*0x0C*/ 23,  36,  37,  38,   /* i j k l */
    /*0x10*/ 50,  49,  24,  25,   /* m n o p */
    /*0x14*/ 16,  19,  31,  20,   /* q r s t */
    /*0x18*/ 22,  47,  17,  45,   /* u v w x */
    /*0x1C*/ 21,  44,   2,   3,   /* y z 1 2 */
    /*0x20*/  4,   5,   6,   7,   /* 3 4 5 6 */
    /*0x24*/  8,   9,  10,  11,   /* 7 8 9 0 */
    /*0x28*/ 28,   1,  14,  15,   /* enter esc backspace tab */
    /*0x2C*/ 57,  12,  78,   0,   /* space minus kp_plus unmapped */
    /*0x30*/  0,   0,   0,   0,   /* 0x30-0x33: unmapped */
    /*0x34*/  0,  41,  13,   0,   /* unmapped grave equal unmapped */
    /*0x38*/ 53,   0,  59,  60,   /* slash unmapped F1 F2 */
    /*0x3C*/ 61,  62,  63,  64,   /* F3 F4 F5 F6 */
    /*0x40*/ 65,  66,  67,  68,   /* F7 F8 F9 F10 */
    /*0x44*/ 87,  88,   0,   0,   /* F11 F12 unmapped unmapped */
    /*0x48*/  0,   0, 102, 104,   /* unmapped unmapped home pageup */
    /*0x4C*/111,   0, 109, 106,   /* delete unmapped pagedown right */
    /*0x50*/105, 108, 103,   0,   /* left down up unmapped */
    /*0x54*/  0,   0,   0,   0,   /* 0x54-0x57: unmapped */
    /*0x58*/  0,  79,  80,  81,   /* unmapped kp1 kp2 kp3 */
    /*0x5C*/ 75,  76,  77,  71,   /* kp4 kp5 kp6 kp7 */
    /*0x60*/ 72,  73,   0,        /* kp8 kp9 unmapped */
];

/* A mapping from an ASUS physical-button code to a ratbag action. */
struct AsusButtonEntry {
    asus_code: u8,
    action_type: ActionType,
    /* Button number for ActionType::Button; special-action index for Special. */
    mapping_value: u32,
}

/* All known ASUS button codes and their default ratbag actions. */
const BUTTON_MAPPING: &[AsusButtonEntry] = &[
    AsusButtonEntry { asus_code: 0xf0, action_type: ActionType::Button,  mapping_value: 1 }, /* left */
    AsusButtonEntry { asus_code: 0xf1, action_type: ActionType::Button,  mapping_value: 2 }, /* right */
    AsusButtonEntry { asus_code: 0xf2, action_type: ActionType::Button,  mapping_value: 3 }, /* middle */
    AsusButtonEntry { asus_code: 0xe8, action_type: ActionType::Special, mapping_value: 1 }, /* wheel up */
    AsusButtonEntry { asus_code: 0xe9, action_type: ActionType::Special, mapping_value: 2 }, /* wheel down */
    AsusButtonEntry { asus_code: 0xe6, action_type: ActionType::Special, mapping_value: 3 }, /* DPI cycle */
    AsusButtonEntry { asus_code: 0xe4, action_type: ActionType::Button,  mapping_value: 4 }, /* backward (left side) */
    AsusButtonEntry { asus_code: 0xe5, action_type: ActionType::Button,  mapping_value: 5 }, /* forward  (left side) */
    AsusButtonEntry { asus_code: 0xe1, action_type: ActionType::Button,  mapping_value: 4 }, /* backward (right side) */
    AsusButtonEntry { asus_code: 0xe2, action_type: ActionType::Button,  mapping_value: 5 }, /* forward  (right side) */
    AsusButtonEntry { asus_code: 0xe7, action_type: ActionType::Special, mapping_value: 4 }, /* DPI target */
];

/* Physical ASUS button codes for the default 8-button layout, indexed by */
/* ratbag button index. Matches the C driver's ASUS_CONFIG_BUTTON_MAPPING. */
const DEFAULT_BUTTON_CODES: [u8; 8] = [
    0xf0, /* index 0: left click */
    0xf1, /* index 1: right click */
    0xf2, /* index 2: middle click */
    0xe4, /* index 3: backward */
    0xe5, /* index 4: forward */
    0xe6, /* index 5: DPI cycle */
    0xe8, /* index 6: wheel up */
    0xe9, /* index 7: wheel down */
];

/* Default LED mode table: ASUS on-wire mode index → LedMode. */
/* Index 0 = Static/On, 1 = Breathing, 2 = Color Cycle, 3-6 = device-specific. */
const DEFAULT_LED_MODES: [LedMode; LED_MODE_COUNT] = [
    LedMode::Solid,     /* 0: static colour */
    LedMode::Breathing, /* 1: breathing */
    LedMode::Cycle,     /* 2: colour cycle */
    LedMode::Solid,     /* 3: rainbow wave (approximated) */
    LedMode::Solid,     /* 4: reactive */
    LedMode::Solid,     /* 5: custom */
    LedMode::Solid,     /* 6: battery indicator */
];

/* -------------------------------------------------------------------------- */
/* Internal data structures                                                    */
/* -------------------------------------------------------------------------- */

/* Firmware and active-profile information from GET_PROFILE_DATA. */
#[derive(Debug, Default)]
struct DeviceProfileData {
    profile_id: u8,
    /* None if no DPI preset is active; Some(i) for preset index i. */
    dpi_preset: Option<u8>,
    /* Primary firmware version [major, minor, build]. */
    fw_primary: [u8; 3],
    /* Secondary (wireless receiver) firmware version [major, minor, build]. */
    fw_secondary: [u8; 3],
}

/* -------------------------------------------------------------------------- */
/* Driver state                                                                */
/* -------------------------------------------------------------------------- */

pub struct AsusDriver {
    /* Number of DPI presets this device exposes (2 or 4). */
    dpi_count: usize,
    /* True when the device reports X and Y DPI values independently. */
    sep_xy_dpi: bool,
    /* True when each LED requires a separate GET_LED_DATA query. */
    sep_leds: bool,
    /* True when LED brightness is a raw 0-255 value rather than a 0-4 index. */
    raw_brightness: bool,
    /* True when the device returns DPI values at half their actual frequency */
    /* (i.e. the reported value must be doubled). */
    double_dpi: bool,
    /* True when the device uses the Strix-style profile-ID byte offset. */
    strix_profile: bool,
    /* Physical ASUS button code for each ratbag button index. */
    button_codes: Vec<u8>,
    /* On-wire LED mode index → ratbag LedMode translation table. */
    led_modes: [LedMode; LED_MODE_COUNT],
}

impl AsusDriver {
    pub fn new() -> Self {
        Self {
            dpi_count: 2,
            sep_xy_dpi: false,
            sep_leds: false,
            raw_brightness: false,
            double_dpi: false,
            strix_profile: false,
            button_codes: DEFAULT_BUTTON_CODES.to_vec(),
            led_modes: DEFAULT_LED_MODES,
        }
    }

    /* ---------------------------------------------------------------------- */
    /* Low-level I/O                                                           */
    /* ---------------------------------------------------------------------- */

    /* Build a 64-byte ASUS request and send it; return the 64-byte response. */
    /*  */
    /* The first two bytes of the request are `cmd` in little-endian order. */
    /* Up to 62 bytes from `params` follow; the remainder is zero-padded. */
    /* Retries up to MAX_ATTEMPTS times before returning an error. */
    async fn query(
        &self,
        io: &mut DeviceIo,
        cmd: u16,
        params: &[u8],
    ) -> Result<[u8; PACKET_SIZE]> {
        let mut request = [0u8; PACKET_SIZE];
        let [lo, hi] = cmd.to_le_bytes();
        request[0] = lo;
        request[1] = hi;
        let copy_len = params.len().min(PACKET_SIZE - 2);
        request[2..2 + copy_len].copy_from_slice(&params[..copy_len]);

        for attempt in 1..=MAX_ATTEMPTS {
            io.write_report(&request)
                .await
                .context("ASUS write failed")?;

            let mut response = [0u8; PACKET_SIZE];
            match tokio::time::timeout(READ_TIMEOUT, io.read_report(&mut response)).await {
                Ok(Ok(n)) if n >= 2 => {
                    let code = u16::from_le_bytes([response[0], response[1]]);
                    if code == STATUS_ERROR {
                        bail!("ASUS device returned error status (sleeping or disconnected)");
                    }
                    return Ok(response);
                }
                Ok(Ok(n)) => {
                    warn!("ASUS: response too short on attempt {attempt}: {n} bytes");
                }
                Ok(Err(e)) => {
                    warn!("ASUS: read error on attempt {attempt}: {e}");
                }
                Err(_elapsed) => {
                    debug!("ASUS: read timeout on attempt {attempt}");
                }
            }
        }

        bail!(
            "ASUS: no valid response after {} attempts (cmd=0x{cmd:04X})",
            MAX_ATTEMPTS
        );
    }

    /* ---------------------------------------------------------------------- */
    /* Device commands                                                         */
    /* ---------------------------------------------------------------------- */

    async fn get_profile_data(&self, io: &mut DeviceIo) -> Result<DeviceProfileData> {
        let resp = self.query(io, CMD_GET_PROFILE_DATA, &[]).await?;
        let r = &resp[RESULTS_OFFSET..];

        let profile_id = if self.strix_profile { r[7] } else { r[8] };
        let dpi_preset = if r[9] != 0 { Some(r[9] - 1) } else { None };

        Ok(DeviceProfileData {
            profile_id,
            dpi_preset,
            fw_primary: [r[13], r[12], r[11]],
            fw_secondary: [r[4], r[3], r[2]],
        })
    }

    async fn set_profile(&self, io: &mut DeviceIo, index: u8) -> Result<()> {
        self.query(io, CMD_SET_PROFILE, &[index]).await?;
        Ok(())
    }

    async fn save_profile(&self, io: &mut DeviceIo) -> Result<()> {
        self.query(io, CMD_SAVE, &[]).await?;
        Ok(())
    }

    async fn get_button_data(&self, io: &mut DeviceIo, group: u8) -> Result<[u8; PACKET_SIZE]> {
        self.query(io, CMD_GET_BUTTON_DATA, &[group]).await
    }

    /* `sep_xy`: pass `true` to retrieve separate X/Y DPI values (uses param 2 */
    /* instead of 0 in the ASUS protocol). */
    async fn get_settings(&self, io: &mut DeviceIo, sep_xy: bool) -> Result<[u8; PACKET_SIZE]> {
        self.query(io, CMD_GET_SETTINGS, &[if sep_xy { 2 } else { 0 }])
            .await
    }

    async fn get_led_data(&self, io: &mut DeviceIo, led: u8) -> Result<[u8; PACKET_SIZE]> {
        self.query(io, CMD_GET_LED_DATA, &[led]).await
    }

    async fn set_button_action(
        &self,
        io: &mut DeviceIo,
        src: u8,
        dst: u8,
        dst_type: u8,
    ) -> Result<()> {
        let params = [0u8, 0, src, ACTION_TYPE_BUTTON, dst, dst_type];
        self.query(io, CMD_SET_BUTTON, &params).await?;
        Ok(())
    }

    async fn set_dpi(&self, io: &mut DeviceIo, preset_index: u8, dpi: u32) -> Result<()> {
        let actual = if self.double_dpi { dpi / 2 } else { dpi };
        let raw = ((actual.saturating_sub(50)) / 50) as u8;
        self.query(io, CMD_SET_SETTING, &[preset_index, 0, raw])
            .await?;
        Ok(())
    }

    async fn set_polling_rate(&self, io: &mut DeviceIo, hz: u32) -> Result<()> {
        let field = self.dpi_count as u8 + FIELD_RATE;
        let idx = POLLING_RATES
            .iter()
            .position(|&r| r == hz)
            .unwrap_or(POLLING_RATES.len() - 1) as u8;
        self.query(io, CMD_SET_SETTING, &[field, 0, idx]).await?;
        Ok(())
    }

    async fn set_debounce(&self, io: &mut DeviceIo, ms: u32) -> Result<()> {
        let field = self.dpi_count as u8 + FIELD_RESPONSE;
        let idx = DEBOUNCE_TIMES
            .iter()
            .position(|&t| t == ms)
            .unwrap_or(0) as u8;
        self.query(io, CMD_SET_SETTING, &[field, 0, idx]).await?;
        Ok(())
    }

    async fn set_angle_snapping(&self, io: &mut DeviceIo, enabled: bool) -> Result<()> {
        let field = self.dpi_count as u8 + FIELD_SNAPPING;
        self.query(io, CMD_SET_SETTING, &[field, 0, enabled as u8])
            .await?;
        Ok(())
    }

    async fn set_led(
        &self,
        io: &mut DeviceIo,
        index: u8,
        asus_mode: u8,
        brightness: u8,
        color: RgbColor,
    ) -> Result<()> {
        let params = [index, 0, asus_mode, brightness, color.r, color.g, color.b];
        self.query(io, CMD_SET_LED, &params).await?;
        Ok(())
    }

    /* ---------------------------------------------------------------------- */
    /* Packet parsing helpers (pure functions, testable without I/O)          */
    /* ---------------------------------------------------------------------- */

    /* Apply a GET_BUTTON_DATA response to the buttons in `profile`. */
    /*  */
    /* The binding table starts at results[4], with each entry being 2 bytes: */
    /* [asus_action_code, asus_action_type]. */
    fn apply_button_data(profile: &mut ProfileInfo, data: &[u8; PACKET_SIZE]) {
        let r = &data[RESULTS_OFFSET..]; /* 62 result bytes */
        for button in &mut profile.buttons {
            let i = button.index as usize;
            let offset = 4 + i * 2;
            if offset + 1 >= r.len() {
                break;
            }

            let asus_action = r[offset];
            let asus_type = r[offset + 1];

            if asus_action == BUTTON_CODE_DISABLED {
                button.action_type = ActionType::None;
                continue;
            }

            match asus_type {
                ACTION_TYPE_KEY => {
                    if let Some(&linux_key) = KEY_MAPPING.get(asus_action as usize) {
                        if linux_key > 0 {
                            button.action_type = ActionType::Key;
                            button.mapping_value = linux_key;
                        }
                    }
                }
                ACTION_TYPE_BUTTON => {
                    if let Some(entry) =
                        BUTTON_MAPPING.iter().find(|e| e.asus_code == asus_action)
                    {
                        button.action_type = entry.action_type;
                        button.mapping_value = entry.mapping_value;
                    } else {
                        debug!("ASUS: unknown button code 0x{asus_action:02x}");
                    }
                }
                _ => {
                    debug!("ASUS: unknown action type 0x{asus_type:02x}");
                }
            }
        }
    }

    /* Apply a GET_SETTINGS response to the resolutions / extra settings in `profile`. */
    /*  */
    /* DPI values are stored as u16-LE at results[4 + i*2] for each preset i. */
    /* After the DPI array come: rate, response, and snapping (each u16-LE). */
    /* On-wire DPI → actual DPI: `raw * 50 + 50` (then `*2` if double_dpi). */
    fn apply_settings_data(
        &self,
        profile: &mut ProfileInfo,
        data: &[u8; PACKET_SIZE],
        dpi_preset: Option<u8>,
    ) {
        let r = &data[RESULTS_OFFSET..];
        let n = self.dpi_count.min(4);

        /* Parse each DPI preset. */
        for res in &mut profile.resolutions {
            let i = res.index as usize;
            if i >= n {
                break;
            }
            let off = 4 + i * 2;
            if off + 2 > r.len() {
                break;
            }
            let raw = u16::from_le_bytes([r[off], r[off + 1]]) as u32;
            let base = raw * 50 + 50;
            let dpi = if self.double_dpi { base * 2 } else { base };
            res.dpi = Dpi::Unified(dpi);
            res.is_active = match dpi_preset {
                Some(p) => p as usize == i,
                None => {
                    if i == 0 {
                        debug!("ASUS: no active DPI preset reported by device, defaulting to preset 0");
                    }
                    i == 0
                }
            };
        }

        /* Parse extra settings that follow the DPI array. */
        let rate_off = 4 + n * 2;
        if rate_off + 2 <= r.len() {
            let idx = u16::from_le_bytes([r[rate_off], r[rate_off + 1]]) as usize;
            if let Some(&rate) = POLLING_RATES.get(idx) {
                profile.report_rate = rate;
            }
        }

        let resp_off = rate_off + 2;
        if resp_off + 2 <= r.len() {
            let idx = u16::from_le_bytes([r[resp_off], r[resp_off + 1]]) as usize;
            if let Some(&ms) = DEBOUNCE_TIMES.get(idx) {
                profile.debounce = ms as i32;
            }
        }

        let snap_off = resp_off + 2;
        if snap_off + 2 <= r.len() {
            let snap = u16::from_le_bytes([r[snap_off], r[snap_off + 1]]);
            profile.angle_snapping = snap as i32;
        }
    }

    /* Apply separate-X/Y DPI data to `profile.resolutions`. */
    /*  */
    /* Each preset is stored as two consecutive u16-LE values (x then y) */
    /* starting at results[4]. */
    fn apply_xy_settings_data(&self, profile: &mut ProfileInfo, data: &[u8; PACKET_SIZE]) {
        let r = &data[RESULTS_OFFSET..];
        let n = self.dpi_count.min(4);
        for res in &mut profile.resolutions {
            let i = res.index as usize;
            if i >= n {
                break;
            }
            let off = 4 + i * 4;
            if off + 4 > r.len() {
                break;
            }
            let raw_x = u16::from_le_bytes([r[off], r[off + 1]]) as u32;
            let raw_y = u16::from_le_bytes([r[off + 2], r[off + 3]]) as u32;
            let (base_x, base_y) = (raw_x * 50 + 50, raw_y * 50 + 50);
            let (x, y) = if self.double_dpi {
                (base_x * 2, base_y * 2)
            } else {
                (base_x, base_y)
            };
            res.dpi = Dpi::Separate { x, y };
        }
    }

    /* Apply LED data from a GET_LED_DATA response to `profile.leds`. */
    /*  */
    /* When `sep_leds` is false the response contains all LEDs packed together: */
    /* LED struct at results[4 + index * 5] (mode, brightness, r, g, b). */
    /* When `sep_leds` is true each response contains a single LED at results[4]. */
    fn apply_led_data(
        &self,
        profile: &mut ProfileInfo,
        data: &[u8; PACKET_SIZE],
        data_led_index: u32, /* which LED index within the data (0 for sep_leds) */
    ) {
        let r = &data[RESULTS_OFFSET..];
        for led in &mut profile.leds {
            let slot = if self.sep_leds {
                /* In separate-LED mode every response is for a single LED. */
                if led.index != data_led_index {
                    continue;
                }
                0
            } else {
                led.index as usize
            };

            let off = 4 + slot * 5;
            if off + 5 > r.len() {
                break;
            }

            let asus_mode = r[off];
            let raw_brightness = r[off + 1];

            led.mode = self
                .led_modes
                .get(asus_mode as usize)
                .copied()
                .unwrap_or(LedMode::Solid);

            led.brightness = if self.raw_brightness {
                u32::from(raw_brightness)
            } else {
                /* Convert 0-4 ASUS scale to 0-255. */
                u32::from(raw_brightness) * 64
            };

            led.color = Color::from_rgb(RgbColor {
                r: r[off + 2],
                g: r[off + 3],
                b: r[off + 4],
            });
        }
    }

    /* ---------------------------------------------------------------------- */
    /* Profile I/O                                                             */
    /* ---------------------------------------------------------------------- */

    async fn load_one_profile(
        &mut self,
        io: &mut DeviceIo,
        profile: &mut ProfileInfo,
        dpi_preset: Option<u8>,
    ) -> Result<()> {
        /* Buttons */
        debug!("ASUS: loading buttons for profile {}", profile.index);
        match self.get_button_data(io, 0).await {
            Ok(data) => Self::apply_button_data(profile, &data),
            Err(e) => warn!("ASUS: failed to load buttons: {e}"),
        }

        /* DPI and extra settings */
        debug!("ASUS: loading settings for profile {}", profile.index);
        match self.get_settings(io, false).await {
            Ok(data) => self.apply_settings_data(profile, &data, dpi_preset),
            Err(e) => warn!("ASUS: failed to load settings: {e}"),
        }

        /* Separate X/Y DPI overlay (if the device supports it) */
        if self.sep_xy_dpi {
            match self.get_settings(io, true).await {
                Ok(data) => self.apply_xy_settings_data(profile, &data),
                Err(e) => warn!("ASUS: failed to load XY DPI data: {e}"),
            }
        }

        /* LEDs */
        if profile.leds.is_empty() {
            return Ok(());
        }

        debug!("ASUS: loading LEDs for profile {}", profile.index);
        if self.sep_leds {
            /* Each LED is queried individually. */
            let led_count = profile.leds.len();
            let mut led_data_buf = vec![[0u8; PACKET_SIZE]; led_count];
            for (i, buf) in led_data_buf.iter_mut().enumerate() {
                match self.get_led_data(io, i as u8).await {
                    Ok(d) => *buf = d,
                    Err(e) => warn!("ASUS: failed to load LED {i}: {e}"),
                }
            }
            for (i, buf) in led_data_buf.iter().enumerate() {
                self.apply_led_data(profile, buf, i as u32);
            }
        } else {
            match self.get_led_data(io, 0).await {
                Ok(data) => self.apply_led_data(profile, &data, 0),
                Err(e) => warn!("ASUS: failed to load LEDs: {e}"),
            }
        }

        Ok(())
    }

    async fn commit_one_profile(
        &mut self,
        io: &mut DeviceIo,
        profile: &ProfileInfo,
    ) -> Result<()> {
        /* Buttons */
        for button in &profile.buttons {
            let Some(&src_code) = self.button_codes.get(button.index as usize) else {
                continue;
            };

            let result = match button.action_type {
                ActionType::None => {
                    self.set_button_action(io, src_code, BUTTON_CODE_DISABLED, ACTION_TYPE_BUTTON)
                        .await
                }
                ActionType::Key => {
                    let linux_key = button.mapping_value;
                    if let Some(asus_code) =
                        KEY_MAPPING.iter().position(|&k| k == linux_key && k != 0)
                    {
                        self.set_button_action(
                            io,
                            src_code,
                            asus_code as u8,
                            ACTION_TYPE_KEY,
                        )
                        .await
                    } else {
                        warn!(
                            "ASUS: no ASUS key code for Linux key {} on button {}",
                            linux_key, button.index
                        );
                        continue;
                    }
                }
                ActionType::Button | ActionType::Special => {
                    if let Some(entry) = BUTTON_MAPPING.iter().find(|e| {
                        e.action_type == button.action_type
                            && e.mapping_value == button.mapping_value
                    }) {
                        self.set_button_action(
                            io,
                            src_code,
                            entry.asus_code,
                            ACTION_TYPE_BUTTON,
                        )
                        .await
                    } else {
                        debug!(
                            "ASUS: no ASUS code for action {:?}/{} on button {}",
                            button.action_type, button.mapping_value, button.index
                        );
                        continue;
                    }
                }
                _ => continue,
            };

            if let Err(e) = result {
                warn!("ASUS: failed to commit button {}: {e}", button.index);
            }
        }

        /* DPI presets */
        for res in &profile.resolutions {
            let dpi_value = match res.dpi {
                Dpi::Unified(v) => v,
                Dpi::Separate { x, .. } => x,
            };
            if let Err(e) = self.set_dpi(io, res.index as u8, dpi_value).await {
                warn!("ASUS: failed to set DPI for preset {}: {e}", res.index);
            }
        }

        /* Polling rate */
        if profile.report_rate > 0 {
            if let Err(e) = self.set_polling_rate(io, profile.report_rate).await {
                warn!("ASUS: failed to set polling rate: {e}");
            }
        }

        /* Debounce */
        if profile.debounce > 0 {
            if let Err(e) = self.set_debounce(io, profile.debounce as u32).await {
                warn!("ASUS: failed to set debounce: {e}");
            }
        }

        /* Angle snapping */
        if profile.angle_snapping >= 0 {
            if let Err(e) = self
                .set_angle_snapping(io, profile.angle_snapping != 0)
                .await
            {
                warn!("ASUS: failed to set angle snapping: {e}");
            }
        }

        /* LEDs */
        for led in &profile.leds {
            let asus_mode = self
                .led_modes
                .iter()
                .position(|&m| m == led.mode)
                .unwrap_or(0) as u8;

            let brightness = if self.raw_brightness {
                led.brightness.min(255) as u8
            } else {
                /* Convert 0-255 to 0-4 ASUS scale using nearest-integer rounding, */
                /* matching the C driver's `round(brightness / 64.0)` behaviour. */
                ((led.brightness.min(255) + 32) / 64).min(4) as u8
            };

            let rgb = led.color.to_rgb();
            if let Err(e) = self
                .set_led(io, led.index as u8, asus_mode, brightness, rgb)
                .await
            {
                warn!("ASUS: failed to set LED {}: {e}", led.index);
            }
        }

        Ok(())
    }
}

/* -------------------------------------------------------------------------- */
/* DeviceDriver implementation                                                 */
/* -------------------------------------------------------------------------- */

#[async_trait]
impl super::DeviceDriver for AsusDriver {
    fn name(&self) -> &str {
        "ASUS ROG"
    }

    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()> {
        let data = self
            .get_profile_data(io)
            .await
            .context("ASUS probe: failed to read profile data")?;

        info!(
            "ASUS device detected: profile={}, FW={:02X}.{:02X}.{:02X}",
            data.profile_id, data.fw_primary[0], data.fw_primary[1], data.fw_primary[2],
        );
        Ok(())
    }

    async fn load_profiles(&mut self, io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()> {
        let pd = self.get_profile_data(io).await?;
        let initial_id = pd.profile_id;
        let num_profiles = info.profiles.len();

        debug!(
            "ASUS: loading {} profile(s), active = {}",
            num_profiles, initial_id
        );

        let indices: Vec<u32> = info.profiles.iter().map(|p| p.index).collect();

        for &idx in &indices {
            /* Switch to the profile when the device has multiple profiles. */
            if num_profiles > 1 && idx != u32::from(initial_id) {
                if let Err(e) = self.set_profile(io, idx as u8).await {
                    warn!("ASUS: failed to switch to profile {idx}: {e}");
                    continue;
                }
            }

            /* Clone, populate, then write back to avoid simultaneous borrows. */
            let mut profile = info
                .profiles
                .iter()
                .find(|p| p.index == idx)
                .cloned()
                .expect("profile index must exist");

            profile.is_active = idx == u32::from(initial_id);

            if let Err(e) = self
                .load_one_profile(io, &mut profile, pd.dpi_preset)
                .await
            {
                warn!("ASUS: failed to load profile {idx}: {e}");
            }

            if let Some(dest) = info.profiles.iter_mut().find(|p| p.index == idx) {
                *dest = profile;
            }
        }

        /* Restore the device to the profile that was active when we started. */
        if num_profiles > 1 {
            if let Err(e) = self.set_profile(io, initial_id).await {
                warn!("ASUS: failed to restore profile {initial_id}: {e}");
            }
        }

        debug!("ASUS: profiles loaded");
        Ok(())
    }

    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()> {
        let num_profiles = info.profiles.len();
        let initial_id = if num_profiles > 1 {
            match self.get_profile_data(io).await {
                Ok(d) => d.profile_id,
                Err(e) => {
                    warn!("ASUS: failed to read current profile before commit: {e}");
                    0
                }
            }
        } else {
            0
        };

        for profile in &info.profiles {
            if !profile.is_dirty {
                continue;
            }

            debug!("ASUS: committing profile {}", profile.index);

            if num_profiles > 1 && profile.index != u32::from(initial_id) {
                if let Err(e) = self.set_profile(io, profile.index as u8).await {
                    warn!("ASUS: failed to switch to profile {} for commit: {e}", profile.index);
                    continue;
                }
            }

            if let Err(e) = self.commit_one_profile(io, profile).await {
                warn!("ASUS: failed to commit profile {}: {e}", profile.index);
            }

            if let Err(e) = self.save_profile(io).await {
                warn!("ASUS: failed to save profile {}: {e}", profile.index);
            }
        }

        /* Restore the active profile. */
        if num_profiles > 1 {
            if let Err(e) = self.set_profile(io, initial_id).await {
                warn!("ASUS: failed to restore profile {initial_id} after commit: {e}");
            }
        }

        Ok(())
    }
}

/* -------------------------------------------------------------------------- */
/* Unit tests                                                                  */
/* -------------------------------------------------------------------------- */

#[cfg(test)]
mod tests {
    use super::*;

    /* ------------------------------------------------------------------ */
    /* Key mapping                                                         */
    /* ------------------------------------------------------------------ */

    #[test]
    fn key_mapping_a_through_z() {
        /* ASUS code 0x04 = 'a', Linux code 30 */
        assert_eq!(KEY_MAPPING[0x04], 30);
        /* ASUS code 0x1C = 'y', Linux code 21 */
        assert_eq!(KEY_MAPPING[0x1C], 21);
        /* ASUS code 0x1D = 'z', Linux code 44 */
        assert_eq!(KEY_MAPPING[0x1D], 44);
    }

    #[test]
    fn key_mapping_digits() {
        /* ASUS 0x1E = '1', Linux 2 */
        assert_eq!(KEY_MAPPING[0x1E], 2);
        /* ASUS 0x27 = '0', Linux 11 */
        assert_eq!(KEY_MAPPING[0x27], 11);
    }

    #[test]
    fn key_mapping_function_keys() {
        assert_eq!(KEY_MAPPING[0x3A], 59); /* F1 */
        assert_eq!(KEY_MAPPING[0x45], 88); /* F12 */
    }

    #[test]
    fn key_mapping_navigation() {
        assert_eq!(KEY_MAPPING[0x4A], 102); /* Home */
        assert_eq!(KEY_MAPPING[0x4B], 104); /* PageUp */
        assert_eq!(KEY_MAPPING[0x4C], 111); /* Delete */
        assert_eq!(KEY_MAPPING[0x4E], 109); /* PageDown */
        assert_eq!(KEY_MAPPING[0x4F], 106); /* Right */
        assert_eq!(KEY_MAPPING[0x50], 105); /* Left */
        assert_eq!(KEY_MAPPING[0x51], 108); /* Down */
        assert_eq!(KEY_MAPPING[0x52], 103); /* Up */
    }

    #[test]
    fn key_mapping_numpad() {
        assert_eq!(KEY_MAPPING[0x59], 79); /* KP1 */
        assert_eq!(KEY_MAPPING[0x60], 72); /* KP8 */
        assert_eq!(KEY_MAPPING[0x61], 73); /* KP9 */
    }

    #[test]
    fn key_mapping_unmapped_codes() {
        assert_eq!(KEY_MAPPING[0x00], 0);
        assert_eq!(KEY_MAPPING[0x01], 0);
        assert_eq!(KEY_MAPPING[0x62], 0);
    }

    /* ------------------------------------------------------------------ */
    /* Button data parsing                                                 */
    /* ------------------------------------------------------------------ */

    fn make_profile_with_buttons(n: u32) -> ProfileInfo {
        ProfileInfo {
            index: 0,
            name: String::new(),
            is_active: true,
            is_enabled: true,
            is_dirty: false,
            report_rate: 1000,
            report_rates: vec![1000],
            angle_snapping: -1,
            debounce: -1,
            debounces: Vec::new(),
            resolutions: Vec::new(),
            leds: Vec::new(),
            buttons: (0..n)
                .map(|i| crate::device::ButtonInfo {
                    index: i,
                    action_type: ActionType::Unknown,
                    action_types: Vec::new(),
                    mapping_value: 0,
                    macro_entries: Vec::new(),
                })
                .collect(),
        }
    }

    #[test]
    fn button_data_left_click() {
        /* Simulate a response where button 0 is bound to ASUS code 0xf0 (left). */
        let mut data = [0u8; PACKET_SIZE];
        /* results[4] = action code, results[5] = type (BUTTON) */
        data[RESULTS_OFFSET + 4] = 0xf0; /* asus_action */
        data[RESULTS_OFFSET + 5] = ACTION_TYPE_BUTTON; /* asus_type */

        let mut profile = make_profile_with_buttons(1);
        AsusDriver::apply_button_data(&mut profile, &data);

        assert_eq!(profile.buttons[0].action_type, ActionType::Button);
        assert_eq!(profile.buttons[0].mapping_value, 1);
    }

    #[test]
    fn button_data_disabled() {
        let mut data = [0u8; PACKET_SIZE];
        data[RESULTS_OFFSET + 4] = BUTTON_CODE_DISABLED;
        data[RESULTS_OFFSET + 5] = ACTION_TYPE_BUTTON;

        let mut profile = make_profile_with_buttons(1);
        AsusDriver::apply_button_data(&mut profile, &data);

        assert_eq!(profile.buttons[0].action_type, ActionType::None);
    }

    #[test]
    fn button_data_keyboard_key() {
        /* ASUS key code 0x04 = 'a' = Linux key 30 */
        let mut data = [0u8; PACKET_SIZE];
        data[RESULTS_OFFSET + 4] = 0x04;
        data[RESULTS_OFFSET + 5] = ACTION_TYPE_KEY;

        let mut profile = make_profile_with_buttons(1);
        AsusDriver::apply_button_data(&mut profile, &data);

        assert_eq!(profile.buttons[0].action_type, ActionType::Key);
        assert_eq!(profile.buttons[0].mapping_value, 30); /* KEY_A */
    }

    #[test]
    fn button_data_multiple_buttons() {
        /* buttons: [left=0xf0, right=0xf1, disabled] */
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        data[base] = 0xf0;
        data[base + 1] = ACTION_TYPE_BUTTON;
        data[base + 2] = 0xf1;
        data[base + 3] = ACTION_TYPE_BUTTON;
        data[base + 4] = BUTTON_CODE_DISABLED;
        data[base + 5] = ACTION_TYPE_BUTTON;

        let mut profile = make_profile_with_buttons(3);
        AsusDriver::apply_button_data(&mut profile, &data);

        assert_eq!(profile.buttons[0].action_type, ActionType::Button);
        assert_eq!(profile.buttons[0].mapping_value, 1);
        assert_eq!(profile.buttons[1].action_type, ActionType::Button);
        assert_eq!(profile.buttons[1].mapping_value, 2);
        assert_eq!(profile.buttons[2].action_type, ActionType::None);
    }

    /* ------------------------------------------------------------------ */
    /* Settings / DPI parsing                                              */
    /* ------------------------------------------------------------------ */

    fn make_profile_with_resolutions(n: u32) -> ProfileInfo {
        ProfileInfo {
            index: 0,
            name: String::new(),
            is_active: true,
            is_enabled: true,
            is_dirty: false,
            report_rate: 0,
            report_rates: Vec::new(),
            angle_snapping: -1,
            debounce: -1,
            debounces: Vec::new(),
            resolutions: (0..n)
                .map(|i| crate::device::ResolutionInfo {
                    index: i,
                    dpi: Dpi::Unified(0),
                    dpi_list: Vec::new(),
                    capabilities: Vec::new(),
                    is_active: false,
                    is_default: false,
                    is_disabled: false,
                })
                .collect(),
            buttons: Vec::new(),
            leds: Vec::new(),
        }
    }

    #[test]
    fn settings_2dpi_basic() {
        /* raw DPI 0: 0 → 0*50+50 = 50 DPI */
        /* raw DPI 1: 31 → 31*50+50 = 1600 DPI */
        /* rate index 3 → 1000 Hz */
        /* response index 0 → 4 ms */
        /* snapping = 1 */
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        /* dpi[0] = 0 */
        data[base] = 0;
        data[base + 1] = 0;
        /* dpi[1] = 31 */
        data[base + 2] = 31;
        data[base + 3] = 0;
        /* rate = 3 (1000 Hz) */
        data[base + 4] = 3;
        data[base + 5] = 0;
        /* response = 0 (4 ms) */
        data[base + 6] = 0;
        data[base + 7] = 0;
        /* snapping = 1 */
        data[base + 8] = 1;
        data[base + 9] = 0;

        let driver = AsusDriver::new(); /* dpi_count = 2 by default */
        let mut profile = make_profile_with_resolutions(2);
        driver.apply_settings_data(&mut profile, &data, None);

        assert_eq!(profile.resolutions[0].dpi, Dpi::Unified(50));
        assert_eq!(profile.resolutions[1].dpi, Dpi::Unified(1600));
        assert_eq!(profile.report_rate, 1000);
        assert_eq!(profile.debounce, 4);
        assert_eq!(profile.angle_snapping, 1);
        /* default active preset is index 0 */
        assert!(profile.resolutions[0].is_active);
        assert!(!profile.resolutions[1].is_active);
    }

    #[test]
    fn settings_dpi_preset_override() {
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        data[base + 2] = 10; /* dpi[1] = 10 → 550 DPI */

        let driver = AsusDriver::new();
        let mut profile = make_profile_with_resolutions(2);
        /* Explicitly set active preset to index 1 */
        driver.apply_settings_data(&mut profile, &data, Some(1));

        assert!(!profile.resolutions[0].is_active);
        assert!(profile.resolutions[1].is_active);
    }

    #[test]
    fn settings_double_dpi_quirk() {
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        /* raw = 10 → without quirk: 550 DPI; with double_dpi: 550*2 = 1100 DPI */
        data[base] = 10;

        let mut driver = AsusDriver::new();
        driver.double_dpi = true;
        let mut profile = make_profile_with_resolutions(1);
        driver.apply_settings_data(&mut profile, &data, None);

        assert_eq!(profile.resolutions[0].dpi, Dpi::Unified(1100));
    }

    #[test]
    fn settings_4dpi_polling_rate() {
        /* 4-DPI device: rate field is at results[4 + 4*2] = results[12] */
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        data[base + 8] = 2; /* rate index 2 → 500 Hz */

        let mut driver = AsusDriver::new();
        driver.dpi_count = 4;
        let mut profile = make_profile_with_resolutions(4);
        driver.apply_settings_data(&mut profile, &data, None);

        assert_eq!(profile.report_rate, 500);
    }

    /* ------------------------------------------------------------------ */
    /* LED data parsing                                                    */
    /* ------------------------------------------------------------------ */

    fn make_profile_with_leds(n: u32) -> ProfileInfo {
        ProfileInfo {
            index: 0,
            name: String::new(),
            is_active: true,
            is_enabled: true,
            is_dirty: false,
            report_rate: 1000,
            report_rates: Vec::new(),
            angle_snapping: -1,
            debounce: -1,
            debounces: Vec::new(),
            resolutions: Vec::new(),
            buttons: Vec::new(),
            leds: (0..n)
                .map(|i| crate::device::LedInfo {
                    index: i,
                    mode: LedMode::Off,
                    modes: Vec::new(),
                    color: Color::default(),
                    secondary_color: Color::default(),
                    tertiary_color: Color::default(),
                    color_depth: 1,
                    effect_duration: 0,
                    brightness: 0,
                })
                .collect(),
        }
    }

    #[test]
    fn led_data_solid_mode() {
        /* LED 0: mode=0 (Solid), brightness=2, R=255, G=128, B=64 */
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        data[base] = 0; /* mode index 0 → Solid */
        data[base + 1] = 2; /* brightness raw */
        data[base + 2] = 255; /* R */
        data[base + 3] = 128; /* G */
        data[base + 4] = 64; /* B */

        let driver = AsusDriver::new();
        let mut profile = make_profile_with_leds(1);
        driver.apply_led_data(&mut profile, &data, 0);

        assert_eq!(profile.leds[0].mode, LedMode::Solid);
        /* brightness: 2 * 64 = 128 */
        assert_eq!(profile.leds[0].brightness, 128);
        let rgb = profile.leds[0].color.to_rgb();
        assert_eq!(rgb.r, 255);
        assert_eq!(rgb.g, 128);
        assert_eq!(rgb.b, 64);
    }

    #[test]
    fn led_data_breathing_mode() {
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        data[base] = 1; /* mode index 1 → Breathing */
        data[base + 1] = 4; /* brightness = 4 → 4*64 = 256 */

        let driver = AsusDriver::new();
        let mut profile = make_profile_with_leds(1);
        driver.apply_led_data(&mut profile, &data, 0);

        assert_eq!(profile.leds[0].mode, LedMode::Breathing);
        assert_eq!(profile.leds[0].brightness, 256);
    }

    #[test]
    fn led_data_cycle_mode() {
        let mut data = [0u8; PACKET_SIZE];
        data[RESULTS_OFFSET + 4] = 2; /* mode index 2 → Cycle */

        let driver = AsusDriver::new();
        let mut profile = make_profile_with_leds(1);
        driver.apply_led_data(&mut profile, &data, 0);

        assert_eq!(profile.leds[0].mode, LedMode::Cycle);
    }

    #[test]
    fn led_data_multiple_leds() {
        /* Two LEDs, non-sep_leds mode */
        let mut data = [0u8; PACKET_SIZE];
        let base = RESULTS_OFFSET + 4;
        /* LED 0 */
        data[base] = 0;
        data[base + 1] = 1;
        data[base + 2] = 10;
        data[base + 3] = 20;
        data[base + 4] = 30;
        /* LED 1 */
        data[base + 5] = 1;
        data[base + 6] = 2;
        data[base + 7] = 40;
        data[base + 8] = 50;
        data[base + 9] = 60;

        let driver = AsusDriver::new();
        let mut profile = make_profile_with_leds(2);
        driver.apply_led_data(&mut profile, &data, 0);

        assert_eq!(profile.leds[0].mode, LedMode::Solid);
        assert_eq!(profile.leds[0].brightness, 64);
        assert_eq!(profile.leds[0].color.to_rgb(), RgbColor { r: 10, g: 20, b: 30 });
        assert_eq!(profile.leds[1].mode, LedMode::Breathing);
        assert_eq!(profile.leds[1].brightness, 128);
        assert_eq!(profile.leds[1].color.to_rgb(), RgbColor { r: 40, g: 50, b: 60 });
    }

    #[test]
    fn led_data_raw_brightness() {
        let mut data = [0u8; PACKET_SIZE];
        data[RESULTS_OFFSET + 4] = 0;
        data[RESULTS_OFFSET + 5] = 200; /* raw brightness */

        let mut driver = AsusDriver::new();
        driver.raw_brightness = true;
        let mut profile = make_profile_with_leds(1);
        driver.apply_led_data(&mut profile, &data, 0);

        /* raw_brightness: value passed through as-is */
        assert_eq!(profile.leds[0].brightness, 200);
    }

    /* ------------------------------------------------------------------ */
    /* Request packet building                                             */
    /* ------------------------------------------------------------------ */

    #[test]
    fn query_builds_correct_request() {
        /* Verify the first few bytes of a well-formed 64-byte ASUS request. */
        let mut request = [0u8; PACKET_SIZE];
        let cmd: u16 = CMD_GET_PROFILE_DATA;
        let [lo, hi] = cmd.to_le_bytes();
        request[0] = lo;
        request[1] = hi;
        /* No params for GET_PROFILE_DATA */

        assert_eq!(request[0], 0x12);
        assert_eq!(request[1], 0x00);
        assert_eq!(request[2], 0x00);
        assert_eq!(request.len(), PACKET_SIZE);
    }

    #[test]
    fn query_params_truncated_to_packet_size() {
        /* Params longer than 62 bytes must be truncated. */
        let cmd: u16 = 0x1234;
        let params = [0xABu8; 100];

        let mut request = [0u8; PACKET_SIZE];
        let [lo, hi] = cmd.to_le_bytes();
        request[0] = lo;
        request[1] = hi;
        let copy_len = params.len().min(PACKET_SIZE - 2);
        request[2..2 + copy_len].copy_from_slice(&params[..copy_len]);

        assert_eq!(copy_len, 62);
        assert_eq!(request[2], 0xAB);
        assert_eq!(request[63], 0xAB);
    }

    #[test]
    fn status_error_detection() {
        let response = [0xFF_u8, 0xAA, 0, 0, 0, 0, 0, 0]; /* 0xAAFF LE */
        let code = u16::from_le_bytes([response[0], response[1]]);
        assert_eq!(code, STATUS_ERROR);
    }

    #[test]
    fn dpi_encoding_roundtrip() {
        /* raw = (dpi - 50) / 50 */
        for &dpi in &[50u32, 100, 800, 1600, 12000] {
            let raw = ((dpi - 50) / 50) as u8;
            let decoded = raw as u32 * 50 + 50;
            assert_eq!(decoded, dpi, "DPI roundtrip failed for {dpi}");
        }
    }

    #[test]
    fn polling_rate_lookup() {
        assert_eq!(POLLING_RATES.iter().position(|&r| r == 1000), Some(3));
        assert_eq!(POLLING_RATES.iter().position(|&r| r == 125), Some(0));
        assert_eq!(POLLING_RATES.iter().position(|&r| r == 999), None);
    }

    #[test]
    fn debounce_lookup() {
        assert_eq!(DEBOUNCE_TIMES.iter().position(|&t| t == 4), Some(0));
        assert_eq!(DEBOUNCE_TIMES.iter().position(|&t| t == 32), Some(7));
    }
}
