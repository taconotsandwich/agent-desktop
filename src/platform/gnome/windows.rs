use crate::{
    error::fail,
    platform::drivers::{Bbox, Probe, Ref, WindowDriver, WindowInfo},
    types::ToolError,
};
use zbus::{Connection, proxy::Proxy};

pub struct GnomeWindows {
    pub(super) bus: Connection,
}
impl GnomeWindows {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }
    pub(super) async fn proxy(&self) -> Result<Proxy<'_>, ToolError> {
        Proxy::new(
            &self.bus,
            "org.agentdesktop.Windows",
            "/org/agentdesktop/Windows",
            "org.agentdesktop.Windows",
        )
        .await
        .map_err(|error| fail("backend_unavailable", error.to_string()))
    }
    async fn control(&self, id: &Ref, action: &str, geometry: Vec<i32>) -> Result<(), ToolError> {
        self.proxy()
            .await?
            .call::<_, _, ()>("Control", &(&id.0, action, geometry))
            .await
            .map_err(|error| fail("window_control_failed", error.to_string()))
    }
}
#[async_trait::async_trait]
impl WindowDriver for GnomeWindows {
    fn id(&self) -> &'static str {
        "gnome-shell-extension"
    }
    async fn probe(&self) -> Probe {
        let result = self.query().await;
        Probe {
            id: self.id(),
            ok: result.is_ok(),
            detail: result
                .map(|_| "GNOME companion extension reachable".into())
                .unwrap_or_else(|error| format!("Enable agent-desktop@local: {}", error.message)),
        }
    }
    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let payload: String = self
            .proxy()
            .await?
            .call("Query", &())
            .await
            .map_err(|error| fail("backend_unavailable", error.to_string()))?;
        serde_json::from_str(&payload)
            .map_err(|error| fail("invalid_window_state", error.to_string()))
    }
    async fn focus(&self, id: &Ref) -> Result<(), ToolError> {
        self.control(id, "focus", vec![]).await
    }
    async fn minimize(&self, id: &Ref) -> Result<(), ToolError> {
        self.control(id, "minimize", vec![]).await
    }
    async fn maximize(&self, id: &Ref) -> Result<(), ToolError> {
        self.control(id, "maximize", vec![]).await
    }
    async fn restore(&self, id: &Ref) -> Result<(), ToolError> {
        self.control(id, "restore", vec![]).await
    }
    async fn close(&self, id: &Ref) -> Result<(), ToolError> {
        self.control(id, "close", vec![]).await
    }
    async fn move_resize(&self, id: &Ref, geo: Bbox) -> Result<(), ToolError> {
        self.control(
            id,
            "move_resize",
            vec![geo.x, geo.y, geo.w as i32, geo.h as i32],
        )
        .await
    }
}
