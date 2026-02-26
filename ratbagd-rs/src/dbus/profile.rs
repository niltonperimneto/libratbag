use std::sync::Arc;

use tokio::sync::RwLock;
use zbus::interface;
use zbus::zvariant::ObjectPath;

use crate::device::ProfileInfo;

/* The org.freedesktop.ratbag1.Profile interface. */
/*  */
/* Represents one of a device's configurable profiles, containing */
/* resolutions, buttons, and LEDs. */
pub struct RatbagProfile {
    info: Arc<RwLock<ProfileInfo>>,
    device_path: String,
}

impl RatbagProfile {
    pub fn new(info: ProfileInfo, device_path: String) -> Self {
        Self {
            info: Arc::new(RwLock::new(info)),
            device_path,
        }
    }
}

#[interface(name = "org.freedesktop.ratbag1.Profile")]
impl RatbagProfile {
    /* Zero-based profile index (constant). */
    #[zbus(property)]
    async fn index(&self) -> u32 {
        self.info.read().await.index
    }

    /* Profile name (read-write). Empty string means name cannot be changed. */
    #[zbus(property)]
    async fn name(&self) -> String {
        self.info.read().await.name.clone()
    }

    #[zbus(property)]
    async fn set_name(&self, name: String) {
        let mut info = self.info.write().await;
        info.name = name;
        info.is_dirty = true;
    }

    /* True if this profile is disabled. */
    #[zbus(property)]
    async fn disabled(&self) -> bool {
        !self.info.read().await.is_enabled
    }

    #[zbus(property)]
    async fn set_disabled(&self, disabled: bool) {
        let mut info = self.info.write().await;
        info.is_enabled = !disabled;
        info.is_dirty = true;
    }

    /* True if this is the active profile (read-only). */
    #[zbus(property)]
    async fn is_active(&self) -> bool {
        self.info.read().await.is_active
    }

    /* True if this profile has uncommitted changes. */
    #[zbus(property)]
    async fn is_dirty(&self) -> bool {
        self.info.read().await.is_dirty
    }

    /* Object paths to this profile's resolutions. */
    #[zbus(property)]
    async fn resolutions(&self) -> Vec<ObjectPath<'static>> {
        let info = self.info.read().await;
        info.resolutions
            .iter()
            .filter_map(|r| {
                let path = format!("{}/p{}/r{}", self.device_path, info.index, r.index);
                ObjectPath::try_from(path).ok()
            })
            .collect()
    }

    /* Object paths to this profile's buttons. */
    #[zbus(property)]
    async fn buttons(&self) -> Vec<ObjectPath<'static>> {
        let info = self.info.read().await;
        info.buttons
            .iter()
            .filter_map(|b| {
                let path = format!("{}/p{}/b{}", self.device_path, info.index, b.index);
                ObjectPath::try_from(path).ok()
            })
            .collect()
    }

    /* Object paths to this profile's LEDs. */
    #[zbus(property)]
    async fn leds(&self) -> Vec<ObjectPath<'static>> {
        let info = self.info.read().await;
        info.leds
            .iter()
            .filter_map(|l| {
                let path = format!("{}/p{}/l{}", self.device_path, info.index, l.index);
                ObjectPath::try_from(path).ok()
            })
            .collect()
    }

    /* Sensor angle snapping (-1 = unsupported, 0 = off, 1 = on). */
    #[zbus(property)]
    async fn angle_snapping(&self) -> i32 {
        self.info.read().await.angle_snapping
    }

    #[zbus(property)]
    async fn set_angle_snapping(&self, value: i32) {
        let mut info = self.info.write().await;
        info.angle_snapping = value;
        info.is_dirty = true;
    }

    /* Button debounce time in ms (-1 = unsupported). */
    #[zbus(property)]
    async fn debounce(&self) -> i32 {
        self.info.read().await.debounce
    }

    #[zbus(property)]
    async fn set_debounce(&self, value: i32) {
        let mut info = self.info.write().await;
        info.debounce = value;
        info.is_dirty = true;
    }

    /* Permitted debounce time values. */
    #[zbus(property)]
    async fn debounces(&self) -> Vec<u32> {
        self.info.read().await.debounces.clone()
    }

    /* Report rate in Hz. */
    #[zbus(property)]
    async fn report_rate(&self) -> u32 {
        self.info.read().await.report_rate
    }

    #[zbus(property)]
    async fn set_report_rate(&self, rate: u32) {
        let mut info = self.info.write().await;
        info.report_rate = rate;
        info.is_dirty = true;
    }

    /* Permitted report rate values. */
    #[zbus(property)]
    async fn report_rates(&self) -> Vec<u32> {
        self.info.read().await.report_rates.clone()
    }

    /* Set this profile as the active profile. */
    async fn set_active(&self) {
        let mut info = self.info.write().await;
        info.is_active = true;
        info.is_dirty = true;
        /* TODO: Deactivate other profiles via the device actor */
        tracing::info!("Profile {} set as active", info.index);
    }
}
