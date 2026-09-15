mod input;
mod screenshot;
mod windows;
pub use input::X11Input;
pub use screenshot::X11Shot;
pub use windows::X11Windows;

use crate::error::BackendError;
use std::sync::{Arc, OnceLock};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{ConnectionExt as _, Window},
    rust_connection::RustConnection,
};

pub(super) fn display() -> Result<String, BackendError> {
    std::env::var("DISPLAY").map_err(|_| BackendError::Unavailable {
        backend: "x11",
        detail: "DISPLAY not set".into(),
    })
}

pub(super) struct X11 {
    pub connection: RustConnection,
    pub root: Window,
    pub root_width: u16,
    pub root_height: u16,
}

impl X11 {
    fn connect() -> Result<Self, BackendError> {
        let display = display()?;
        let (connection, screen_num) =
            x11rb::connect(Some(&display)).map_err(|error| BackendError::Unavailable {
                backend: "x11",
                detail: format!("{display}: {error}"),
            })?;
        let root = connection.setup().roots[screen_num].root;
        let geometry = connection
            .get_geometry(root)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        Ok(Self {
            connection,
            root,
            root_width: geometry.width,
            root_height: geometry.height,
        })
    }
}

static CONNECTION: OnceLock<Arc<X11>> = OnceLock::new();

/// One shared X11 connection for every driver. `DISPLAY` is fixed before the
/// server starts (join-seat runs first), so caching the first connection is
/// safe; a failed connect is not cached and is retried on the next call.
pub(super) fn connection() -> Result<Arc<X11>, BackendError> {
    if let Some(x11) = CONNECTION.get() {
        return Ok(x11.clone());
    }
    let x11 = Arc::new(X11::connect()?);
    let _ = CONNECTION.set(x11.clone());
    Ok(x11)
}

/// X11 failures keep the `external_command_failed` wire code the xdotool
/// implementation reported, so `retryable` semantics are unchanged.
pub(super) fn failed(error: impl std::fmt::Display) -> BackendError {
    BackendError::ExternalCommandFailed {
        stderr: format!("x11: {error}"),
    }
}
