use std::sync::Arc;

use tokio::sync::RwLock;
use zbus::interface;

use crate::device::LedInfo;

/* The org.freedesktop.ratbag1.Led interface. */
/*  */
/* Represents one LED on a mouse within a given profile. */
pub struct RatbagLed {
    info: Arc<RwLock<LedInfo>>,
}

impl RatbagLed {
    pub fn new(info: LedInfo) -> Self {
        Self {
            info: Arc::new(RwLock::new(info)),
        }
    }
}

#[interface(name = "org.freedesktop.ratbag1.Led")]
impl RatbagLed {
    /* Zero-based LED index (constant). */
    #[zbus(property)]
    async fn index(&self) -> u32 {
        self.info.read().await.index
    }

    /* Current LED mode (read-write). */
    #[zbus(property)]
    async fn mode(&self) -> u32 {
        self.info.read().await.mode
    }

    #[zbus(property)]
    async fn set_mode(&self, mode: u32) {
        self.info.write().await.mode = mode;
    }

    /* Supported LED modes (constant). */
    #[zbus(property)]
    async fn modes(&self) -> Vec<u32> {
        self.info.read().await.modes.clone()
    }

    /* LED color as an RGB triplet (read-write). */
    #[zbus(property)]
    async fn color(&self) -> (u32, u32, u32) {
        let info = self.info.read().await;
        (info.color.red, info.color.green, info.color.blue)
    }

    #[zbus(property)]
    async fn set_color(&self, color: (u32, u32, u32)) {
        let mut info = self.info.write().await;
        info.color.red = color.0;
        info.color.green = color.1;
        info.color.blue = color.2;
    }

    /* Color depth enum (constant). */
    #[zbus(property)]
    async fn color_depth(&self) -> u32 {
        self.info.read().await.color_depth
    }

    /* Effect duration in ms, range 0-10000 (read-write). */
    #[zbus(property)]
    async fn effect_duration(&self) -> u32 {
        self.info.read().await.effect_duration
    }

    #[zbus(property)]
    async fn set_effect_duration(&self, duration: u32) {
        self.info.write().await.effect_duration = duration;
    }

    /* LED brightness, 0-255 (read-write). */
    #[zbus(property)]
    async fn brightness(&self) -> u32 {
        self.info.read().await.brightness
    }

    #[zbus(property)]
    async fn set_brightness(&self, brightness: u32) {
        self.info.write().await.brightness = brightness;
    }
}
