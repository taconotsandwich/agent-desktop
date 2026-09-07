//! The rmcp wire surface: kde-mcp's 12 tools + `doctor` (13th, required
//! here because the registry must report which backend won per capability).
//!
//! One `#[tool_router] impl` block (rmcp does not compose across blocks).
//! Handlers are thin: validate refs → dispatch to registry drivers → return
//! structured JSON. No compositor code here. Approvals/policy deferred.

use crate::core::RefStore;
use crate::drivers::Button;
use crate::registry::Registry;
use crate::types::{Diff, Ref};
use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

#[derive(Clone)]
pub struct AgentDesktop {
    pub refs: Arc<RefStore>,
    pub registry: Arc<tokio::sync::RwLock<Option<Registry>>>,
    pub tool_router: ToolRouter<AgentDesktop>,
}

impl AgentDesktop {
    pub fn new(refs: Arc<RefStore>) -> Self {
        Self {
            refs,
            registry: Arc::new(tokio::sync::RwLock::new(None)),
            tool_router: Self::tool_router(),
        }
    }
}

fn ok(v: serde_json::Value) -> Result<CallToolResult, McpError> {
    let content = Content::json(v).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![content]))
}

fn not_yet(what: &'static str) -> Result<CallToolResult, McpError> {
    ok(json!({"ok": false, "error": {"code": "not_implemented", "message": what, "retryable": false}}))
}

