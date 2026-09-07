//! Backend constructors + session/desktop detection.
//!
//! Detection: `XDG_SESSION_TYPE` (wayland/x11) × `XDG_CURRENT_DESKTOP`
//! (KDE/GNOME). Override with `AGENT_DESKTOP_BACKEND=kwin|gnome|x11|fake`
//! for tests and nested sessions.

pub mod gnome_windows;
pub mod kwin_input;
pub mod kwin_shot;
pub mod kwin_windows;
pub mod portal;
pub mod portal_input;
pub mod x11;

pub use gnome_windows::GnomeWindows;
pub use kwin_input::KwinInput;
pub use kwin_shot::KwinShot;
pub use kwin_windows::KwinWindows;
pub use portal::{PortalShot, portal_present};
pub use portal_input::PortalInput;
pub use x11::{X11Input, X11Shot, X11Windows};

use crate::drivers::{Desktop, Probe, SessionType};

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

pub fn detect_session() -> SessionType {
    match std::env::var("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "x11" => SessionType::X11,
        _ if std::env::var("WAYLAND_DISPLAY").is_ok() => SessionType::Wayland,
        _ if std::env::var("DISPLAY").is_ok() => SessionType::X11,
        _ => SessionType::Wayland,
    }
}

pub fn detect_desktop() -> Desktop {
    let cur = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_lowercase();
    if cur.contains("kde") {
        Desktop::Kde
    } else if cur.contains("gnome") {
        Desktop::Gnome
    } else {
        Desktop::Other
    }
}
