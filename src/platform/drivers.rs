//! Driver traits: the wayland/x11 × kde/gnome split point.
//!
//! Two axes compose here, they never leak into the MCP surface:
//! - display protocol: Wayland (EIS / portal RemoteDesktop) vs X11 (xdotool)
//! - compositor: KWin scripting vs GNOME Shell ext/introspect vs wmctrl/EWMH
//!
//! A11y (AT-SPI2) and clipboard (wl-* vs xclip) are shared helpers, one impl
//! each, selected by session type — not per-compositor code.

pub use crate::types::{Bbox, Button, Ref, ToolError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum SessionType {
    Wayland,
    X11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum Desktop {
    Kde,
    Gnome,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub id: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Pixels. Native desktop coordinates; engine downscales previews and maps
/// model coordinates back before dispatch.
#[async_trait::async_trait]
pub trait ShotDriver: Send + Sync {
    fn id(&self) -> &'static str;
    fn window_capture(&self) -> bool {
        true
    }
    async fn probe(&self) -> Probe;
    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError>;
}

#[derive(Debug, Clone)]
pub enum ShotTarget {
    Full,
    Area(Bbox),
    Window { id: Ref, geometry: Bbox },
}

#[derive(Debug, Clone)]
pub struct Shot {
    pub bytes: Vec<u8>,
    pub format: ShotFormat,
    /// Desktop-coordinate size the bytes map to (post-scale mapping base).
    pub coord_w: u32,
    pub coord_h: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum ShotFormat {
    Png,
    Jpeg,
}

/// Pointer + keyboard as one driver: on Wayland both come from the same
/// token (EIS fd / portal session); splitting them invites half-sessions.
#[async_trait::async_trait]
pub trait InputDriver: Send + Sync {
    fn id(&self) -> &'static str;
    async fn probe(&self) -> Probe;
    /// Establish devices before the caller focuses the target: creating a
    /// virtual pointer can change the compositor's focus and pointer picking.
    async fn prepare(&self) -> Result<(), ToolError> {
        Ok(())
    }
    async fn click(
        &self,
        x: i32,
        y: i32,
        button: Button,
        hold: Vec<crate::platform::keymap::Modifier>,
    ) -> Result<(), ToolError>;
    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError>;
    /// Pixel drag. `dwell_ms` pauses with the button held before moving
    /// (lets apps initiate box-select/orbit instead of treating a fast flick
    /// as a click); `step_ms` paces interpolated motions. Server supplies
    /// defaults (300/15) when the caller omits them.
    async fn drag(
        &self,
        path: Vec<(i32, i32)>,
        button: Button,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), ToolError>;
    async fn scroll(
        &self,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        hold: Vec<crate::platform::keymap::Modifier>,
    ) -> Result<(), ToolError>;
    /// Literal text only. No modifiers (poka-yoke: use `key` for chords).
    async fn type_text(&self, text: String) -> Result<(), ToolError>;
    /// Named keys/chords only (e.g. "ctrl+s"). No text.
    async fn key(&self, keys: Vec<crate::platform::keymap::Chord>) -> Result<(), ToolError>;
}

/// Window-addressed input. Events go to the client that owns the window:
/// the window is never activated and the real pointer never moves. Only X11
/// clients can be addressed (X11 sessions, Xwayland windows on Wayland
/// sessions); `resolve` says whether a window qualifies. Coordinates are
/// desktop pixels, exactly like `InputDriver`.
#[async_trait::async_trait]
pub trait TargetedInputDriver: Send + Sync {
    fn id(&self) -> &'static str;
    async fn probe(&self) -> Probe;
    /// The X11 window behind `window`, or `None` when this driver cannot
    /// address it (a native Wayland client).
    async fn resolve(&self, window: &WindowInfo) -> Result<Option<Ref>, ToolError>;
    /// `count` presses at (x, y), paced inside the toolkit's double-click
    /// interval. The point must lie inside the window's client area.
    async fn click(
        &self,
        target: &Ref,
        x: i32,
        y: i32,
        button: Button,
        count: u32,
    ) -> Result<(), ToolError>;
    /// Same contract as `InputDriver::drag`; the first point must lie inside
    /// the window's client area.
    async fn drag(
        &self,
        target: &Ref,
        path: Vec<(i32, i32)>,
        button: Button,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), ToolError>;
    async fn scroll(&self, target: &Ref, x: i32, y: i32, dx: i32, dy: i32)
    -> Result<(), ToolError>;
    /// Literal text only; characters the keymap cannot produce fail with
    /// `unsupported` before anything is sent.
    async fn type_text(&self, target: &Ref, text: String) -> Result<(), ToolError>;
    /// Named keys/chords only, each held for `hold`.
    async fn key(
        &self,
        target: &Ref,
        keys: Vec<crate::platform::keymap::Chord>,
        hold: std::time::Duration,
    ) -> Result<(), ToolError>;
}

#[async_trait::async_trait]
pub trait WindowDriver: Send + Sync {
    fn id(&self) -> &'static str;
    async fn probe(&self) -> Probe;
    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError>;
    async fn focus(&self, id: &Ref) -> Result<(), ToolError>;
    async fn minimize(&self, id: &Ref) -> Result<(), ToolError>;
    async fn maximize(&self, id: &Ref) -> Result<(), ToolError>;
    async fn restore(&self, id: &Ref) -> Result<(), ToolError>;
    async fn close(&self, id: &Ref) -> Result<(), ToolError>;
    async fn move_resize(&self, id: &Ref, geo: Bbox) -> Result<(), ToolError>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub window_ref: Ref,
    pub title: String,
    pub class: String,
    pub app_id: String,
    pub pid: Option<u32>,
    pub minimized: bool,
    pub client_protocol: Option<String>,
    pub geometry: Bbox,
    pub screen: u32,
    pub is_active: bool,
}
