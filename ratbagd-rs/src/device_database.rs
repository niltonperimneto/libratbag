use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

use configparser::ini::Ini;
use tracing::{debug, warn};

/* Bus protocol identifier used in `.device` match patterns and DB keys. */
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BusType {
    Usb,
    Bluetooth,
    Other(String),
}

impl BusType {
    /* Convert the numeric bustype from a udev HID_ID attribute into a BusType. */
    pub fn from_u16(bustype: u16) -> Self {
        match bustype {
            0x03 => BusType::Usb,
            0x05 => BusType::Bluetooth,
            other => BusType::Other(format!("{:04x}", other)),
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "usb" => BusType::Usb,
            "bluetooth" => BusType::Bluetooth,
            other => BusType::Other(other.to_string()),
        }
    }
}

impl fmt::Display for BusType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BusType::Usb => f.write_str("usb"),
            BusType::Bluetooth => f.write_str("bluetooth"),
            BusType::Other(s) => f.write_str(s),
        }
    }
}

/* A parsed `.device` file entry describing a supported mouse. */
#[derive(Debug, Clone)]
pub struct DeviceEntry {
    pub name: String,
    pub driver: String,
    pub matches: Vec<DeviceMatch>,
    pub driver_config: Option<DriverConfig>,
}

/* A single bus:vid:pid match pattern from the `DeviceMatch=` field. */
#[derive(Debug, Clone)]
pub struct DeviceMatch {
    pub bustype: BusType,
    pub vid: u16,
    pub pid: u16,
}

/* Driver-specific configuration from the `[Driver/xxx]` section. */
#[derive(Debug, Clone, Default)]
pub struct DriverConfig {
    pub profiles: Option<u32>,
    pub buttons: Option<u32>,
    pub leds: Option<u32>,
    pub dpis: Option<u32>,
    pub dpi_range: Option<DpiRange>,
    #[allow(dead_code)]
    pub wireless: bool,
}

/* A DPI range specification parsed from `DpiRange=min:max@step`. */
#[derive(Debug, Clone)]
pub struct DpiRange {
    pub min: u32,
    pub max: u32,
    pub step: u32,
}

/* Device database: maps `(bustype, vid, pid)` to a `DeviceEntry`. */
/*                                                                   */
/* Entries are reference-counted so that devices with multiple match */
/* patterns share a single allocation instead of being duplicated.   */
pub type DeviceDb = HashMap<(BusType, u16, u16), Arc<DeviceEntry>>;

/* Load all `.device` files from the given directory into a lookup table. */
/*  */
/* Each `DeviceMatch` pattern (semicolon-separated in the file) becomes */
/* a separate key in the returned map, all pointing to the same `DeviceEntry`. */
pub fn load_device_database(data_dir: &Path) -> DeviceDb {
    let mut db = HashMap::new();

    let entries = match std::fs::read_dir(data_dir) {
        Ok(e) => e,
        Err(err) => {
            warn!("Failed to read device data directory {:?}: {}", data_dir, err);
            return db;
        }
    };

    for dir_entry in entries.flatten() {
        let path = dir_entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("device") {
            continue;
        }

        match parse_device_file(&path) {
            Ok(entry) => {
                /* Collect keys first so we move BusType out of the Vec
                 * before entry is frozen inside the Arc. */
                let keys: Vec<(BusType, u16, u16)> = entry
                    .matches
                    .iter()
                    .map(|m| (m.bustype.clone(), m.vid, m.pid))
                    .collect();
                let entry = Arc::new(entry);
                for key in keys {
                    db.insert(key, Arc::clone(&entry));
                }
                debug!(
                    "Loaded device: {} ({} match patterns)",
                    entry.name,
                    entry.matches.len()
                );
            }
            Err(err) => {
                warn!("Failed to parse {:?}: {}", path, err);
            }
        }
    }

    debug!("Device database loaded: {} entries", db.len());
    db
}

/* Parse a single `.device` INI file into a `DeviceEntry`. */
fn parse_device_file(path: &Path) -> Result<DeviceEntry, String> {
    let mut ini = Ini::new();
    ini.load(path).map_err(|e| format!("INI parse error: {}", e))?;

    /* [Device] section — required fields */
    let name = ini
        .get("device", "name")
        .ok_or("Missing [Device] Name")?;
    let driver = ini
        .get("device", "driver")
        .ok_or("Missing [Device] Driver")?;
    let match_str = ini
        .get("device", "devicematch")
        .ok_or("Missing [Device] DeviceMatch")?;

    /* Parse semicolon-separated match patterns: "usb:046d:c539;usb:046d:c53a" */
    let matches = parse_device_matches(&match_str)?;

    /* [Driver/xxx] section — optional */
    let driver_section = format!("driver/{}", driver);
    let driver_config = if ini.sections().contains(&driver_section) {
        Some(parse_driver_config(&ini, &driver_section))
    } else {
        None
    };

    Ok(DeviceEntry {
        name,
        driver,
        matches,
        driver_config,
    })
}

