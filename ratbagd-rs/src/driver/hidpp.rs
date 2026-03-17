/* Shared HID++ protocol definitions for both 1.0 and 2.0. */
/*  */
/* HID++ uses two report formats: */
/* - Short (Report ID 0x10): 7 bytes total */
/* - Long  (Report ID 0x11): 20 bytes total */

/* HID++ report IDs */
pub const REPORT_ID_SHORT: u8 = 0x10;
pub const REPORT_ID_LONG: u8 = 0x11;

/* HID++ error sub-IDs */
pub const HIDPP10_ERROR: u8 = 0x8F;
pub const HIDPP20_ERROR: u8 = 0xFF;

/* Well-known device indices.                                      */
/*                                                                 */
/* Logitech HID++ addresses devices by index:                      */
/* - 0xFF: The device itself when directly connected (wired/BT).   */
/* - 0x01–0x06: Paired devices on a Unifying / Lightspeed / Bolt   */
/*   receiver.  0x01 is the first (and usually only) paired slot.  */
pub const DEVICE_IDX_CORDED: u8 = 0xFF;
pub const DEVICE_IDX_RECEIVER: u8 = 0x01;

/* HID++ 2.0 feature pages */
pub const PAGE_DEVICE_NAME: u16 = 0x0005;
pub const PAGE_SPECIAL_KEYS_BUTTONS: u16 = 0x1B04;
pub const PAGE_ADJUSTABLE_DPI: u16 = 0x2201;
pub const PAGE_ADJUSTABLE_REPORT_RATE: u16 = 0x8060;
pub const PAGE_COLOR_LED_EFFECTS: u16 = 0x8070;
pub const PAGE_RGB_EFFECTS: u16 = 0x8071;
pub const PAGE_ONBOARD_PROFILES: u16 = 0x8100;

/* Computes Logitech's variant of CRC-CCITT (polynomial 0x1021, seed 0xFFFF). */
pub fn compute_ccitt_crc(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;

    for &byte in data {
        let temp = (crc >> 8) ^ u16::from(byte);
        crc <<= 8;
        let mut quick = temp ^ (temp >> 4);
        crc ^= quick;
        quick <<= 5;
        crc ^= quick;
        quick <<= 7;
        crc ^= quick;
    }

    crc
}

/* Root feature index — always fixed at 0x00 */
pub const ROOT_FEATURE_INDEX: u8 = 0x00;

/* Root feature function IDs */
pub const ROOT_FN_GET_FEATURE: u8 = 0x00;
pub const ROOT_FN_GET_PROTOCOL_VERSION: u8 = 0x01;

/* -------------------------------------------------------------------------- */
/* LED protocol constants (from C library hidpp20.h)                          */
/* -------------------------------------------------------------------------- */

/* HID++ 2.0 LED hardware mode bytes (hidpp20_led_mode / hidpp20_color_led_zone_effect) */
pub const LED_HW_MODE_OFF: u8 = 0x00;
pub const LED_HW_MODE_FIXED: u8 = 0x01;
pub const LED_HW_MODE_CYCLE: u8 = 0x03;
pub const LED_HW_MODE_COLOR_WAVE: u8 = 0x04;
pub const LED_HW_MODE_STARLIGHT: u8 = 0x05;
pub const LED_HW_MODE_BREATHING: u8 = 0x0A;

/* Size of the internal LED payload as defined in C struct hidpp20_internal_led. */
pub const LED_PAYLOAD_SIZE: usize = 11;

