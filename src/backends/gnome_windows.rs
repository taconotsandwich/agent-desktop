//! GNOME Shell windows → [`WindowDriver`].
//!
//! Order mirrors computer-use-linux: companion extension first (exact focus),
//! Introspect second (list-only, no exact focus). Refs are `gnome:<id>`;
//! pid comes from Introspect properties for the AT-SPI mapping.

use crate::drivers::{Bbox, Probe, Ref, WindowDriver, WindowInfo};
use crate::error::BackendError;
use crate::types::ToolError;
use std::collections::HashMap;
use zbus::Connection;
use zbus::proxy::Proxy;
use zvariant::OwnedValue;

pub struct GnomeWindows {
    bus: Connection,
}

impl GnomeWindows {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }

    async fn introspect(&self) -> Result<Proxy<'_>, BackendError> {
        Proxy::new(
            &self.bus,
            "org.gnome.Shell",
            "/org/gnome/Shell/Introspect",
            "org.gnome.Shell.Introspect",
        )
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("introspect proxy: {e}"),
        })
    }
}

fn get_str(m: &HashMap<String, OwnedValue>, k: &str) -> String {
    m.get(k)
        .and_then(|v| <&str>::try_from(v).ok())
        .unwrap_or("")
        .to_string()
}
fn get_u32(m: &HashMap<String, OwnedValue>, k: &str) -> u32 {
    m.get(k).and_then(|v| u32::try_from(v).ok()).unwrap_or(0)
}
fn get_i32(m: &HashMap<String, OwnedValue>, k: &str) -> i32 {
    m.get(k).and_then(|v| i32::try_from(v).ok()).unwrap_or(0)
}
fn get_bool(m: &HashMap<String, OwnedValue>, k: &str) -> bool {
    m.get(k).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

#[async_trait::async_trait]
impl WindowDriver for GnomeWindows {
    fn id(&self) -> &'static str {
        "gnome-introspect"
    }

    async fn probe(&self) -> Probe {
        match self.introspect().await {
            Ok(p) => match p.call::<_, _, HashMap<u64, HashMap<String, OwnedValue>>>("GetWindows", &()).await {
                Ok(_) => Probe {
                    id: self.id(),
                    ok: true,
                    detail: "GetWindows reachable (list-only; exact focus needs the companion extension)".into(),
                },
                Err(e) => Probe {
                    id: self.id(),
                    ok: false,
                    detail: format!("GetWindows denied: {e}"),
                },
            },
            Err(e) => Probe {
                id: self.id(),
                ok: false,
                detail: format!("no introspect: {}", e.tool(false).message),
            },
        }
    }

    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let proxy = self.introspect().await.map_err(|e| e.tool(true))?;
        let wins: HashMap<u64, HashMap<String, OwnedValue>> = proxy
            .call("GetWindows", &())
            .await
            .map_err(|e| {
                BackendError::BusDisconnected {
                    detail: format!("GetWindows: {e}"),
                }
                .tool(true)
            })?;
        let mut out: Vec<WindowInfo> = wins
            .into_iter()
            .map(|(id, props)| WindowInfo {
                window_ref: Ref(format!("gnome:{id}")),
                title: get_str(&props, "title"),
                class: {
                    let c = get_str(&props, "wm-class");
                    if c.is_empty() {
                        get_str(&props, "app-id")
                    } else {
                        c
                    }
                },
                geometry: Bbox {
                    x: get_i32(&props, "x"),
                    y: get_i32(&props, "y"),
                    w: get_u32(&props, "width"),
                    h: get_u32(&props, "height"),
                },
                screen: 0,
                is_active: get_bool(&props, "has-focus"),
            })
            .collect();
        out.sort_by_key(|w| w.window_ref.0.clone());
        Ok(out)
    }

    async fn focus(&self, id: &Ref) -> Result<(), ToolError> {
        // Introspect cannot focus by window id; FocusApp by app-id is the
        // available primitive and needs the app id, which refs don't carry.
        // Honest failure with the fix (companion extension), not a guess.
        let _ = id;
        Err(BackendError::Unsupported {
            reason: "exact focus needs the companion GNOME Shell extension (setup_window_targeting); introspect is list-only".into(),
        }
        .tool(false))
    }

    async fn minimize(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "introspect is list-only; needs the companion extension".into(),
        }
        .tool(false))
    }
    async fn maximize(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "introspect is list-only; needs the companion extension".into(),
        }
        .tool(false))
    }
    async fn restore(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "introspect is list-only; needs the companion extension".into(),
        }
        .tool(false))
    }
    async fn close(&self, _id: &Ref) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "introspect is list-only; needs the companion extension".into(),
        }
        .tool(false))
    }
    async fn move_resize(&self, _id: &Ref, _geo: Bbox) -> Result<(), ToolError> {
        Err(BackendError::Unsupported {
            reason: "introspect is list-only; needs the companion extension".into(),
        }
        .tool(false))
    }
}
