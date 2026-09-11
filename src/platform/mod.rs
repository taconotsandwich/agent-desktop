pub mod drivers;
pub mod gnome;
pub mod kde;
pub mod keymap;
pub mod registry;
pub mod wayland;
pub mod x11;

use drivers::{Desktop, SessionType};

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