/* Build the 11-byte LED payload matching the C `struct hidpp20_internal_led`. */
/*  */
/* The byte layout for each mode is:                                          */
/* Off:       [0x00, 0..10 zero]                                              */
/* Solid:     [0x01, R, G, B, 0x00, 0..6 zero]                               */
/* Cycle:     [0x03, 0..5 zero, period_hi, period_lo, brightness, 0..2 zero]  */
/* ColorWave: [0x04, 0..5 zero, period_hi, period_lo, brightness, 0..2 zero]  */
/* Starlight: [0x05, sky_R, sky_G, sky_B, star_R, star_G, star_B, 0..4 zero]  */
/* Breathing: [0x0A, R, G, B, period_hi, period_lo, waveform, brightness, 0..3]*/
pub fn build_led_payload(led: &crate::device::LedInfo) -> [u8; LED_PAYLOAD_SIZE] {
    use crate::device::LedMode;

    let mut payload = [0u8; LED_PAYLOAD_SIZE];
    let rgb = led.color.to_rgb();
    let period = (led.effect_duration as u16).to_be_bytes();
    let brightness = (led.brightness.min(255) * 100 / 255) as u8;

    match led.mode {
        LedMode::Off => {
            payload[0] = LED_HW_MODE_OFF;
        }
        LedMode::Solid => {
            /* Solid mode has no brightness byte in the protocol.
             * Apply brightness by scaling RGB values directly. */
            let br = led.brightness.min(255);
            payload[0] = LED_HW_MODE_FIXED;
            payload[1] = (u32::from(rgb.r) * br / 255) as u8;
            payload[2] = (u32::from(rgb.g) * br / 255) as u8;
            payload[3] = (u32::from(rgb.b) * br / 255) as u8;
        }
        LedMode::Cycle => {
            payload[0] = LED_HW_MODE_CYCLE;
            payload[6] = period[0];
            payload[7] = period[1];
            payload[8] = brightness;
        }
        LedMode::ColorWave => {
            payload[0] = LED_HW_MODE_COLOR_WAVE;
            payload[6] = period[0];
            payload[7] = period[1];
            payload[8] = brightness;
        }
        LedMode::Starlight => {
            /* Starlight has no brightness byte — scale RGB. */
            let br = led.brightness.min(255);
            let star = led.secondary_color.to_rgb();
            payload[0] = LED_HW_MODE_STARLIGHT;
            payload[1] = (u32::from(rgb.r) * br / 255) as u8;
            payload[2] = (u32::from(rgb.g) * br / 255) as u8;
            payload[3] = (u32::from(rgb.b) * br / 255) as u8;
            payload[4] = (u32::from(star.r) * br / 255) as u8;
            payload[5] = (u32::from(star.g) * br / 255) as u8;
            payload[6] = (u32::from(star.b) * br / 255) as u8;
        }
        LedMode::Breathing => {
            payload[0] = LED_HW_MODE_BREATHING;
            payload[1] = rgb.r;
            payload[2] = rgb.g;
            payload[3] = rgb.b;
            payload[4] = period[0];
            payload[5] = period[1];
            /* waveform defaults to 0x00 (default sine) */
            payload[7] = brightness;
        }
        LedMode::TriColor => {
            /* TriColor has no brightness byte — scale all 3 zones. */
            let br = led.brightness.min(255);
            let center = led.secondary_color.to_rgb();
            let right = led.tertiary_color.to_rgb();
            payload[0] = LED_HW_MODE_FIXED;
            payload[1] = (u32::from(rgb.r) * br / 255) as u8;
            payload[2] = (u32::from(rgb.g) * br / 255) as u8;
            payload[3] = (u32::from(rgb.b) * br / 255) as u8;
            payload[4] = (u32::from(center.r) * br / 255) as u8;
            payload[5] = (u32::from(center.g) * br / 255) as u8;
            payload[6] = (u32::from(center.b) * br / 255) as u8;
            payload[7] = (u32::from(right.r) * br / 255) as u8;
            payload[8] = (u32::from(right.g) * br / 255) as u8;
            payload[9] = (u32::from(right.b) * br / 255) as u8;
        }
    }

    payload
}

/* -------------------------------------------------------------------------- */
/* LED payload deserialization (shared by live 0x8070 and EEPROM parsing)     */
/* -------------------------------------------------------------------------- */

/* Decoded LED state from an 11-byte HID++ 2.0 LED payload.
 * Used by both the live feature read (0x8070 getZoneEffect) and EEPROM
 * profile sector parsing, which store identical byte layouts. */
#[derive(Debug, Clone)]
pub struct DecodedLedState {
    pub mode: crate::device::LedMode,
    pub color: crate::device::Color,
    pub secondary_color: crate::device::Color,
    pub effect_duration: u32,
    pub brightness: u32,
}

/* Parse an 11-byte LED payload into a `DecodedLedState`.
 *
 * The byte layout matches the C `struct hidpp20_internal_led`:
 *   byte 0:    mode (LED_HW_MODE_*)
 *   bytes 1-10: mode-specific effect data
 *
 * This function extracts the mode, colors, duration and brightness
 * without applying any brightness-to-RGB scaling — that is the
 * serialization side's concern. */
pub fn parse_led_payload(payload: &[u8]) -> DecodedLedState {
    use crate::device::{Color, LedMode, RgbColor};

    let mut state = DecodedLedState {
        mode: LedMode::Off,
        color: Color::default(),
        secondary_color: Color::default(),
        effect_duration: 0,
        brightness: 255,
    };

    if payload.len() < LED_PAYLOAD_SIZE {
        return state;
    }

    let mode_byte = payload[0];

    match mode_byte {
        LED_HW_MODE_OFF => {
            state.mode = LedMode::Off;
        }
        LED_HW_MODE_FIXED => {
            state.mode = LedMode::Solid;
            state.color = Color::from_rgb(RgbColor {
                r: payload[1],
                g: payload[2],
                b: payload[3],
            });
        }
        LED_HW_MODE_CYCLE => {
            state.mode = LedMode::Cycle;
            state.effect_duration =
                u32::from(u16::from_be_bytes([payload[6], payload[7]]));
            state.brightness = u32::from(payload[8]) * 255 / 100;
        }
        LED_HW_MODE_COLOR_WAVE => {
            state.mode = LedMode::ColorWave;
            state.effect_duration =
                u32::from(u16::from_be_bytes([payload[6], payload[7]]));
            state.brightness = u32::from(payload[8]) * 255 / 100;
        }
        LED_HW_MODE_STARLIGHT => {
            state.mode = LedMode::Starlight;
            state.color = Color::from_rgb(RgbColor {
                r: payload[1],
                g: payload[2],
                b: payload[3],
            });
            state.secondary_color = Color::from_rgb(RgbColor {
                r: payload[4],
                g: payload[5],
                b: payload[6],
            });
        }
        LED_HW_MODE_BREATHING => {
            state.mode = LedMode::Breathing;
            state.color = Color::from_rgb(RgbColor {
                r: payload[1],
                g: payload[2],
                b: payload[3],
            });
            state.effect_duration =
                u32::from(u16::from_be_bytes([payload[4], payload[5]]));
            /* byte 6 = waveform */
            state.brightness = u32::from(payload[7]) * 255 / 100;
        }
        _ => { /* Unknown mode — keep defaults (Off). */ }
    }

    state
}