/* Parse a `DeviceMatch` string like `"usb:046d:c539;usb:046d:c53a"`. */
fn parse_device_matches(s: &str) -> Result<Vec<DeviceMatch>, String> {
    let mut matches = Vec::new();

    for part in s.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let segments: Vec<&str> = part.split(':').collect();
        if segments.len() != 3 {
            return Err(format!("Invalid DeviceMatch pattern: {}", part));
        }

        let bustype = BusType::from_str(segments[0]);
        let vid = u16::from_str_radix(segments[1], 16)
            .map_err(|e| format!("Invalid VID in '{}': {}", part, e))?;
        let pid = u16::from_str_radix(segments[2], 16)
            .map_err(|e| format!("Invalid PID in '{}': {}", part, e))?;

        matches.push(DeviceMatch { bustype, vid, pid });
    }

    if matches.is_empty() {
        return Err("DeviceMatch is empty".to_string());
    }

    Ok(matches)
}

/* Parse the `[Driver/xxx]` section for driver-specific configuration. */
fn parse_driver_config(ini: &Ini, section: &str) -> DriverConfig {
    let dpi_range = if let Some(range_str) = ini.get(section, "dpirange") {
        parse_dpi_range(&range_str)
    } else {
        None
    };

    DriverConfig {
        profiles: ini.get(section, "profiles").and_then(|v| v.parse().ok()),
        buttons: ini.get(section, "buttons").and_then(|v| v.parse().ok()),
        leds: ini.get(section, "leds").and_then(|v| v.parse().ok()),
        dpis: ini.get(section, "dpis").and_then(|v| v.parse().ok()),
        wireless: ini
            .get(section, "wireless")
            .and_then(|v| v.parse::<u32>().ok())
            .map(|v| v != 0)
            .unwrap_or(false),
        dpi_range,
    }
}

/* Parse a DPI range string like `"100:16000@100"`. */
fn parse_dpi_range(s: &str) -> Option<DpiRange> {
    let (range_part, step_str) = s.split_once('@')?;
    let (min_str, max_str) = range_part.split_once(':')?;

    let min = min_str.parse().ok()?;
    let max = max_str.parse().ok()?;
    let step: u32 = step_str.parse().ok()?;

    /* Reject degenerate ranges that would cause step_by(0) panics or empty lists. */
    if step == 0 || min > max {
        return None;
    }

    Some(DpiRange { min, max, step })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_device_matches_single() {
        let matches = parse_device_matches("usb:046d:c539").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].bustype, BusType::Usb);
        assert_eq!(matches[0].vid, 0x046d);
        assert_eq!(matches[0].pid, 0xc539);
    }

    #[test]
    fn test_parse_device_matches_multiple() {
        let matches = parse_device_matches("usb:0b05:18e3;usb:0b05:18e5").unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].pid, 0x18e3);
        assert_eq!(matches[1].pid, 0x18e5);
    }

    #[test]
    fn test_parse_device_matches_bluetooth() {
        let matches = parse_device_matches("bluetooth:046d:b025").unwrap();
        assert_eq!(matches[0].bustype, BusType::Bluetooth);
    }

    #[test]
    fn test_parse_device_matches_mixed_bus() {
        let matches =
            parse_device_matches("usb:046d:4090;bluetooth:046d:b025").unwrap();
        assert_eq!(matches[0].bustype, BusType::Usb);
        assert_eq!(matches[1].bustype, BusType::Bluetooth);
    }

    #[test]
    fn test_parse_dpi_range() {
        let range = parse_dpi_range("100:16000@100").unwrap();
        assert_eq!(range.min, 100);
        assert_eq!(range.max, 16000);
        assert_eq!(range.step, 100);
    }

    #[test]
    fn test_parse_dpi_range_invalid() {
        assert!(parse_dpi_range("invalid").is_none());
    }

    #[test]
    fn test_parse_dpi_range_zero_step() {
        assert!(parse_dpi_range("100:16000@0").is_none());
    }

    #[test]
    fn test_parse_dpi_range_inverted_bounds() {
        assert!(parse_dpi_range("16000:100@100").is_none());
    }

    #[test]
    fn test_parse_device_matches_invalid() {
        assert!(parse_device_matches("usb:046d").is_err());
    }

    #[test]
    fn test_parse_device_matches_empty() {
        assert!(parse_device_matches("").is_err());
    }

    #[test]
    fn test_bustype_from_u16() {
        assert_eq!(BusType::from_u16(0x03), BusType::Usb);
        assert_eq!(BusType::from_u16(0x05), BusType::Bluetooth);
        assert_eq!(BusType::from_u16(0x01), BusType::Other("0001".to_string()));
    }

    #[test]
    fn test_bustype_display() {
        assert_eq!(BusType::Usb.to_string(), "usb");
        assert_eq!(BusType::Bluetooth.to_string(), "bluetooth");
        assert_eq!(BusType::Other("serial".to_string()).to_string(), "serial");
    }
}
