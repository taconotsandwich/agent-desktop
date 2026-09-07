//! Compositor-agnostic MCP-facing shapes.
//!
//! Ports kde-mcp spec §4 verbatim: a11y-first, response-as-instruction,
//! poka-yoke opaque refs, trust-tagged externals. No KWin/GNOME/X11 types here.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Opaque handle minted by observe/inspect/window_query. Must come from a
/// recent call in this session; fabricated/stale refs fail with fresh state
/// inline (poka-yoke, spec §4.5).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Ref(pub String);

/// Provenance of every externally-visible string (spec §4.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Trust {
    Trusted,
    AgentAuthored,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct TrustedText {
    pub trust: Trust,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Bbox {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Side-effect summary returned by every mutating tool (spec §4.2).
/// Empty fields are omitted, never `[]` (spec §4.3).
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Diff {
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub windows_added: Vec<WindowBrief>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub windows_removed: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub focus_changed: Option<WindowBrief>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub vision_fallback: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WindowBrief {
    pub window_ref: Ref,
    pub title: TrustedText,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
