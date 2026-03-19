/* DBus Manager interface: entry point that tracks device object paths and, under dev-hooks, injects
 * or resets synthetic test devices. */
use zbus::interface;
use zbus::zvariant::ObjectPath;

/// DBus API version. Must match the C daemon's value for client compatibility.
pub const API_VERSION: i32 = 2;

#[cfg(feature = "dev-hooks")]
use crate::udev_monitor::DeviceAction;
#[cfg(feature = "dev-hooks")]
use tokio::sync::mpsc;
#[cfg(feature = "dev-hooks")]
use tracing::{info, warn};

#[cfg(feature = "dev-hooks")]
use crate::driver::MockState;
#[cfg(feature = "dev-hooks")]
use std::collections::HashMap;
#[cfg(feature = "dev-hooks")]
use std::sync::{Arc, Mutex};

/// The `org.freedesktop.ratbag1.Manager` interface.
///
/// Entry point for clients (Piper, ratbagctl) to discover connected devices.
/// State is managed through zbus's built-in interior mutability (`get_mut()`),
/// so no additional locking is needed.
pub struct RatbagManager {
    devices: Vec<String>,

    /// Channel to inject synthetic test devices into the main event loop.
    /// Only present when the `dev-hooks` feature is enabled.
    #[cfg(feature = "dev-hooks")]
    test_device_tx: Option<mpsc::Sender<DeviceAction>>,
    /// Monotonic counter used to generate unique test device sysnames.
    #[cfg(feature = "dev-hooks")]
    test_device_counter: u32,
    /// Sysname of the currently-live test device, if any.
    #[cfg(feature = "dev-hooks")]
    current_test_sysname: Option<String>,
    /// Shared mock I/O state map: sysname → MockState.
    /// Populated by the event loop when `InjectTestWithDriver` completes.
    /// Read by `GetMockIoLog` to return the write log.
    #[cfg(feature = "dev-hooks")]
    mock_states: Arc<Mutex<HashMap<String, Arc<Mutex<MockState>>>>>,
}

