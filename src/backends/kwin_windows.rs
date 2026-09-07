//! KWin Scripting window control → [`WindowDriver`].
//!
//! Caller-hosted D-Bus receiver pattern (ported from kde-mcp
//! `kwin/scripting.rs`): KWin's QJSEngine has no I/O, so each invocation
//! hosts a one-shot `ScriptSink1` object and the script calls back with
//! `result`/`error`. Windows are matched by `internalId`.

use crate::drivers::{Bbox, Probe, Ref, WindowDriver, WindowInfo};
use crate::error::BackendError;
use crate::types::ToolError;
use std::time::Duration;
use tokio::sync::mpsc;
use zbus::Connection;

#[zbus::proxy(
    interface = "org.kde.kwin.Scripting",
    default_service = "org.kde.KWin",
    default_path = "/Scripting"
)]
trait Scripting {
    #[zbus(name = "loadScript")]
    fn load_script(&self, file_path: &str, plugin_name: &str) -> zbus::Result<i32>;
    #[zbus(name = "unloadScript")]
    fn unload_script(&self, plugin_name: &str) -> zbus::Result<bool>;
}

#[zbus::proxy(
    interface = "org.kde.kwin.Script",
    default_service = "org.kde.KWin",
    default_path = "/Scripting/Script0"
)]
trait Script {
    #[zbus(name = "run")]
    fn run(&self) -> zbus::Result<()>;
}

struct ScriptSink {
    tx: mpsc::Sender<Result<String, String>>,
}

#[zbus::interface(name = "org.agentdesktop.ScriptSink1")]
impl ScriptSink {
    #[zbus(name = "result")]
    async fn result(&self, payload: String) {
        let _ = self.tx.send(Ok(payload)).await;
    }
    #[zbus(name = "error")]
    async fn error(&self, message: String) {
        let _ = self.tx.send(Err(message)).await;
    }
}

pub struct KwinWindows {
    bus: Connection,
}

impl KwinWindows {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }

    async fn run(&self, body: &str) -> Result<String, BackendError> {
        run_script(&self.bus, body, Duration::from_secs(10)).await
    }

    fn uuid_of(window_ref: &Ref) -> Result<&str, BackendError> {
        window_ref
            .0
            .strip_prefix("kwin:")
            .ok_or_else(|| BackendError::StaleHandle(format!("not a kwin ref: {}", window_ref.0)))
    }
}

#[async_trait::async_trait]
impl WindowDriver for KwinWindows {
    fn id(&self) -> &'static str {
        "kwin-scripting"
    }

    async fn probe(&self) -> Probe {
        super::kwin_probe(&self.bus, self.id()).await
    }

    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let body = r#"
            var ws = workspace;
            var windows = ws.windowList();
            var out = [];
            for (var i = 0; i < windows.length; i++) {
                var w = windows[i];
                var g = w.frameGeometry;
                var output = w.output;
                out.push({
                    uuid: w.internalId ? w.internalId.toString() : ("idx:" + i),
                    caption: w.caption || "",
                    resourceClass: w.resourceClass || "",
                    pid: w.pid || 0,
                    geometry: [g.x, g.y, g.width, g.height],
                    screen: output ? output.name : "",
                    active: (ws.activeWindow === w),
                    minimized: !!w.minimized
                });
            }
            return {windows: out, count: windows.length,
                    stacking: ws.stackingOrder.length,
                    activeCaption: ws.activeWindow ? ws.activeWindow.caption : ""};
        "#;
        let payload = self.run(body).await.map_err(|e| e.tool(true))?;
        let root: serde_json::Value =
            serde_json::from_str(&payload).map_err(|e| BackendError::ExternalCommandFailed {
                stderr: format!("window list parse: {e}"),
            }.tool(true))?;
        let items: Vec<serde_json::Value> = root
            .get("windows")
            .and_then(|w| w.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(parse_entries(&items))
    }

    async fn focus(&self, id: &Ref) -> Result<(), ToolError> {
        self.mutate(id, "focus", None).await
    }
    async fn minimize(&self, id: &Ref) -> Result<(), ToolError> {
        self.mutate(id, "minimize", None).await
    }
    async fn maximize(&self, id: &Ref) -> Result<(), ToolError> {
        self.mutate(id, "maximize", None).await
    }
    async fn restore(&self, id: &Ref) -> Result<(), ToolError> {
        self.mutate(id, "restore", None).await
    }
    async fn close(&self, id: &Ref) -> Result<(), ToolError> {
        self.mutate(id, "close", None).await
    }
    async fn move_resize(&self, id: &Ref, geo: Bbox) -> Result<(), ToolError> {
        self.mutate(id, "move_resize", Some(geo)).await
    }
}

/// Parse script payload entries. NOTE: KWin emits fractional geometry under
/// fractional scaling (e.g. 520.625) — parse as f64 and round, never strict
/// as_i64 (which silently drops scaled windows).
pub fn parse_entries(items: &[serde_json::Value]) -> Vec<WindowInfo> {
    items
        .iter()
        .filter_map(|v| {
            let g = v.get("geometry")?.as_array()?;
            let num = |n: &serde_json::Value| {
                n.as_f64()
                    .map(|f| f.round() as i32)
                    .or_else(|| n.as_i64().map(|i| i as i32))
            };
            let nums: Vec<i32> = g.iter().filter_map(num).collect();
            if nums.len() != 4 {
                return None;
            }
            Some(WindowInfo {
                window_ref: Ref(format!("kwin:{}", v.get("uuid")?.as_str().unwrap_or("?"))),
                title: v.get("caption")?.as_str().unwrap_or("").into(),
                class: v.get("resourceClass")?.as_str().unwrap_or("").into(),
                geometry: Bbox {
                    x: nums[0],
                    y: nums[1],
                    w: nums[2].max(0) as u32,
                    h: nums[3].max(0) as u32,
                },
                screen: 0,
                is_active: v.get("active")?.as_bool().unwrap_or(false),
            })
        })
        .collect()
}

