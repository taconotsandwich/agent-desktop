mod authorization;
mod connector;
mod screenshot;
mod windows;
pub use authorization::{application_entry, authorize_private_session, register_application};
pub use connector::KwinConnector;
pub use screenshot::KwinShot;
pub use windows::KwinWindows;

use crate::platform::drivers::Probe;

/// Shared KWin presence check: `org.kde.KWin` owning a session-bus name.
pub async fn kwin_probe(bus: &zbus::Connection, id: &'static str) -> Probe {
    let detail = async || -> Result<bool, String> {
        let proxy = zbus::fdo::DBusProxy::new(bus)
            .await
            .map_err(|e| e.to_string())?;
        let names = proxy.list_names().await.map_err(|e| e.to_string())?;
        Ok(names.iter().any(|n| n.as_str() == "org.kde.KWin"))
    };
    match detail().await {
        Ok(true) => Probe {
            id,
            ok: true,
            detail: "org.kde.KWin on session bus".into(),
        },
        Ok(false) => Probe {
            id,
            ok: false,
            detail: "org.kde.KWin not on session bus".into(),
        },
        Err(e) => Probe {
            id,
            ok: false,
            detail: format!("dbus list_names: {e}"),
        },
    }
}