/* Feature 0x8100 Button Data */
pub const BUTTON_TYPE_MACRO: u8 = 0x00;
pub const BUTTON_TYPE_HID: u8 = 0x80;
pub const BUTTON_TYPE_SPECIAL: u8 = 0x90;
pub const BUTTON_TYPE_DISABLED: u8 = 0xFF;

pub const BUTTON_SUBTYPE_MOUSE: u8 = 0x01;
pub const BUTTON_SUBTYPE_KEYBOARD: u8 = 0x02;
pub const BUTTON_SUBTYPE_CONSUMER: u8 = 0x03;

/* A parsed HID++ report. */
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidppReport {
    /* Short report (7 bytes, report ID 0x10). */
    Short {
        device_index: u8,
        sub_id: u8,
        address: u8,
        params: [u8; 3],
    },
    /* Long report (20 bytes, report ID 0x11). */
    Long {
        device_index: u8,
        sub_id: u8,
        address: u8,
        params: [u8; 16],
    },
}

impl HidppReport {
    /* Try to parse a raw byte buffer into a structured report. */
    /* Returns `None` if the buffer is too short or has an */
    /* unrecognised report ID. */
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < 7 {
            return None;
        }

        match buf[0] {
            REPORT_ID_SHORT => Some(Self::Short {
                device_index: buf[1],
                sub_id: buf[2],
                address: buf[3],
                params: [buf[4], buf[5], buf[6]],
            }),
            REPORT_ID_LONG if buf.len() >= 20 => {
                let mut params = [0u8; 16];
                params.copy_from_slice(&buf[4..20]);
                Some(Self::Long {
                    device_index: buf[1],
                    sub_id: buf[2],
                    address: buf[3],
                    params,
                })
            }
            _ => None,
        }
    }

    pub fn is_error(&self) -> bool {
        match self {
            Self::Short { sub_id, .. } => *sub_id == HIDPP10_ERROR,
            Self::Long { sub_id, .. } => *sub_id == HIDPP20_ERROR,
        }
    }

    /* For a HID++ 2.0 long report, returns true if it is a response */
    /* matching the given device index and feature index. */
    pub fn matches_hidpp20(&self, expected_dev: u8, expected_feature: u8) -> bool {
        matches!(
            self,
            Self::Long { device_index, sub_id, .. }
                if *device_index == expected_dev && *sub_id == expected_feature
        )
    }

    /* Check if this report is a HID++ 2.0 error for the given device          */
    /* and feature.  Returns `Some(error_code)` when matched.                   */
    /*                                                                          */
    /* Two error formats exist:                                                 */
    /* - Long  (0x11): [dev, 0xFF, feature_idx, (fn<<4|sw), error_code, ...]    */
    /* - Short (0x10): [dev, 0x8F, feature_idx, (fn<<4|sw), error_code, 0]      */
    /*   The short variant is used by receivers when the wireless device is      */
    /*   unreachable or the request is invalid.                                 */
    pub fn hidpp20_error_code(
        &self,
        expected_dev: u8,
        expected_feature: u8,
    ) -> Option<u8> {
        match self {
            Self::Long {
                device_index,
                sub_id,
                address,
                params,
            } if *device_index == expected_dev
                && *sub_id == HIDPP20_ERROR
                && *address == expected_feature =>
            {
                Some(params[1])
            }
            Self::Short {
                device_index,
                sub_id,
                address,
                params,
            } if *device_index == expected_dev
                && *sub_id == HIDPP10_ERROR
                && *address == expected_feature =>
            {
                Some(params[1])
            }
            _ => None,
        }
    }
}

/* -------------------------------------------------------------------------- */
/* Shared matcher helpers for DeviceIo::request() closures                   */
/* -------------------------------------------------------------------------- */

/* Result of matching a HID++ 1.0 register response.
 *
 * Encapsulates the parse → error-check → field-match → extract pattern
 * that every register read/write closure in hidpp10.rs duplicates. */
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hidpp10MatchResult {
    Short([u8; 3]),
    Long([u8; 16]),
    NoMatch,
}

/* Match a raw buffer against an expected HID++ 1.0 register response.
 *
 * Returns `Short(params)` or `Long(params)` when the report matches
 * the expected (device_index, sub_id, register) triple and is not an
 * error report.  Returns `NoMatch` for everything else (parse failure,
 * error reports, field mismatches, non-HID++ data). */