impl KwinWindows {
    async fn mutate(&self, id: &Ref, action: &str, geo: Option<Bbox>) -> Result<(), ToolError> {
        let uuid = Self::uuid_of(id).map_err(|e| e.tool(false))?;
        // Escape for JS single-quoted context (uuids are internalIds; quotes illegal but be safe).
        let uuid = uuid.replace('\\', "\\\\").replace('\'', "\\'");
        let op = match action {
            "focus" => "ws.activeWindow = list[i];".to_string(),
            "minimize" => "list[i].minimized = true;".to_string(),
            "maximize" => "list[i].setMaximize(true, true);".to_string(),
            "restore" => "list[i].minimized = false; list[i].setMaximize(false, false);".to_string(),
            "close" => "list[i].closeWindow();".to_string(),
            "move_resize" => {
                let g = geo.unwrap();
                format!(
                    "list[i].frameGeometry = Qt.rect({}, {}, {}, {});",
                    g.x, g.y, g.w, g.h
                )
            }
            other => {
                return Err(BackendError::Unsupported {
                    reason: format!("unknown window action: {other}"),
                }
                .tool(false));
            }
        };
        let body = format!(
            "var ws = workspace; var list = ws.windowList(); var found = false; \
             for (var i=0;i<list.length;i++) {{ \
               if (list[i].internalId && list[i].internalId.toString() === '{uuid}') \
                 {{ {op} found = true; break; }} }} \
             if (!found) {{ throw 'window not found'; }} return {{ok: true}};"
        );
        self.run(&body).await.map_err(|e| e.tool(true))?;
        Ok(())
    }
}

fn render_script(caller_bus_name: &str, sink_obj_path: &str, user_body: &str) -> String {
    format!(
        r#"
(function() {{
    function _send(method, payload) {{
        callDBus({caller_bus_name:?}, {sink_obj_path:?},
                 "org.agentdesktop.ScriptSink1", method, payload);
    }}
    try {{
        var _r = (function() {{
            {user_body}
        }})();
        _send("result", typeof _r === "string" ? _r : JSON.stringify(_r));
    }} catch (e) {{
        _send("error", String(e));
    }}
}})();
"#
    )
}

pub async fn run_script(
    bus: &Connection,
    body: &str,
    timeout: Duration,
) -> Result<String, BackendError> {
    let invocation_id = uuid::Uuid::new_v4().simple().to_string();
    let obj_path = format!("/org/agentdesktop/Sink/{invocation_id}");
    let plugin_name = format!("agent-desktop-{invocation_id}");
    let caller_name = bus
        .unique_name()
        .ok_or_else(|| BackendError::BusDisconnected {
            detail: "no unique name on session bus".into(),
        })?
        .to_string();
    let (tx, mut rx) = mpsc::channel::<Result<String, String>>(2);
    let sink = ScriptSink { tx };
    bus.object_server()
        .at(obj_path.clone(), sink)
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?;

    let js = render_script(&caller_name, &obj_path, body);
    let mut tmp = tempfile::Builder::new()
        .prefix("agent-desktop-script-")
        .suffix(".js")
        .tempfile()
        .map_err(|e| BackendError::Io {
            path: "/tmp/agent-desktop-script-*.js".into(),
            error: e.to_string(),
        })?;
    use std::io::Write;
    tmp.write_all(js.as_bytes()).map_err(|e| BackendError::Io {
        path: tmp.path().to_string_lossy().to_string(),
        error: e.to_string(),
    })?;
    tmp.flush().map_err(|e| BackendError::Io {
        path: tmp.path().to_string_lossy().to_string(),
        error: e.to_string(),
    })?;

    let scripting_proxy = ScriptingProxy::new(bus)
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?;
    let script_id = scripting_proxy
        .load_script(&tmp.path().to_string_lossy(), &plugin_name)
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?;
    let script_proxy = ScriptProxy::builder(bus)
        .destination("org.kde.KWin")
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?
        .path(format!("/Scripting/Script{script_id}"))
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?
        .build()
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?;
    script_proxy.run().await.map_err(|e| BackendError::BusDisconnected {
        detail: e.to_string(),
    })?;

    let outcome = match tokio::time::timeout(timeout, rx.recv()).await {
        Ok(Some(Ok(payload))) => Ok(payload),
        Ok(Some(Err(msg))) => Err(BackendError::ExternalCommandFailed { stderr: msg }),
        Ok(None) => Err(BackendError::ExternalCommandFailed {
            stderr: "script channel closed without delivering a result".into(),
        }),
        Err(_) => Err(BackendError::Timeout {
            detail: "kwin script timed out".into(),
        }),
    };
    let _ = scripting_proxy.unload_script(&plugin_name).await;
    let _ = bus
        .object_server()
        .remove::<ScriptSink, _>(obj_path.as_str())
        .await;
    drop(tmp);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_geometry_rounds_instead_of_dropping() {
        let items = vec![
            serde_json::json!({"uuid": "{a}", "caption": "KCalc", "resourceClass": "org.kde.kcalc",
                "geometry": [580, 280, 640, 520.625], "active": true}),
            serde_json::json!({"uuid": "{b}", "caption": "", "resourceClass": "plasmashell",
                "geometry": [0, 0, 1800, 1125], "active": false}),
        ];
        let wins = parse_entries(&items);
        assert_eq!(wins.len(), 2);
        assert_eq!(wins[0].geometry.h, 521);
        assert!(wins[0].is_active);
    }
}
