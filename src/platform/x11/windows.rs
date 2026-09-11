use super::{display, has, run};
use crate::{
    error::fail,
    platform::drivers::{Bbox, Probe, Ref, WindowDriver, WindowInfo},
    types::ToolError,
};

pub struct X11Windows;

impl X11Windows {
    async fn control(
        &self,
        command: &'static str,
        id: &Ref,
        args: &[&str],
    ) -> Result<(), ToolError> {
        let wid =
            id.0.strip_prefix("x11:")
                .ok_or_else(|| fail("stale_window", "Invalid X11 window identity"))?;
        let d = display().map_err(|error| error.tool(false))?;
        let mut argv = vec!["-i", "-r", wid];
        argv.extend_from_slice(args);
        run(command, "wmctrl", &argv, &d)
            .await
            .map(|_| ())
            .map_err(|error| error.tool(false))
    }
}

#[async_trait::async_trait]
impl WindowDriver for X11Windows {
    fn id(&self) -> &'static str {
        "x11-ewmh"
    }
    async fn probe(&self) -> Probe {
        let result = async {
            let d = display()?;
            run("window manager", "wmctrl", &["-m"], &d).await
        }
        .await;
        Probe {
            id: self.id(),
            ok: result.is_ok() && has("xdotool") && has("xprop"),
            detail: result
                .map(|_| "EWMH window manager reachable".into())
                .unwrap_or_else(|error| error.to_string()),
        }
    }
    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let d = display().map_err(|error| error.tool(false))?;
        let raw = run("list", "wmctrl", &["-l", "-p", "-G", "-u"], &d)
            .await
            .map_err(|error| error.tool(true))?;
        let active = run("active window", "xdotool", &["getactivewindow"], &d)
            .await
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .and_then(|text| text.trim().parse::<u64>().ok());
        let mut windows = Vec::new();
        for line in String::from_utf8_lossy(&raw).lines() {
            let parts: Vec<_> = line.split_whitespace().collect();
            if parts.len() < 8 {
                continue;
            }
            let id = parts[0];
            let numeric = u64::from_str_radix(id.trim_start_matches("0x"), 16).ok();
            let attributes = run(
                "window attributes",
                "xprop",
                &[
                    "-id",
                    id,
                    "_NET_WM_STATE",
                    "_NET_WM_WINDOW_TYPE",
                    "_GTK_APPLICATION_ID",
                    "WM_CLASS",
                ],
                &d,
            )
            .await
            .map_err(|error| error.tool(true))?;
            let attrs = String::from_utf8_lossy(&attributes);
            if let Some(kind) = attrs
                .lines()
                .find(|line| line.starts_with("_NET_WM_WINDOW_TYPE(ATOM)"))
                && !kind.contains("_NET_WM_WINDOW_TYPE_NORMAL")
                && !kind.contains("_NET_WM_WINDOW_TYPE_DIALOG")
            {
                continue;
            }
            let app_id = attrs
                .lines()
                .find(|line| line.starts_with("_GTK_APPLICATION_ID"))
                .and_then(|line| line.split_once(" = "))
                .map(|(_, value)| value.trim_matches('"').to_string())
                .unwrap_or_default();
            let quoted = |property: &str| {
                attrs
                    .lines()
                    .find(|line| line.starts_with(property))
                    .and_then(|line| line.split_once(" = "))
                    .and_then(|(_, value)| value.rsplit(", ").next())
                    .map(|value| value.trim_matches('"').to_owned())
            };
            windows.push(WindowInfo {
                window_ref: Ref(format!("x11:{id}")),
                title: parts[8..].join(" "),
                class: quoted("WM_CLASS(").unwrap_or_default(),
                app_id,
                pid: parts[2].parse::<u32>().ok().filter(|pid| *pid > 0),
                minimized: attrs.contains("_NET_WM_STATE_HIDDEN"),
                client_protocol: Some("x11".into()),
                geometry: Bbox {
                    x: parts[3].parse().unwrap_or(0),
                    y: parts[4].parse().unwrap_or(0),
                    w: parts[5].parse().unwrap_or(0),
                    h: parts[6].parse().unwrap_or(0),
                },
                screen: 0,
                is_active: active.is_some() && numeric == active,
            });
        }
        Ok(windows)
    }
    async fn focus(&self, id: &Ref) -> Result<(), ToolError> {
        let d = display().map_err(|error| error.tool(false))?;
        let wid =
            id.0.strip_prefix("x11:")
                .ok_or_else(|| fail("stale_window", "Invalid X11 window identity"))?;
        run("focus", "wmctrl", &["-i", "-a", wid], &d)
            .await
            .map(|_| ())
            .map_err(|error| error.tool(false))
    }
    async fn minimize(&self, id: &Ref) -> Result<(), ToolError> {
        let d = display().map_err(|error| error.tool(false))?;
        let wid =
            id.0.strip_prefix("x11:")
                .ok_or_else(|| fail("stale_window", "Invalid X11 window identity"))?;
        run("minimize", "xdotool", &["windowminimize", wid], &d)
            .await
            .map(|_| ())
            .map_err(|error| error.tool(false))
    }
    async fn maximize(&self, id: &Ref) -> Result<(), ToolError> {
        self.control("maximize", id, &["-b", "add,maximized_vert,maximized_horz"])
            .await
    }
    async fn restore(&self, id: &Ref) -> Result<(), ToolError> {
        self.control(
            "restore",
            id,
            &["-b", "remove,maximized_vert,maximized_horz"],
        )
        .await?;
        self.focus(id).await
    }
    async fn close(&self, id: &Ref) -> Result<(), ToolError> {
        let d = display().map_err(|error| error.tool(false))?;
        let wid =
            id.0.strip_prefix("x11:")
                .ok_or_else(|| fail("stale_window", "Invalid X11 window identity"))?;
        run("close", "wmctrl", &["-i", "-c", wid], &d)
            .await
            .map(|_| ())
            .map_err(|error| error.tool(false))
    }
    async fn move_resize(&self, id: &Ref, geo: Bbox) -> Result<(), ToolError> {
        if geo.w == 0 || geo.h == 0 {
            return Err(fail("invalid_geometry", "Window size must be positive"));
        }
        self.control(
            "move resize",
            id,
            &["-e", &format!("0,{},{},{},{}", geo.x, geo.y, geo.w, geo.h)],
        )
        .await
    }
}
