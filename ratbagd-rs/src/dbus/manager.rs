use std::sync::Arc;

use tokio::sync::RwLock;
use zbus::interface;

/* DBus API version. Must match the C daemon's value for client compatibility. */
pub const API_VERSION: i32 = 2;

/* The org.freedesktop.ratbag1.Manager interface. */
/*  */
/* This is the entry point for clients (Piper, ratbagctl) to discover */
/* connected devices. */
pub struct RatbagManager {
    devices: Arc<RwLock<Vec<String>>>,
}

impl Default for RatbagManager {
    fn default() -> Self {
        Self {
            devices: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl RatbagManager {
    /* Register a new device path (called when udev detects a device). */
    pub async fn add_device(&self, path: String) {
        self.devices.write().await.push(path);
    }

    /* Remove a device path (called when udev detects removal). */
    pub async fn remove_device(&self, path: &str) {
        self.devices.write().await.retain(|p| p != path);
    }
}

#[interface(name = "org.freedesktop.ratbag1.Manager")]
impl RatbagManager {
    /* The DBus API version (constant, read-only). */
    #[zbus(property)]
    async fn api_version(&self) -> i32 {
        API_VERSION
    }

    /* Array of object paths to the connected devices. */
    #[zbus(property)]
    async fn devices(&self) -> Vec<zbus::zvariant::ObjectPath<'static>> {
        self.devices
            .read()
            .await
            .iter()
            .filter_map(|p| zbus::zvariant::ObjectPath::try_from(p.as_str()).ok())
            .map(|p| p.to_owned())
            .collect()
    }
}
