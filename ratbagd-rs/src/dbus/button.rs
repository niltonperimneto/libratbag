use std::sync::Arc;

use tokio::sync::RwLock;
use zbus::interface;
use zbus::zvariant::OwnedValue;

use crate::device::{ActionType, ButtonInfo};

/* Fallback OwnedValue for when serialization fails. */
fn fallback_owned_value() -> OwnedValue {
    OwnedValue::from(0u32)
}

/* The org.freedesktop.ratbag1.Button interface. */
/*  */
/* Represents one physical button on a mouse within a given profile. */
pub struct RatbagButton {
    info: Arc<RwLock<ButtonInfo>>,
}

impl RatbagButton {
    pub fn new(info: ButtonInfo) -> Self {
        Self {
            info: Arc::new(RwLock::new(info)),
        }
    }
}

#[interface(name = "org.freedesktop.ratbag1.Button")]
impl RatbagButton {
    /* Zero-based button index (constant). */
    #[zbus(property)]
    async fn index(&self) -> u32 {
        self.info.read().await.index
    }

    /* Current button mapping as (ActionType, Variant). */
    /*  */
    /* ActionType determines the variant format: */
    /* - Button (1): u32 button number */
    /* - Special (2): u32 special value */
    /* - Key (3): u32 keycode */
    /* - Macro (4): Vec<(u32, u32)> key events */
    /* - None (0) / Unknown (1000): u32 with value 0 */
    #[zbus(property)]
    async fn mapping(&self) -> (u32, OwnedValue) {
        let info = self.info.read().await;
        let action_type = info.action_type as u32;

        let value: OwnedValue = match info.action_type {
            ActionType::Macro => {
                OwnedValue::try_from(zbus::zvariant::Value::from(
                    info.macro_entries.clone(),
                ))
                .unwrap_or_else(|_| fallback_owned_value())
            }
            _ => {
                OwnedValue::try_from(zbus::zvariant::Value::from(
                    info.mapping_value,
                ))
                .unwrap_or_else(|_| fallback_owned_value())
            }
        };

        (action_type, value)
    }

    #[zbus(property)]
    async fn set_mapping(&self, mapping: (u32, OwnedValue)) {
        let (action_type_raw, value) = mapping;
        let mut info = self.info.write().await;

        info.action_type = match action_type_raw {
            0 => ActionType::None,
            1 => ActionType::Button,
            2 => ActionType::Special,
            3 => ActionType::Key,
            4 => ActionType::Macro,
            _ => ActionType::Unknown,
        };

        /* Convert OwnedValue → Value once; prevents a use-after-move */
        let inner: zbus::zvariant::Value<'_> = value.into();

        match info.action_type {
            ActionType::Macro => {
                if let zbus::zvariant::Value::Array(arr) = &inner {
                    let entries: Vec<(u32, u32)> = arr
                        .iter()
                        .filter_map(|v| {
                            if let zbus::zvariant::Value::Structure(s) = v {
                                let fields = s.fields();
                                if let [zbus::zvariant::Value::U32(a), zbus::zvariant::Value::U32(b)] = fields {
                                    return Some((*a, *b));
                                }
                            }
                            None
                        })
                        .collect();
                    info.macro_entries = entries;
                }
            }
            _ => {
                if let zbus::zvariant::Value::U32(val) = &inner {
                    info.mapping_value = *val;
                }
            }
        }
    }

    /* Supported action types for this button (constant). */
    #[zbus(property)]
    async fn action_types(&self) -> Vec<u32> {
        self.info.read().await.action_types.clone()
    }
}
