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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionType {
    Wayland,
    X11,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
/// model coords back before dispatch (Anthropic sweet spot ≤1568 long edge).
#[async_trait::async_trait]
pub trait ShotDriver: Send + Sync {
    fn id(&self) -> &'static str;
    async fn probe(&self) -> Probe;
    async fn capture(
        &self,
        target: ShotTarget,
        max_long_edge: u32,
    ) -> Result<Shot, ToolError>;
}

#[derive(Debug, Clone)]
pub enum ShotTarget {
    Full,
    Window(Ref),
    Element(Ref),
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
    async fn click(
        &self,
        x: i32,
        y: i32,
        button: Button,
        hold: Vec<crate::keymap::Modifier>,
    ) -> Result<(), ToolError>;
    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError>;
    async fn drag(&self, path: Vec<(i32, i32)>, button: Button) -> Result<(), ToolError>;
    async fn scroll(
        &self,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        hold: Vec<crate::keymap::Modifier>,
    ) -> Result<(), ToolError>;
    /// Literal text only. No modifiers (poka-yoke: use `key` for chords).
    async fn type_text(&self, text: String) -> Result<(), ToolError>;
    /// Named keys/chords only (e.g. "ctrl+s"). No text.
    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError>;
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
    pub geometry: Bbox,
    pub screen: u32,
    pub is_active: bool,
}
