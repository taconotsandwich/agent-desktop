//! The rmcp wire surface: kde-mcp's 12 tools + `doctor` (13th, required
//! here because the registry must report which backend won per capability).
//!
//! One `#[tool_router] impl` block (rmcp does not compose across blocks).
//! Handlers are thin: validate refs → dispatch to registry drivers → return
//! structured JSON. No compositor code here. Approvals/policy deferred.

use crate::a11y::{self, AtspiConnection, WalkOpts};
use crate::backends::kwin_windows::run_script;
use crate::clip;
use crate::core::RefStore;
use crate::drivers::{Button, SessionType, ShotTarget, WindowInfo};
use crate::error::BackendError;
use crate::keymap::Modifier;
use crate::registry::Registry;
use crate::types::{Diff, Ref};
use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
use atspi::proxy::proxy_ext::ProxyExt;
use base64::Engine as _;
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
use std::time::Duration;

#[derive(Clone)]
pub struct AgentDesktop {
    pub refs: Arc<RefStore>,
    pub registry: Arc<tokio::sync::RwLock<Option<Registry>>>,
    pub bus: zbus::Connection,
    pub atspi: Arc<AtspiConnection>,
    pub session: SessionType,
    pub tool_router: ToolRouter<AgentDesktop>,
}

impl AgentDesktop {
    pub fn new(
        refs: Arc<RefStore>,
        bus: zbus::Connection,
        atspi: Arc<AtspiConnection>,
        session: SessionType,
    ) -> Self {
        Self {
            refs,
            registry: Arc::new(tokio::sync::RwLock::new(None)),
            bus,
            atspi,
            session,
            tool_router: Self::tool_router(),
        }
    }
}

fn ok(v: serde_json::Value) -> Result<CallToolResult, McpError> {
    let content = Content::json(v).map_err(|e| McpError::internal_error(e.to_string(), None))?;
    Ok(CallToolResult::success(vec![content]))
}

fn stale(what: &str) -> Result<CallToolResult, McpError> {
    ok(json!({"ok": false,
        "error": {"code": "stale_handle", "message": format!("{what}; re-observe"), "retryable": false},
        "diff": Diff::default()}))
}

fn fail(e: BackendError, retryable: bool) -> Result<CallToolResult, McpError> {
    let t = e.tool(retryable);
    ok(json!({"ok": false,
        "error": {"code": t.code, "message": t.message, "retryable": t.retryable},
        "diff": Diff::default()}))
}

/// Clone the probed registry, if any.
async fn registry_of(slf: &AgentDesktop) -> Option<Registry> {
    slf.registry.read().await.as_ref().cloned()
}