pub fn match_hidpp10_register(
    buf: &[u8],
    expected_device: u8,
    expected_sub_id: u8,
    expected_register: u8,
) -> Hidpp10MatchResult {
    let Some(report) = HidppReport::parse(buf) else {
        return Hidpp10MatchResult::NoMatch;
    };
    if report.is_error() {
        return Hidpp10MatchResult::NoMatch;
    }
    match report {
        HidppReport::Short { device_index, sub_id, address, params }
            if device_index == expected_device
                && sub_id == expected_sub_id
                && address == expected_register =>
        {
            Hidpp10MatchResult::Short(params)
        }
        HidppReport::Long { device_index, sub_id, address, params }
            if device_index == expected_device
                && sub_id == expected_sub_id
                && address == expected_register =>
        {
            Hidpp10MatchResult::Long(params)
        }
        _ => Hidpp10MatchResult::NoMatch,
    }
}

/* Result of matching an HID++ 2.0 feature response.
 *
 * Encapsulates the three-step matcher pattern used by every
 * feature_request variant: (1) check for HID++ error, (2) match
 * Long response, (3) match Short acknowledgment. */
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hidpp20MatchResult {
    /* Successful response with 16-byte params (Short acks are zero-padded). */
    Ok([u8; 16]),
    /* HID++ error with the raw error code byte. */
    HidppErr(u8),
    /* Report did not match the expected feature. */
    NoMatch,
}

/* Match a raw buffer against an expected HID++ 2.0 feature response.
 *
 * Checks for error reports first (both Long 0xFF and Short 0x8F),
 * then matches successful Long responses (full 16-byte params), then
 * successful Short responses (3-byte params zero-padded to 16). */
pub fn match_hidpp20_feature_response(
    buf: &[u8],
    expected_device: u8,
    expected_feature_index: u8,
) -> Hidpp20MatchResult {
    let Some(report) = HidppReport::parse(buf) else {
        return Hidpp20MatchResult::NoMatch;
    };

    /* 1. Check for HID++ error (Long 0xFF or Short 0x8F). */
    if let Some(code) = report.hidpp20_error_code(expected_device, expected_feature_index) {
        return Hidpp20MatchResult::HidppErr(code);
    }

    /* 2. Successful Long response. */
    if let HidppReport::Long { device_index, sub_id, params, .. } = &report {
        if *device_index == expected_device && *sub_id == expected_feature_index {
            return Hidpp20MatchResult::Ok(*params);
        }
    }

    /* 3. Successful Short response (SET acknowledgment). */
    if let HidppReport::Short { device_index, sub_id, params, .. } = &report {
        if *device_index == expected_device && *sub_id == expected_feature_index {
            let mut long_params = [0u8; 16];
            long_params[..3].copy_from_slice(params);
            return Hidpp20MatchResult::Ok(long_params);
        }
    }

    Hidpp20MatchResult::NoMatch
}

/* Construct an `anyhow::Error` for a HID++ 2.0 feature error response.
 *
 * This is the common error conversion used by feature_request,
 * short_feature_request, and short_feature_request_with_params. */
pub fn hidpp20_feature_error(code: u8, feature_index: u8, function: u8) -> anyhow::Error {
    let name = hidpp20_error_name(code);
    anyhow::anyhow!(
        "HID++ error {name} (0x{code:02X}) for feature 0x{feature_index:02X} fn={function}"
    )
}

/* Decode a HID++ 2.0 report-rate bitmap (from feature 0x8060
 * getReportRateList) into a list of supported rates in Hz.
 *
 * Each set bit `n` (0-based) represents a supported rate of
 * `1000 / (n + 1)` Hz.  Bit 0 = 1000 Hz, bit 1 = 500 Hz, etc. */
pub fn decode_report_rate_bitmap(bitmap: u8) -> Vec<u32> {
    (0..8u32)
        .filter(|bit| bitmap & (1 << bit) != 0)
        .map(|bit| 1000 / (bit + 1))
        .collect()
}

/* Human-readable name for a HID++ 2.0 error code.                 */
pub fn hidpp20_error_name(code: u8) -> &'static str {
    match code {
        0x00 => "NO_ERROR",
        0x01 => "UNKNOWN",
        0x02 => "INVALID_ARGUMENT",
        0x03 => "OUT_OF_RANGE",
        0x04 => "HARDWARE_ERROR",
        0x05 => "LOGITECH_INTERNAL",
        0x06 => "INVALID_FEATURE_INDEX",
        0x07 => "INVALID_FUNCTION_ID",
        0x08 => "BUSY",
        0x09 => "UNSUPPORTED",
        _ => "UNKNOWN_ERROR",
    }
}

/* Build a 7-byte HID++ short report. */
pub fn build_short_report(device_index: u8, sub_id: u8, address: u8, params: [u8; 3]) -> [u8; 7] {
    [
        REPORT_ID_SHORT,
        device_index,
        sub_id,
        address,
        params[0],
        params[1],
        params[2],
    ]
}

/* Build a 20-byte HID++ long report. */
pub fn build_long_report(device_index: u8, sub_id: u8, address: u8, params: [u8; 16]) -> [u8; 20] {
    let mut buf = [0u8; 20];
    buf[0] = REPORT_ID_LONG;
    buf[1] = device_index;
    buf[2] = sub_id;
    buf[3] = address;
    buf[4..20].copy_from_slice(&params);
    buf
}

