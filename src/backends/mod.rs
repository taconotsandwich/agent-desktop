//! Backend constructors + session/desktop detection.
//!
//! Detection: `XDG_SESSION_TYPE` (wayland/x11) × `XDG_CURRENT_DESKTOP`
//! (KDE/GNOME). Override with `AGENT_DESKTOP_BACKEND=kwin|gnome|x11|fake`
//! for tests and nested sessions.

pub mod kwin_input;
pub mod kwin_shot;
pub mod kwin_windows;

pub use kwin_input::KwinInput;
pub use kwin_shot::KwinShot;
pub use kwin_windows::KwinWindows;

use crate::drivers::{Desktop, SessionType};

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