fn no_backend() -> Result<CallToolResult, McpError> {
    ok(json!({"ok": false,
        "error": {"code": "no_backend", "message": "no compositor probed yet", "retryable": false}}))
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
    /// full | window:<ref>
    pub region: Option<String>,
    pub max_long_edge: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ReadTextArgs {
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

// ---- Codex-identical computer actions ----

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClickArgs {
    pub x: i32,
    pub y: i32,
    pub button: Option<Button>,
    /// Held modifiers, e.g. ["ctrl"] for Ctrl+click.
    pub keys: Option<Vec<String>>,
    /// 1 (default), 2 = double-click, 3 = triple-click.
    pub count: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MoveArgs {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DragArgs {
    /// Ordered pixel path; first point is the press location.
    pub path: Vec<(i32, i32)>,
    pub button: Option<Button>,
    /// Pause with the button held before moving (default 300ms; lets apps
    /// initiate the gesture instead of seeing a fast flick as a click).
    pub dwell_ms: Option<u64>,
    /// Pacing between interpolated motions (default 15ms).
    pub step_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ScrollArgs {
    pub x: i32,
    pub y: i32,
    pub scroll_x: Option<i32>,
    pub scroll_y: Option<i32>,
    pub keys: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TypeArgs {
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct KeypressArgs {
    pub keys: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaitArgs {
    /// Pause in milliseconds (default 2000, max 30000).
    pub ms: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WindowControlArgs {
    pub window_ref: Ref,
    /// focus | minimize | maximize | restore | close | move | resize
    pub action: String,
    pub geometry: Option<[i32; 4]>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClipboardSetArgs {
    pub text: String,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DoctorArgs {}

async fn query_windows(reg: &Registry) -> Result<Vec<WindowInfo>, BackendError> {
    reg.windows.query().await.map_err(|t| BackendError::Failed(t.message))
}

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
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        let wins = match query_windows(&reg).await {
            Ok(w) => w,
            Err(e) => return fail(e, true),
        };
        let ids: Vec<String> = wins.iter().map(|w| w.window_ref.0.clone()).collect();
        let minted = self.refs.mint_windows(ids).await;
        let windows: Vec<_> = wins
            .iter()
            .zip(minted)
            .map(|(w, r)| {
                json!({"ref": r.0,
                    "title": {"trust": "external", "text": &w.title},
                    "class": {"trust": "trusted", "text": &w.class},
                    "active": w.is_active})
            })
            .collect();
        ok(json!({"ok": true, "count": windows.len(), "windows": windows}))
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
            return stale("ref expired");
        }
        let conn = match self.atspi.get().await {
            Ok(c) => c,
            Err(e) => return fail(e, true),
        };
        let root = if self.refs.is_window(&args.target).await {
            match self.window_root(conn, &args.target).await {
                Ok(r) => r,
                Err(e) => return fail(e, true),
            }
        } else {
            match self.element_proxy(conn, &args.target).await {
                Ok(r) => r,
                Err(e) => return fail(e, false),
            }
        };
        let opts = WalkOpts {
            max_depth: args.max_depth.unwrap_or(3),
            max_elements: 200,
            role_filter: None,
            name_contains: None,
        };
        match a11y::walk(conn, root, self.refs.clone(), opts).await {
            Ok((elements, truncated, warnings)) => {
                ok(json!({"ok": true, "elements": elements, "truncated": truncated, "warnings": warnings}))
            }
            Err(e) => fail(e, true),
        }
    }

    #[tool(
        description = "Capture pixels as an MCP image block. Prefer observe/inspect — pixels are the escape hatch.",
        annotations(read_only_hint = true)
    )]
    async fn screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, McpError> {
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        let edge = args.max_long_edge.unwrap_or(1280).clamp(256, 1568);
        let region = args.region.unwrap_or_else(|| "full".into());
        if region != "full" {
            return ok(json!({"ok": false,
                "error": {"code": "unsupported", "message": "window/element-targeted capture lands with frameGeometry crop; use full for now", "retryable": false}}));
        }
        let shot = match reg.shot.capture(ShotTarget::Full, edge).await {
            Ok(s) => s,
            Err(t) => {
                return ok(json!({"ok": false,
                    "error": {"code": t.code, "message": t.message, "retryable": true}}));
            }
        };
        let b64 = base64::engine::general_purpose::STANDARD.encode(&shot.bytes);
        Ok(CallToolResult::success(vec![
            Content::image(b64, "image/png"),
            Content::text(
                json!({"coord_width": shot.coord_w, "coord_height": shot.coord_h}).to_string(),
            ),
        ]))
    }

    #[tool(
        description = "OCR a region. Use when AT-SPI exposes no text (canvas, PDF, image). Output is always trust: external.",
        annotations(read_only_hint = true)
    )]
    async fn read_text(
        &self,
        Parameters(_args): Parameters<ReadTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        ok(json!({"ok": false,
            "error": {"code": "not_implemented", "message": "read_text lands with the tesseract helper", "retryable": false}}))
    }

    #[tool(
        description = "List windows with class, title, geometry, screen.",
        annotations(read_only_hint = true)
    )]
    async fn window_query(
        &self,
        Parameters(args): Parameters<WindowQueryArgs>,
    ) -> Result<CallToolResult, McpError> {
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        let wins = match query_windows(&reg).await {
            Ok(w) => w,
            Err(e) => return fail(e, true),
        };
        let ids: Vec<String> = wins.iter().map(|w| w.window_ref.0.clone()).collect();
        let minted = self.refs.mint_windows(ids).await;
        let fmt = args.format.unwrap_or_else(|| "titles".into());
        let windows: Vec<_> = wins
            .iter()
            .zip(minted)
            .map(|(w, r)| match fmt.as_str() {
                "ids" => json!({"ref": r.0}),
                "full" => json!({"ref": r.0, "title": &w.title, "class": &w.class,
                    "geometry": [w.geometry.x, w.geometry.y, w.geometry.w, w.geometry.h],
                    "screen": w.screen, "active": w.is_active}),
                _ => json!({"ref": r.0, "title": &w.title, "class": &w.class, "active": w.is_active}),
            })
            .collect();
        ok(json!({"ok": true, "windows": windows}))
    }

    #[tool(description = "Read the clipboard.", annotations(read_only_hint = true))]
    async fn clipboard_read(
        &self,
        Parameters(_args): Parameters<ClipboardReadArgs>,
    ) -> Result<CallToolResult, McpError> {
        match clip::get(self.session).await {
            Ok(text) => ok(json!({"ok": true,
                "text": {"trust": "external", "text": text}})),
            Err(e) => fail(e, true),
        }
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
            return stale("ref expired");
        }
        if self.refs.is_window(&args.element_ref).await {
            return ok(json!({"ok": false,
                "error": {"code": "unsupported", "message": "ref is a window, not an element", "retryable": false}}));
        }
        let conn = match self.atspi.get().await {
            Ok(c) => c,
            Err(e) => return fail(e, true),
        };
        let proxy = match self.element_proxy(conn, &args.element_ref).await {
            Ok(p) => p,
            Err(e) => return fail(e, false),
        };
        let proxies = match proxy.proxies().await {
            Ok(p) => p,
            Err(e) => {
                return fail(
                    BackendError::BusDisconnected {
                        detail: e.to_string(),
                    },
                    true,
                );
            }
        };
        let action_proxy = match proxies.action().await {
            Ok(a) => a,
            Err(_) => {
                return ok(json!({"ok": false,
                    "error": {"code": "not_actionable", "message": "element has no Action interface; try mouse on bbox", "retryable": false}}));
            }
        };
        let actions = match action_proxy.get_actions().await {
            Ok(a) => a,
            Err(e) => {
                return fail(
                    BackendError::BusDisconnected {
                        detail: e.to_string(),
                    },
                    true,
                );
            }
        };
        let requested = args.action.to_ascii_lowercase();
        let idx = match actions
            .iter()
            .position(|a| a.name.eq_ignore_ascii_case(&requested))
        {
            Some(i) => i as i32,
            None => {
                return ok(json!({"ok": false,
                    "error": {"code": "not_actionable",
                        "message": format!("no action {requested:?}; available: {:?}",
                            actions.iter().map(|a| &a.name).collect::<Vec<_>>()),
                        "retryable": false}}));
            }
        };
        match action_proxy.do_action(idx).await {
            Ok(true) => ok(json!({"ok": true, "action_invoked": actions[idx as usize].name})),
            _ => ok(json!({"ok": false,
                "error": {"code": "not_actionable", "message": "AT-SPI do_action returned false", "retryable": true}})),
        }
    }

    // ---- Codex computer actions (identical names + shapes) ----

    #[tool(
        name = "click",
        description = "Click at coordinates. Vision fallback when AT-SPI is empty.",
        annotations(destructive_hint = true)
    )]
    async fn click_at(
        &self,
        Parameters(args): Parameters<ClickArgs>,
    ) -> Result<CallToolResult, McpError> {
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        let button = args.button.unwrap_or(Button::Left);
        let mut hold: Vec<Modifier> = Vec::new();
        for h in args.keys.unwrap_or_default() {
            match Modifier::parse(&h) {
                Ok(m) => hold.push(m),
                Err(e) => return fail(e, false),
            }
        }
        let count = args.count.unwrap_or(1).clamp(1, 3);
        let mut res: Result<(), BackendError> = Ok(());
        for _ in 0..count {
            res = reg
                .input
                .click(args.x, args.y, button, hold.clone())
                .await
                .map_err(|t| BackendError::Failed(t.message));
            if res.is_err() {
                break;
            }
        }
        match res {
            Ok(()) => ok(json!({"ok": true, "diff": {"vision_fallback": true}})),
            Err(e) => fail(e, true),
        }
    }

    #[tool(
        name = "move",
        description = "Move the cursor to coordinates without clicking.",
        annotations(destructive_hint = true)
    )]
    async fn move_to(
        &self,
        Parameters(args): Parameters<MoveArgs>,
    ) -> Result<CallToolResult, McpError> {
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        match reg.input.move_to(args.x, args.y).await {
            Ok(()) => ok(json!({"ok": true})),
            Err(t) => ok(json!({"ok": false,
                "error": {"code": t.code, "message": t.message, "retryable": true}})),
        }
    }

    #[tool(
        name = "drag",
        description = "Drag along a pixel path; first point is the press location.",
        annotations(destructive_hint = true)
    )]
    async fn drag_path(
        &self,
        Parameters(args): Parameters<DragArgs>,
    ) -> Result<CallToolResult, McpError> {
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        if args.path.len() < 2 {
            return fail(
                BackendError::Unsupported {
                    reason: "drag needs a path with ≥2 points".into(),
                },
                false,
            );
        }
        match reg
            .input
            .drag(
                args.path,
                args.button.unwrap_or(Button::Left),
                args.dwell_ms.unwrap_or(300).min(5000),
                args.step_ms.unwrap_or(15).clamp(1, 500),
            )
            .await
        {
            Ok(()) => ok(json!({"ok": true, "diff": {"vision_fallback": true}})),
            Err(t) => ok(json!({"ok": false,
                "error": {"code": t.code, "message": t.message, "retryable": true}})),
        }
    }

    #[tool(
        name = "scroll",
        description = "Scroll at coordinates. Positive scroll_y scrolls down.",
        annotations(destructive_hint = true)
    )]
    async fn scroll_at(
        &self,
        Parameters(args): Parameters<ScrollArgs>,
    ) -> Result<CallToolResult, McpError> {
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        let mut hold: Vec<Modifier> = Vec::new();
        for h in args.keys.unwrap_or_default() {
            match Modifier::parse(&h) {
                Ok(m) => hold.push(m),
                Err(e) => return fail(e, false),
            }
        }
        match reg
            .input
            .scroll(
                args.x,
                args.y,
                args.scroll_x.unwrap_or(0),
                args.scroll_y.unwrap_or(3),
                hold,
            )
            .await
        {
            Ok(()) => ok(json!({"ok": true})),
            Err(t) => ok(json!({"ok": false,
                "error": {"code": t.code, "message": t.message, "retryable": true}})),
        }
    }

    #[tool(
        name = "type",
        description = "Type literal text. No modifier keys (use keypress for chords).",
        annotations(destructive_hint = true)
    )]
    async fn type_text(
        &self,
        Parameters(args): Parameters<TypeArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.text.is_empty() {
            return fail(
                BackendError::Unsupported {
                    reason: "empty text".into(),
                },
                false,
            );
        }
        // Non-ASCII → clipboard paste path (evdev is US-QWERTY ASCII only).
        if !args.text.is_ascii() {
            return self.paste_text(&args.text).await;
        }
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        match reg.input.type_text(args.text).await {
            Ok(()) => ok(json!({"ok": true})),
            Err(t) => {
                if t.code == "unsupported" {
                    // Unmappable character → paste path.
                    return self.paste_text(&t.message).await;
                }
                ok(json!({"ok": false,
                    "error": {"code": t.code, "message": t.message, "retryable": true}}))
            }
        }
    }

    #[tool(
        name = "keypress",
        description = "Press named keys or chords (e.g. 'ctrl+s'). No text (use type for text).",
        annotations(destructive_hint = true)
    )]
    async fn press_keys(
        &self,
        Parameters(args): Parameters<KeypressArgs>,
    ) -> Result<CallToolResult, McpError> {
        if args.keys.is_empty() {
            return fail(
                BackendError::Unsupported {
                    reason: "empty chord".into(),
                },
                false,
            );
        }
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        match reg.input.key(args.keys).await {
            Ok(()) => ok(json!({"ok": true})),
            Err(t) => ok(json!({"ok": false,
                "error": {"code": t.code, "message": t.message, "retryable": false}})),
        }
    }

    #[tool(
        name = "wait",
        description = "Pause before the next action (lets UI settle).",
        annotations(read_only_hint = true)
    )]
    async fn wait_ms(
        &self,
        Parameters(args): Parameters<WaitArgs>,
    ) -> Result<CallToolResult, McpError> {
        let ms = args.ms.unwrap_or(2000).clamp(1, 30000);
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        ok(json!({"ok": true, "elapsed_ms": ms}))
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
            return stale("ref expired");
        }
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        let action = args.action.to_ascii_lowercase();
        let geo = args.geometry.map(|g| crate::drivers::Bbox {
            x: g[0],
            y: g[1],
            w: g[2].max(0) as u32,
            h: g[3].max(0) as u32,
        });
        let res = match action.as_str() {
            "focus" => reg.windows.focus(&args.window_ref).await,
            "minimize" => reg.windows.minimize(&args.window_ref).await,
            "maximize" => reg.windows.maximize(&args.window_ref).await,
            "restore" => reg.windows.restore(&args.window_ref).await,
            "close" => reg.windows.close(&args.window_ref).await,
            "move" | "resize" => match geo {
                Some(g) => reg.windows.move_resize(&args.window_ref, g).await,
                None => {
                    return fail(
                        BackendError::Unsupported {
                            reason: format!("geometry [x,y,w,h] required for {action}"),
                        },
                        false,
                    );
                }
            },
            other => {
                return fail(
                    BackendError::Unsupported {
                        reason: format!("unknown window action: {other}"),
                    },
                    false,
                );
            }
        };
        match res {
            Ok(()) => ok(json!({"ok": true})),
            Err(t) => ok(json!({"ok": false,
                "error": {"code": t.code, "message": t.message, "retryable": true}})),
        }
    }

    #[tool(description = "Set the clipboard contents.", annotations(destructive_hint = true))]
    async fn clipboard_set(
        &self,
        Parameters(args): Parameters<ClipboardSetArgs>,
    ) -> Result<CallToolResult, McpError> {
        match clip::set(self.session, &args.text).await {
            Ok(()) => ok(json!({"ok": true})),
            Err(e) => fail(e, true),
        }
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
            Some(r) => {
                let (a11y_ok, a11y_detail) = crate::a11y::status(&self.atspi).await;
                ok(json!({
                "ok": true,
                "session": format!("{:?}", self.session),
                "shot": r.shot.id(),
                "input": r.input.id(),
                "windows": r.windows.id(),
                "a11y_ok": a11y_ok,
                "a11y": a11y_detail,
                "probes": r.probes.iter().map(|p| json!({"id": p.id, "ok": p.ok, "detail": &p.detail})).collect::<Vec<_>>(),
            }))
            }
            None => ok(json!({"ok": false, "error": {"code": "no_backend", "message": "no compositor probed yet", "retryable": false}})),
        }
    }
}