/* Build a HID++ 2.0 feature request. */
/*  */
/* Layout: `[0x11, device_idx, feature_idx, (function << 4 | sw_id), params...]` */
pub fn build_hidpp20_request(
    device_index: u8,
    feature_index: u8,
    function: u8,
    sw_id: u8,
    params: &[u8],
) -> [u8; 20] {
    let mut buf = [0u8; 20];
    buf[0] = REPORT_ID_LONG;
    buf[1] = device_index;
    buf[2] = feature_index;
    buf[3] = (function << 4) | (sw_id & 0x0F);
    let copy_len = params.len().min(16);
    buf[4..4 + copy_len].copy_from_slice(&params[..copy_len]);
    buf
}

/* Build a HID++ 2.0 short feature request (7 bytes). */
/*  */
/* Mirrors the C `REPORT_ID_SHORT` requests used for parameter-free */
/* commands like MEMORY_WRITE_END.  Layout:                         */
/* `[0x10, device_idx, feature_idx, (function << 4 | sw_id), 0, 0, 0]` */
pub fn build_hidpp20_short_request(
    device_index: u8,
    feature_index: u8,
    function: u8,
    sw_id: u8,
) -> [u8; 7] {
    [
        REPORT_ID_SHORT,
        device_index,
        feature_index,
        (function << 4) | (sw_id & 0x0F),
        0,
        0,
        0,
    ]
}

