use std::sync::Arc;

use tokio::sync::RwLock;
use zbus::interface;
use zbus::zvariant::OwnedValue;

use crate::device::{Dpi, ResolutionInfo};

/* The org.freedesktop.ratbag1.Resolution interface. */
/*  */
/* Represents one resolution preset within a profile. */
pub struct RatbagResolution {
    info: Arc<RwLock<ResolutionInfo>>,
}

impl RatbagResolution {
    pub fn new(info: ResolutionInfo) -> Self {
        Self {
            info: Arc::new(RwLock::new(info)),
        }
    }
}

/* Fallback OwnedValue for when serialization fails. */
/* Uses a u32(0) which is always serializable. */
fn fallback_owned_value() -> OwnedValue {
    /* u32 → Value → OwnedValue is infallible in practice */
    OwnedValue::from(0u32)
}

#[interface(name = "org.freedesktop.ratbag1.Resolution")]
impl RatbagResolution {
    /* Zero-based resolution index (constant). */
    #[zbus(property)]
    async fn index(&self) -> u32 {
        self.info.read().await.index
    }

    /* Resolution capabilities (constant). */
    #[zbus(property)]
    async fn capabilities(&self) -> Vec<u32> {
        self.info.read().await.capabilities.clone()
    }

    /* Whether this is the active resolution (read-only). */
    #[zbus(property)]
    async fn is_active(&self) -> bool {
        self.info.read().await.is_active
    }

    /* Whether this is the default resolution (read-only). */
    #[zbus(property)]
    async fn is_default(&self) -> bool {
        self.info.read().await.is_default
    }

    /* Whether this resolution is disabled (read-write). */
    #[zbus(property)]
    async fn is_disabled(&self) -> bool {
        self.info.read().await.is_disabled
    }

    #[zbus(property)]
    async fn set_is_disabled(&self, disabled: bool) {
        self.info.write().await.is_disabled = disabled;
    }

    /* DPI value as a variant: either a u32 or a (u32, u32) tuple. */
    #[zbus(property)]
    async fn resolution(&self) -> OwnedValue {
        let info = self.info.read().await;
        match info.dpi {
            Dpi::Unified(val) => {
                OwnedValue::try_from(zbus::zvariant::Value::from(val))
                    .unwrap_or_else(|_| fallback_owned_value())
            }
            Dpi::Separate { x, y } => {
                OwnedValue::try_from(zbus::zvariant::Value::from((x, y)))
                    .unwrap_or_else(|_| fallback_owned_value())
            }
            Dpi::Unknown => fallback_owned_value(),
        }
    }

    #[zbus(property)]
    async fn set_resolution(&self, value: OwnedValue) {
        let mut info = self.info.write().await;
        let inner: zbus::zvariant::Value<'_> = value.into();

        match &inner {
            zbus::zvariant::Value::U32(val) => {
                info.dpi = Dpi::Unified(*val);
            }
            zbus::zvariant::Value::Structure(s) => {
                let fields = s.fields();
                if let [zbus::zvariant::Value::U32(x), zbus::zvariant::Value::U32(y)] = fields {
                    info.dpi = Dpi::Separate { x: *x, y: *y };
                } else {
                    tracing::warn!("Invalid structure in resolution value");
                }
            }
            _ => {
                tracing::warn!("Invalid resolution value received over DBus");
            }
        }
    }

    /* List of supported DPI values (constant). */
    #[zbus(property)]
    async fn resolutions(&self) -> Vec<u32> {
        self.info.read().await.dpi_list.clone()
    }

    /* Set this resolution as the active one. */
    async fn set_active(&self) {
        let mut info = self.info.write().await;
        tracing::info!("Resolution {} set as active", info.index);
        info.is_active = true;
    }

    /* Set this resolution as the default one. */
    async fn set_default(&self) {
        let mut info = self.info.write().await;
        tracing::info!("Resolution {} set as default", info.index);
        info.is_default = true;
    }
}