impl AgentDesktop {
    /// Unicode / unmappable-text path: set both selections + Shift+Insert,
    /// restore old clipboard best-effort. Shift+Insert (not Ctrl+V) because
    /// terminals treat Ctrl+V as readline quoted-insert (literal ^M).
    async fn paste_text(&self, text: &str) -> Result<CallToolResult, McpError> {
        let old = clip::get(self.session).await.ok();
        if let Err(e) = clip::set_both(self.session, text).await {
            return fail(e, true);
        }
        let reg = match registry_of(self).await {
            Some(r) => r,
            None => return no_backend(),
        };
        if let Err(t) = reg.input.key(vec!["shift+Insert".into()]).await {
            return ok(json!({"ok": false,
                "error": {"code": t.code, "message": t.message, "retryable": true}}));
        }
        if let Some(prev) = old {
            let _ = clip::set(self.session, &prev).await;
        }
        ok(json!({"ok": true, "via": "clipboard_paste"}))
    }

    async fn element_proxy<'a>(
        &self,
        conn: &'a atspi::AccessibilityConnection,
        r: &Ref,
    ) -> Result<AccessibleProxy<'a>, BackendError> {
        // Walk() mints refs whose .0 IS the base64 bus|path payload. Window
        // refs (kwin:<uuid>) never reach here — callers check first.
        let (bus, path) = a11y::decode_element_ref(&r.0)
            .ok_or_else(|| BackendError::StaleHandle(format!("bad ref {}", r.0)))?;
        let bus_name = zbus::names::BusName::try_from(bus)
            .map_err(|e| BackendError::StaleHandle(format!("bad bus name: {e}")))?;
        let obj_path = zbus::zvariant::ObjectPath::try_from(path)
            .map_err(|e| BackendError::StaleHandle(format!("bad path: {e}")))?;
        AccessibleProxy::builder(conn.connection())
            .destination(bus_name)
            .map_err(|e| BackendError::BusDisconnected {
                detail: e.to_string(),
            })?
            .path(obj_path)
            .map_err(|e| BackendError::BusDisconnected {
                detail: e.to_string(),
            })?
            .cache_properties(atspi::zbus::proxy::CacheProperties::No)
            .build()
            .await
            .map_err(|e| BackendError::BusDisconnected {
                detail: e.to_string(),
            })
    }

    /// Window ref → owning app's AT-SPI root via pid match.
    async fn window_root<'a>(
        &self,
        conn: &'a atspi::AccessibilityConnection,
        r: &Ref,
    ) -> Result<AccessibleProxy<'a>, BackendError> {
        // Fresh query to map ref → kwin uuid → pid via a second script call.
        let reg = registry_of(self).await.ok_or(BackendError::Unavailable {
            backend: "windows",
            detail: "no compositor probed yet".into(),
        })?;
        let wins = query_windows(&reg).await?;
        let _ = wins;
        // pid lookup: KWin internalId → pid through a targeted script.
        let uuid = r
            .0
            .strip_prefix("kwin:")
            .ok_or_else(|| BackendError::StaleHandle(format!("not a kwin ref: {}", r.0)))?;
        let uuid = uuid.replace('\\', "\\\\").replace('\'', "\\'");
        let body = format!(
            "var ws = workspace; var list = ws.windowList(); \
             for (var i=0;i<list.length;i++) {{ \
               if (list[i].internalId && list[i].internalId.toString() === '{uuid}') \
                 {{ return {{ pid: list[i].pid || 0 }}; }} }} \
             return {{ pid: 0 }};"
        );
        let payload = run_script(&self.bus, &body, Duration::from_secs(5))
            .await?;
        let pid: i64 = serde_json::from_str::<serde_json::Value>(&payload)
            .ok()
            .and_then(|v| v.get("pid")?.as_i64())
            .unwrap_or(0);
        if pid <= 0 {
            return Err(BackendError::Failed("window has no pid; cannot map to AT-SPI app".into()));
        }
        // Walk registry children for the app whose bus owner pid matches.
        let root = conn.root_accessible_on_registry().await.map_err(|e| {
            BackendError::BusDisconnected {
                detail: format!("registry root: {e}"),
            }
        })?;
        let count = root.child_count().await.unwrap_or(0);
        for i in 0..count {
            let child_ref = match root.get_child_at_index(i).await {
                Ok(r) => r,
                Err(_) => continue,
            };
            let app_proxy = match child_ref.into_accessible_proxy(conn.connection()).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            let bus_name = app_proxy.inner().destination().to_string();
            if bus_owner_pid(conn.connection(), &bus_name).await == Some(pid) {
                return Ok(app_proxy);
            }
        }
        Err(BackendError::Failed(format!(
            "no AT-SPI application with pid {pid}; app may not be a11y-enabled"
        )))
    }
}

async fn bus_owner_pid(conn: &zbus::Connection, bus_name: &str) -> Option<i64> {
    let proxy = zbus::fdo::DBusProxy::new(conn).await.ok()?;
    let name = zbus::names::BusName::try_from(bus_name.to_string()).ok()?;
    proxy
        .get_connection_unix_process_id(name)
        .await
        .ok()
        .map(|p| p as i64)
}

#[tool_handler]
impl ServerHandler for AgentDesktop {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }
}