/* Build a HID++ 2.0 short feature request (7 bytes) with parameters. */
/*  */
/* Some firmware commands (e.g. SET_CURRENT_PROFILE, SET_CURRENT_DPI_INDEX) */
/* must be sent as short reports to match the C driver's behaviour. */
/* Layout: `[0x10, dev_idx, feature_idx, (fn << 4 | sw_id), p0, p1, p2]` */
pub fn build_hidpp20_short_request_with_params(
    device_index: u8,
    feature_index: u8,
    function: u8,
    sw_id: u8,
    params: &[u8],
) -> [u8; 7] {
    let mut buf = [0u8; 7];
    buf[0] = REPORT_ID_SHORT;
    buf[1] = device_index;
    buf[2] = feature_index;
    buf[3] = (function << 4) | (sw_id & 0x0F);
    let copy_len = params.len().min(3);
    buf[4..4 + copy_len].copy_from_slice(&params[..copy_len]);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_short_report() {
        let buf = [0x10, 0x00, 0x01, 0x57, 0xAA, 0xBB, 0xCC];
        let report = HidppReport::parse(&buf).expect("valid short report");
        assert_eq!(
            report,
            HidppReport::Short {
                device_index: 0x00,
                sub_id: 0x01,
                address: 0x57,
                params: [0xAA, 0xBB, 0xCC],
            }
        );
    }

    #[test]
    fn parse_long_report() {
        let mut buf = [0u8; 20];
        buf[0] = REPORT_ID_LONG;
        buf[1] = 0x02;
        buf[2] = 0x03;
        buf[3] = 0xFF;
        let report = HidppReport::parse(&buf).expect("valid long report");
        match report {
            HidppReport::Long { device_index, sub_id, address, params } => {
                assert_eq!(device_index, 0x02);
                assert_eq!(sub_id, 0x03);
                assert_eq!(address, 0xFF);
                assert_eq!(params[0], 0x00);
            }
            _ => panic!("Expected Long report"),
        }
    }

    #[test]
    fn parse_invalid_report_id() {
        let buf = [0x99, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert!(HidppReport::parse(&buf).is_none());
    }

    #[test]
    fn parse_buffer_too_short() {
        assert!(HidppReport::parse(&[0x10, 0x00]).is_none());
        assert!(HidppReport::parse(&[]).is_none());
    }

    #[test]
    fn build_short_roundtrip() {
        let report = build_short_report(0x00, 0x01, 0xAA, [0xBB, 0xCC, 0xDD]);
        let parsed = HidppReport::parse(&report).expect("roundtrip");
        match parsed {
            HidppReport::Short { device_index, sub_id, address, params } => {
                assert_eq!(device_index, 0x00);
                assert_eq!(sub_id, 0x01);
                assert_eq!(address, 0xAA);
                assert_eq!(params[0], 0xBB);
            }
            _ => panic!("Expected Short report"),
        }
    }

    #[test]
    fn build_hidpp20_request_encoding() {
        let req = build_hidpp20_request(0x00, 0x01, 0x02, 0x0A, &[0x11, 0x22]);
        assert_eq!(req[0], REPORT_ID_LONG);
        assert_eq!(req[1], 0x00);
        assert_eq!(req[2], 0x01);
        /* function=0x02, sw_id=0x0A → (0x02 << 4) | 0x0A = 0x2A */
        assert_eq!(req[3], 0x2A);
        assert_eq!(req[4], 0x11);
        assert_eq!(req[5], 0x22);
    }

    #[test]
    fn error_detection() {
        let err_short = HidppReport::Short {
            device_index: 0,
            sub_id: HIDPP10_ERROR,
            address: 0,
            params: [0; 3],
        };
        assert!(err_short.is_error());

        let ok_short = HidppReport::Short {
            device_index: 0,
            sub_id: 0x01,
            address: 0,
            params: [0; 3],
        };
        assert!(!ok_short.is_error());
    }

    #[test]
    fn matches_hidpp20_helper() {
        let report = HidppReport::Long {
            device_index: 0x00,
            sub_id: 0x05,
            address: 0x10,
            params: [0; 16],
        };
        assert!(report.matches_hidpp20(0x00, 0x05));
        assert!(!report.matches_hidpp20(0x00, 0x06));
        assert!(!report.matches_hidpp20(0x01, 0x05));
    }

    /* ------------------------------------------------------------------ */
    /* LED payload serialization tests                                    */
    /* ------------------------------------------------------------------ */

    use crate::device::{Color, LedInfo, LedMode};

    fn make_led(mode: LedMode) -> LedInfo {
        LedInfo {
            index: 0,
            mode,
            modes: vec![LedMode::Off],
            color: Color::default(),
            secondary_color: Color::default(),
            tertiary_color: Color::default(),
            color_depth: 1,
            effect_duration: 0,
            brightness: 255,
        }
    }

    #[test]
    fn led_payload_off() {
        let led = make_led(LedMode::Off);
        let p = build_led_payload(&led);
        assert_eq!(p, [0x00; LED_PAYLOAD_SIZE]);
    }

    #[test]
    fn led_payload_solid() {
        let mut led = make_led(LedMode::Solid);
        led.color = Color { red: 255, green: 128, blue: 0 };
        let p = build_led_payload(&led);
        assert_eq!(p[0], LED_HW_MODE_FIXED);
        assert_eq!(p[1], 255);
        assert_eq!(p[2], 128);
        assert_eq!(p[3], 0);
    }

    #[test]
    fn led_payload_cycle() {
        let mut led = make_led(LedMode::Cycle);
        led.effect_duration = 5000;
        led.brightness = 255;
        let p = build_led_payload(&led);
        assert_eq!(p[0], LED_HW_MODE_CYCLE);
        /* period 5000 = 0x1388 big-endian */
        assert_eq!(p[6], 0x13);
        assert_eq!(p[7], 0x88);
        /* brightness 255 → 100% */
        assert_eq!(p[8], 100);
    }

    #[test]
    fn led_payload_color_wave() {
        let mut led = make_led(LedMode::ColorWave);
        led.effect_duration = 3000;
        led.brightness = 127;
        let p = build_led_payload(&led);
        assert_eq!(p[0], LED_HW_MODE_COLOR_WAVE);
        assert_eq!(p[6], 0x0B);
        assert_eq!(p[7], 0xB8);
        /* brightness 127 → 127*100/255 = 49 */
        assert_eq!(p[8], 49);
    }

    #[test]
    fn led_payload_starlight() {
        let mut led = make_led(LedMode::Starlight);
        led.color = Color { red: 10, green: 20, blue: 30 };
        led.secondary_color = Color { red: 40, green: 50, blue: 60 };
        let p = build_led_payload(&led);
        assert_eq!(p[0], LED_HW_MODE_STARLIGHT);
        /* sky color */
        assert_eq!(p[1], 10);
        assert_eq!(p[2], 20);
        assert_eq!(p[3], 30);
        /* star color */
        assert_eq!(p[4], 40);
        assert_eq!(p[5], 50);
        assert_eq!(p[6], 60);
    }

    #[test]
    fn led_payload_breathing() {
        let mut led = make_led(LedMode::Breathing);
        led.color = Color { red: 0, green: 255, blue: 0 };
        led.effect_duration = 2000;
        led.brightness = 200;
        let p = build_led_payload(&led);
        assert_eq!(p[0], LED_HW_MODE_BREATHING);
        assert_eq!(p[1], 0);
        assert_eq!(p[2], 255);
        assert_eq!(p[3], 0);
        /* period 2000 = 0x07D0 */
        assert_eq!(p[4], 0x07);
        assert_eq!(p[5], 0xD0);
        /* waveform = 0x00 (default) at [6] */
        assert_eq!(p[6], 0x00);
        /* brightness 200 → 200*100/255 = 78 */
        assert_eq!(p[7], 78);
    }

    #[test]
    fn led_payload_tricolor() {
        let mut led = make_led(LedMode::TriColor);
        led.color = Color { red: 255, green: 0, blue: 0 };
        led.secondary_color = Color { red: 0, green: 255, blue: 0 };
        led.tertiary_color = Color { red: 0, green: 0, blue: 255 };
        let p = build_led_payload(&led);
        /* TriColor serializes as FIXED mode byte */
        assert_eq!(p[0], LED_HW_MODE_FIXED);
        /* left (primary) */
        assert_eq!(p[1], 255);
        assert_eq!(p[2], 0);
        assert_eq!(p[3], 0);
        /* center (secondary) */
        assert_eq!(p[4], 0);
        assert_eq!(p[5], 255);
        assert_eq!(p[6], 0);
        /* right (tertiary) */
        assert_eq!(p[7], 0);
        assert_eq!(p[8], 0);
        assert_eq!(p[9], 255);
    }

    /* ------------------------------------------------------------------ */
    /* Short request builder tests                                        */
    /* ------------------------------------------------------------------ */

    #[test]
    fn build_short_request_encoding() {
        let req = build_hidpp20_short_request(0xFF, 0x05, 0x08, 0x04);
        assert_eq!(req[0], REPORT_ID_SHORT);
        assert_eq!(req[1], 0xFF);   /* device index */
        assert_eq!(req[2], 0x05);   /* feature index */
        /* function=0x08, sw_id=0x04 → (0x08 << 4) | 0x04 = 0x84 */
        assert_eq!(req[3], 0x84);
        assert_eq!(req[4], 0x00);   /* zero params */
        assert_eq!(req[5], 0x00);
        assert_eq!(req[6], 0x00);
        assert_eq!(req.len(), 7);
    }

    /* ------------------------------------------------------------------ */
    /* Opcode alignment with C driver (compile-time sanity)               */
    /* ------------------------------------------------------------------ */

    #[test]
    fn led_hw_mode_constants_match_c() {
        /* C: hidpp20_color_led_zone_effect enum values */
        assert_eq!(LED_HW_MODE_OFF, 0x00);
        assert_eq!(LED_HW_MODE_FIXED, 0x01);
        assert_eq!(LED_HW_MODE_CYCLE, 0x03);
        assert_eq!(LED_HW_MODE_COLOR_WAVE, 0x04);
        assert_eq!(LED_HW_MODE_STARLIGHT, 0x05);
        assert_eq!(LED_HW_MODE_BREATHING, 0x0A);
    }

    #[test]
    fn crc_ccitt_empty_is_seed() {
        /* With no data the CRC should remain at the seed value 0xFFFF. */
        assert_eq!(compute_ccitt_crc(&[]), 0xFFFF);
    }

    #[test]
    fn crc_ccitt_known_vector() {
        /* "123456789" is the standard CRC-CCITT test vector → 0x29B1. */
        let data = b"123456789";
        assert_eq!(compute_ccitt_crc(data), 0x29B1);
    }

    /* ------------------------------------------------------------------ */
    /* HID++ 1.0 register matcher tests                                   */
    /* ------------------------------------------------------------------ */

    #[test]
    fn hidpp10_match_short_success() {
        let buf = build_short_report(0xFF, 0x81, 0x63, [0xAA, 0xBB, 0xCC]);
        let result = match_hidpp10_register(&buf, 0xFF, 0x81, 0x63);
        assert_eq!(result, Hidpp10MatchResult::Short([0xAA, 0xBB, 0xCC]));
    }

    #[test]
    fn hidpp10_match_long_success() {
        let mut params = [0u8; 16];
        params[0] = 0x42;
        let buf = build_long_report(0x01, 0x83, 0x63, params);
        let result = match_hidpp10_register(&buf, 0x01, 0x83, 0x63);
        assert_eq!(result, Hidpp10MatchResult::Long(params));
    }

    #[test]
    fn hidpp10_match_error_report() {
        /* Error sub_id 0x8F should yield NoMatch. */
        let buf = build_short_report(0xFF, HIDPP10_ERROR, 0x63, [0x00, 0x02, 0x00]);
        let result = match_hidpp10_register(&buf, 0xFF, 0x81, 0x63);
        assert_eq!(result, Hidpp10MatchResult::NoMatch);
    }

    #[test]
    fn hidpp10_match_wrong_fields() {
        /* Correct format but wrong device index → NoMatch. */
        let buf = build_short_report(0x01, 0x81, 0x63, [0xAA, 0xBB, 0xCC]);
        let result = match_hidpp10_register(&buf, 0xFF, 0x81, 0x63);
        assert_eq!(result, Hidpp10MatchResult::NoMatch);
    }

    /* ------------------------------------------------------------------ */
    /* HID++ 2.0 feature response matcher tests                           */
    /* ------------------------------------------------------------------ */

    #[test]
    fn hidpp20_match_long_success() {
        let mut params = [0u8; 16];
        params[0] = 0x42;
        params[1] = 0x13;
        let buf = build_long_report(0xFF, 0x05, 0x30, params);
        let result = match_hidpp20_feature_response(&buf, 0xFF, 0x05);
        assert_eq!(result, Hidpp20MatchResult::Ok(params));
    }

    #[test]
    fn hidpp20_match_short_ack() {
        /* Short acknowledgment: 3 params zero-padded to 16. */
        let buf = build_short_report(0xFF, 0x05, 0x10, [0xAA, 0xBB, 0x00]);
        let result = match_hidpp20_feature_response(&buf, 0xFF, 0x05);
        let mut expected = [0u8; 16];
        expected[0] = 0xAA;
        expected[1] = 0xBB;
        assert_eq!(result, Hidpp20MatchResult::Ok(expected));
    }

    #[test]
    fn hidpp20_match_long_error() {
        /* Long error: sub_id = 0xFF, params[1] = error_code. */
        let mut params = [0u8; 16];
        params[0] = 0x30; /* (fn << 4 | sw_id) */
        params[1] = 0x02; /* INVALID_ARGUMENT */
        let buf = build_long_report(0xFF, HIDPP20_ERROR, 0x05, params);
        let result = match_hidpp20_feature_response(&buf, 0xFF, 0x05);
        assert_eq!(result, Hidpp20MatchResult::HidppErr(0x02));
    }

    #[test]
    fn hidpp20_match_short_error() {
        /* Short error: sub_id = 0x8F, params[1] = error_code. */
        let buf = build_short_report(0xFF, HIDPP10_ERROR, 0x05, [0x30, 0x02, 0x00]);
        let result = match_hidpp20_feature_response(&buf, 0xFF, 0x05);
        assert_eq!(result, Hidpp20MatchResult::HidppErr(0x02));
    }

    #[test]
    fn hidpp20_match_wrong_feature() {
        let mut params = [0u8; 16];
        params[0] = 0x42;
        let buf = build_long_report(0xFF, 0x05, 0x30, params);
        /* Expected feature 0x06, but report has 0x05 → NoMatch. */
        let result = match_hidpp20_feature_response(&buf, 0xFF, 0x06);
        assert_eq!(result, Hidpp20MatchResult::NoMatch);
    }

    /* ------------------------------------------------------------------ */
    /* LED payload parser tests                                           */
    /* ------------------------------------------------------------------ */

    #[test]
    fn parse_led_off() {
        let payload = [0u8; LED_PAYLOAD_SIZE];
        let state = parse_led_payload(&payload);
        assert_eq!(state.mode, LedMode::Off);
    }

    #[test]
    fn parse_led_solid() {
        let mut payload = [0u8; LED_PAYLOAD_SIZE];
        payload[0] = LED_HW_MODE_FIXED;
        payload[1] = 0xFF;
        payload[2] = 0x80;
        payload[3] = 0x00;
        let state = parse_led_payload(&payload);
        assert_eq!(state.mode, LedMode::Solid);
        let rgb = state.color.to_rgb();
        assert_eq!(rgb.r, 0xFF);
        assert_eq!(rgb.g, 0x80);
        assert_eq!(rgb.b, 0x00);
    }

    #[test]
    fn parse_led_cycle() {
        let mut payload = [0u8; LED_PAYLOAD_SIZE];
        payload[0] = LED_HW_MODE_CYCLE;
        /* period 5000 = 0x1388 big-endian */
        payload[6] = 0x13;
        payload[7] = 0x88;
        /* brightness 100% */
        payload[8] = 100;
        let state = parse_led_payload(&payload);
        assert_eq!(state.mode, LedMode::Cycle);
        assert_eq!(state.effect_duration, 5000);
        assert_eq!(state.brightness, 255);
    }

    #[test]
    fn parse_led_starlight() {
        let mut payload = [0u8; LED_PAYLOAD_SIZE];
        payload[0] = LED_HW_MODE_STARLIGHT;
        payload[1] = 10; payload[2] = 20; payload[3] = 30;
        payload[4] = 40; payload[5] = 50; payload[6] = 60;
        let state = parse_led_payload(&payload);
        assert_eq!(state.mode, LedMode::Starlight);
        let c1 = state.color.to_rgb();
        assert_eq!((c1.r, c1.g, c1.b), (10, 20, 30));
        let c2 = state.secondary_color.to_rgb();
        assert_eq!((c2.r, c2.g, c2.b), (40, 50, 60));
    }

    #[test]
    fn parse_led_breathing() {
        let mut payload = [0u8; LED_PAYLOAD_SIZE];
        payload[0] = LED_HW_MODE_BREATHING;
        payload[1] = 0; payload[2] = 255; payload[3] = 0;
        /* period 2000 = 0x07D0 */
        payload[4] = 0x07; payload[5] = 0xD0;
        /* byte 6 = waveform (ignored) */
        /* brightness 78% → 78*255/100 = 198 */
        payload[7] = 78;
        let state = parse_led_payload(&payload);
        assert_eq!(state.mode, LedMode::Breathing);
        let rgb = state.color.to_rgb();
        assert_eq!((rgb.r, rgb.g, rgb.b), (0, 255, 0));
        assert_eq!(state.effect_duration, 2000);
        assert_eq!(state.brightness, 78 * 255 / 100);
    }

    #[test]
    fn parse_led_color_wave() {
        let mut payload = [0u8; LED_PAYLOAD_SIZE];
        payload[0] = LED_HW_MODE_COLOR_WAVE;
        payload[6] = 0x0B; payload[7] = 0xB8; /* period 3000 */
        payload[8] = 49; /* brightness 49% */
        let state = parse_led_payload(&payload);
        assert_eq!(state.mode, LedMode::ColorWave);
        assert_eq!(state.effect_duration, 3000);
        assert_eq!(state.brightness, 49 * 255 / 100);
    }

    /* ------------------------------------------------------------------ */
    /* Report rate bitmap decoder tests                                   */
    /* ------------------------------------------------------------------ */

    #[test]
    fn decode_rate_bitmap_typical() {
        /* Bits 0,1,3 set → 1000, 500, 250 Hz. */
        let rates = decode_report_rate_bitmap(0b00001011);
        assert_eq!(rates, vec![1000, 500, 250]);
    }

    #[test]
    fn decode_rate_bitmap_empty() {
        assert_eq!(decode_report_rate_bitmap(0x00), Vec::<u32>::new());
    }

    #[test]
    fn decode_rate_bitmap_all() {
        let rates = decode_report_rate_bitmap(0xFF);
        assert_eq!(rates, vec![1000, 500, 333, 250, 200, 166, 142, 125]);
    }
}