// ---- Observation (read-only) ----

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ObserveArgs {
    /// summary | interactive | full
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct InspectArgs {
    pub target: Ref,
    pub max_depth: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ScreenshotArgs {
    /// full | window:<ref> | element:<ref>
    pub region: Option<String>,
    pub max_long_edge: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadTextArgs {
    /// window:<ref> | element:<ref> | full
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WindowQueryArgs {
    /// ids | titles | full
    pub format: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClipboardReadArgs {}

// ---- Action (destructive) ----

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ActOnElementArgs {
    pub element_ref: Ref,
    pub action: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MouseArgs {
    /// click | move | drag | scroll
    pub op: String,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub button: Option<Button>,
    pub path: Option<Vec<(i32, i32)>>,
    pub dx: Option<i32>,
    pub dy: Option<i32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TypeArgs {
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct KeyArgs {
    pub keys: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WindowControlArgs {
    pub window_ref: Ref,
    /// focus | minimize | maximize | restore | close | move | resize
    pub action: String,
    pub geometry: Option<crate::drivers::Bbox>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClipboardSetArgs {
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DoctorArgs {}

#[tool_router]
impl AgentDesktop {
    // ---- Observation (read-only) ----

    #[tool(
        description = "Situational snapshot of the desktop. No screenshot. Reflex first call in unfamiliar state.",
        annotations(read_only_hint = true)
    )]
    async fn observe(
        &self,
        Parameters(_args): Parameters<ObserveArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("observe lands with the AT-SPI helper")
    }

    #[tool(
        description = "Drill into the AT-SPI tree of a window or element. Returns a flattened element list.",
        annotations(read_only_hint = true)
    )]
    async fn inspect(
        &self,
        Parameters(args): Parameters<InspectArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.refs.check(&args.target).await {
            return ok(json!({"ok": false, "error": {"code": "stale_handle", "message": "ref expired; re-observe", "retryable": false}, "diff": Diff::default()}));
        }
        not_yet("inspect lands with the AT-SPI helper")
    }

    #[tool(
        description = "Capture pixels as an MCP image block. Prefer observe/inspect — pixels are the escape hatch.",
        annotations(read_only_hint = true)
    )]
    async fn screenshot(
        &self,
        Parameters(_args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("screenshot lands with the shot drivers")
    }

    #[tool(
        description = "OCR a region. Use when AT-SPI exposes no text (canvas, PDF, image). Output is always trust: external.",
        annotations(read_only_hint = true)
    )]
    async fn read_text(
        &self,
        Parameters(_args): Parameters<ReadTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("read_text lands with the OCR helper")
    }

    #[tool(
        description = "List windows with class, title, geometry, screen.",
        annotations(read_only_hint = true)
    )]
    async fn window_query(
        &self,
        Parameters(_args): Parameters<WindowQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("window_query lands with the window drivers")
    }

    #[tool(description = "Read the clipboard.", annotations(read_only_hint = true))]
    async fn clipboard_read(
        &self,
        Parameters(_args): Parameters<ClipboardReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("clipboard_read lands with the clipboard helpers")
    }

    // ---- Action (destructive) ----

    #[tool(
        description = "Invoke a named action on an accessibility element by ref.",
        annotations(destructive_hint = true)
    )]
    async fn act_on_element(
        &self,
        Parameters(args): Parameters<ActOnElementArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.refs.check(&args.element_ref).await {
            return ok(json!({"ok": false, "error": {"code": "stale_handle", "message": "ref expired; re-observe", "retryable": false}, "diff": Diff::default()}));
        }
        not_yet("act_on_element lands with AT-SPI + input drivers")
    }

    #[tool(
        description = "Mouse click/move/scroll/drag at coordinates. Vision fallback when AT-SPI is empty.",
        annotations(destructive_hint = true)
    )]
    async fn mouse(
        &self,
        Parameters(_args): Parameters<MouseArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("mouse lands with the input drivers")
    }

    #[tool(
        name = "keyboard.type",
        description = "Type literal text. No modifier keys (use keyboard.key for chords).",
        annotations(destructive_hint = true)
    )]
    async fn keyboard_type(
        &self,
        Parameters(_args): Parameters<TypeArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("keyboard.type lands with the input drivers")
    }

    #[tool(
        name = "keyboard.key",
        description = "Press named keys or chords (e.g. 'ctrl+s'). No text (use keyboard.type for text).",
        annotations(destructive_hint = true)
    )]
    async fn keyboard_key(
        &self,
        Parameters(_args): Parameters<KeyArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("keyboard.key lands with the input drivers")
    }

    #[tool(
        description = "Mutate window state by ref: focus | minimize | maximize | restore | close | move | resize.",
        annotations(destructive_hint = true)
    )]
    async fn window_control(
        &self,
        Parameters(args): Parameters<WindowControlArgs>,
    ) -> Result<CallToolResult, McpError> {
        if !self.refs.check(&args.window_ref).await {
            return ok(json!({"ok": false, "error": {"code": "stale_handle", "message": "ref expired; re-query", "retryable": false}, "diff": Diff::default()}));
        }
        not_yet("window_control lands with the window drivers")
    }

    #[tool(description = "Set the clipboard contents.", annotations(destructive_hint = true))]
    async fn clipboard_set(
        &self,
        Parameters(_args): Parameters<ClipboardSetArgs>,
    ) -> Result<CallToolResult, McpError> {
        not_yet("clipboard_set lands with the clipboard helpers")
    }

    #[tool(
        description = "Readiness report: session, desktop, per-capability winning backend and blockers.",
        annotations(read_only_hint = true)
    )]
    async fn doctor(
        &self,
        Parameters(_args): Parameters<DoctorArgs>,
    ) -> Result<CallToolResult, McpError> {
        let guard = self.registry.read().await;
        match guard.as_ref() {
            Some(r) => ok(json!({
                "ok": true,
                "shot": r.shot.id(),
                "input": r.input.id(),
                "windows": r.windows.id(),
                "probes": r.probes.iter().map(|p| json!({"id": p.id, "ok": p.ok, "detail": &p.detail})).collect::<Vec<_>>(),
            })),
            None => ok(json!({"ok": false, "error": {"code": "no_backend", "message": "no compositor probed yet", "retryable": false}})),
        }
    }
}

#[tool_handler]
impl ServerHandler for AgentDesktop {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}