impl Default for RatbagManager {
    fn default() -> Self {
        Self {
            devices: Vec::new(),
            #[cfg(feature = "dev-hooks")]
            test_device_tx: None,
            #[cfg(feature = "dev-hooks")]
            test_device_counter: 0,
            #[cfg(feature = "dev-hooks")]
            current_test_sysname: None,
            #[cfg(feature = "dev-hooks")]
            mock_states: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl RatbagManager {
    /// Register a new device path (called when udev detects a device).
    pub fn add_device(&mut self, path: String) {
        self.devices.push(path);
    }

    /// Remove a device path (called when udev detects removal).
    pub fn remove_device(&mut self, path: &str) {
        self.devices.retain(|p| p != path);
    }

    /// Wire up the test device channel.
    ///
    /// Must be called before `LoadTestDevice` will function.
    #[cfg(feature = "dev-hooks")]
    pub fn set_test_device_tx(&mut self, tx: mpsc::Sender<DeviceAction>) {
        self.test_device_tx = Some(tx);
    }

    /// Return a clone of the shared mock state map so the event loop
    /// can insert entries when `InjectTestWithDriver` succeeds.
    #[cfg(feature = "dev-hooks")]
    pub fn mock_states_handle(&self) -> Arc<Mutex<HashMap<String, Arc<Mutex<MockState>>>>> {
        Arc::clone(&self.mock_states)
    }
}

#[interface(name = "org.freedesktop.ratbag1.Manager")]
impl RatbagManager {
    /// The DBus API version (constant, read-only).
    #[zbus(property, name = "APIVersion")]
    fn api_version(&self) -> i32 {
        API_VERSION
    }

    /// Array of object paths to the connected devices.
    #[zbus(property)]
    fn devices(&self) -> Vec<ObjectPath<'static>> {
        self.devices
            .iter()
            .filter_map(|p| ObjectPath::try_from(p.clone()).ok())
            .collect()
    }

    /// Load a synthetic test device from a JSON description.
    ///
    /// The JSON format mirrors the C `ratbagd-json.c` schema.
    /// An empty string `""` produces the minimum sane one-profile device.
    ///
    /// Only available when built with `--features dev-hooks`.
    #[cfg(feature = "dev-hooks")]
    async fn load_test_device(&mut self, json: String) -> zbus::fdo::Result<String> {
        use crate::test_device::spec::{build_device_info, parse_json};

        let spec = parse_json(&json).map_err(|e| {
            warn!("LoadTestDevice: JSON parse error: {e}");
            zbus::fdo::Error::InvalidArgs(format!("Invalid device JSON: {e}"))
        })?;

        let sysname = format!("testdevice{}", self.test_device_counter);
        self.test_device_counter += 1;

        let device_info = build_device_info(&sysname, spec);

        info!(
            "LoadTestDevice: injecting '{}' ({} profile(s))",
            sysname,
            device_info.profiles.len()
        );

        let Some(tx) = &self.test_device_tx else {
            warn!("LoadTestDevice: test_device_tx not configured");
            return Err(zbus::fdo::Error::Failed(
                "dev-hooks channel not initialised".into(),
            ));
        };

        /* Remove any previously-injected test device first */
        if let Some(old) = self.current_test_sysname.take() {
            let _ = tx.send(DeviceAction::RemoveTest { sysname: old }).await;
        }

        self.current_test_sysname = Some(sysname.clone());

        tx.send(DeviceAction::InjectTest {
            sysname: sysname.clone(),
            device_info,
        })
        .await
        .map_err(|e| {
            warn!("LoadTestDevice: channel send failed: {e}");
            zbus::fdo::Error::Failed("Internal send error".into())
        })?;

        Ok(sysname)
    }

    /// Remove the currently-live synthetic test device.
    ///
    /// A no-op if no test device is loaded.
    ///
    /// Only available when built with `--features dev-hooks`.
    #[cfg(feature = "dev-hooks")]
    async fn reset_test_device(&mut self) -> zbus::fdo::Result<()> {
        let Some(sysname) = self.current_test_sysname.take() else {
            return Ok(());
        };

        info!("ResetTestDevice: removing '{sysname}'");

        let Some(tx) = &self.test_device_tx else {
            return Err(zbus::fdo::Error::Failed(
                "dev-hooks channel not initialised".into(),
            ));
        };

        tx.send(DeviceAction::RemoveTest { sysname })
            .await
            .map_err(|e| {
                warn!("ResetTestDevice: channel send failed: {e}");
                zbus::fdo::Error::Failed("Internal send error".into())
            })?;

        Ok(())
    }

    /// Inject a test device that runs through the real driver's
    /// probe/load_profiles/commit path using a mock I/O backend.
    ///
    /// Arguments:
    /// - `driver_name`: driver identifier (e.g. "steelseries", "hidpp20")
    /// - `device_config_json`: JSON describing the DeviceInfo (same format as LoadTestDevice)
    /// - `io_script_json`: JSON describing the mock I/O exchanges
    ///
    /// Returns the sysname of the injected device.
    #[cfg(feature = "dev-hooks")]
    async fn load_test_device_with_driver(
        &mut self,
        driver_name: String,
        device_config_json: String,
        io_script_json: String,
    ) -> zbus::fdo::Result<String> {
        use crate::test_device::spec::{build_device_info, parse_json};

        let spec = parse_json(&device_config_json).map_err(|e| {
            warn!("LoadTestDeviceWithDriver: JSON parse error: {e}");
            zbus::fdo::Error::InvalidArgs(format!("Invalid device JSON: {e}"))
        })?;

        let sysname = format!("testdriver{}", self.test_device_counter);
        self.test_device_counter += 1;

        let device_info = build_device_info(&sysname, spec);

        info!(
            "LoadTestDeviceWithDriver: injecting '{}' driver={}",
            sysname, driver_name
        );

        let Some(tx) = &self.test_device_tx else {
            warn!("LoadTestDeviceWithDriver: test_device_tx not configured");
            return Err(zbus::fdo::Error::Failed(
                "dev-hooks channel not initialised".into(),
            ));
        };

        /* Remove any previously-injected test device first */
        if let Some(old) = self.current_test_sysname.take() {
            let _ = tx.send(DeviceAction::RemoveTest { sysname: old }).await;
        }

        self.current_test_sysname = Some(sysname.clone());

        tx.send(DeviceAction::InjectTestWithDriver {
            sysname: sysname.clone(),
            driver_name,
            device_info,
            io_script_json,
        })
        .await
        .map_err(|e| {
            warn!("LoadTestDeviceWithDriver: channel send failed: {e}");
            zbus::fdo::Error::Failed("Internal send error".into())
        })?;

        Ok(sysname)
    }

    /// Return the mock I/O write log for a test device as a JSON array
    /// of hex-encoded byte arrays.
    ///
    /// Example return: `[["10","01","81","00"],["11","01","83","63"]]`
    #[cfg(feature = "dev-hooks")]
    fn get_mock_io_log(&self, sysname: String) -> zbus::fdo::Result<String> {
        let states = self.mock_states.lock().unwrap();
        let Some(state) = states.get(&sysname) else {
            return Err(zbus::fdo::Error::InvalidArgs(format!(
                "No mock state for device '{sysname}'"
            )));
        };

        let st = state.lock().unwrap();
        let log: Vec<Vec<String>> = st
            .write_log
            .iter()
            .map(|report| report.iter().map(|b| format!("{b:02x}")).collect())
            .collect();

        serde_json::to_string(&log).map_err(|e| {
            zbus::fdo::Error::Failed(format!("JSON serialization error: {e}"))
        })
    }
}
