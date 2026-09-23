#![allow(dead_code)]
use agent_desktop::platform::drivers::{
    Bbox, Button, InputDriver, Probe, Ref, Shot, ShotDriver, ShotFormat, ShotTarget,
    TargetedInputDriver, ToolError, WindowDriver, WindowInfo,
};
use std::sync::Mutex;

fn err() -> ToolError {
    ToolError {
        code: "not_implemented".into(),
        message: "fake driver: behaviour lands with the real backend".into(),
        retryable: false,
    }
}

pub struct FakeShot;
pub struct FakeInput;
pub struct FakeWindows;
/// Addresses every window except native Wayland ones and records each
/// dispatch as one line.
#[derive(Default)]
pub struct FakeTargeted {
    pub calls: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ShotDriver for FakeShot {
    fn id(&self) -> &'static str {
        "fake-shot"
    }
    async fn probe(&self) -> Probe {
        Probe {
            id: self.id(),
            ok: true,
            detail: "fake always ok".into(),
        }
    }
    async fn capture(&self, _t: ShotTarget, _e: u32) -> Result<Shot, ToolError> {
        let image = image::DynamicImage::new_rgb8(800, 450);
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .map_err(|error| agent_desktop::error::fail("invalid_screenshot", error.to_string()))?;
        Ok(Shot {
            bytes,
            format: ShotFormat::Png,
            coord_w: 1600,
            coord_h: 900,
        })
    }
}

#[async_trait::async_trait]
impl InputDriver for FakeInput {
    fn id(&self) -> &'static str {
        "fake-input"
    }
    async fn probe(&self) -> Probe {
        Probe {
            id: self.id(),
            ok: true,
            detail: "fake always ok".into(),
        }
    }
    async fn click(
        &self,
        _x: i32,
        _y: i32,
        _b: Button,
        _hold: Vec<agent_desktop::platform::keymap::Modifier>,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    async fn move_to(&self, _x: i32, _y: i32) -> Result<(), ToolError> {
        Ok(())
    }
    async fn drag(
        &self,
        _p: Vec<(i32, i32)>,
        _b: Button,
        _dwell_ms: u64,
        _step_ms: u64,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    async fn scroll(
        &self,
        _x: i32,
        _y: i32,
        _dx: i32,
        _dy: i32,
        _hold: Vec<agent_desktop::platform::keymap::Modifier>,
    ) -> Result<(), ToolError> {
        Ok(())
    }
    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        if text.is_empty() {
            return Err(err());
        }
        Ok(())
    }
    async fn key(
        &self,
        keys: Vec<agent_desktop::platform::keymap::Chord>,
    ) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(err());
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl WindowDriver for FakeWindows {
    fn id(&self) -> &'static str {
        "fake-windows"
    }
    async fn probe(&self) -> Probe {
        Probe {
            id: self.id(),
            ok: true,
            detail: "fake always ok".into(),
        }
    }
    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        Ok(vec![WindowInfo {
            window_ref: Ref("fake:1".into()),
            title: "fake window".into(),
            class: "fake".into(),
            app_id: "fake.desktop".into(),
            pid: None,
            minimized: false,
            client_protocol: None,
            geometry: Bbox {
                x: 0,
                y: 0,
                w: 800,
                h: 600,
            },
            screen: 0,
            is_active: true,
        }])
    }
    async fn focus(&self, _id: &Ref) -> Result<(), ToolError> {
        Ok(())
    }
    async fn minimize(&self, _id: &Ref) -> Result<(), ToolError> {
        Ok(())
    }
    async fn maximize(&self, _id: &Ref) -> Result<(), ToolError> {
        Ok(())
    }
    async fn restore(&self, _id: &Ref) -> Result<(), ToolError> {
        Ok(())
    }
    async fn close(&self, _id: &Ref) -> Result<(), ToolError> {
        Ok(())
    }
    async fn move_resize(&self, _id: &Ref, _geo: Bbox) -> Result<(), ToolError> {
        Ok(())
    }
}

impl FakeTargeted {
    fn record(&self, call: String) -> Result<(), ToolError> {
        self.calls.lock().unwrap().push(call);
        Ok(())
    }
}

#[async_trait::async_trait]
impl TargetedInputDriver for FakeTargeted {
    fn id(&self) -> &'static str {
        "fake-targeted"
    }
    async fn probe(&self) -> Probe {
        Probe {
            id: self.id(),
            ok: true,
            detail: "fake always ok".into(),
        }
    }
    async fn resolve(&self, window: &WindowInfo) -> Result<Option<Ref>, ToolError> {
        Ok((window.client_protocol.as_deref() != Some("wayland"))
            .then(|| window.window_ref.clone()))
    }
    async fn click(
        &self,
        target: &Ref,
        x: i32,
        y: i32,
        button: Button,
        count: u32,
    ) -> Result<(), ToolError> {
        self.record(format!("click {} ({x},{y}) {button:?} x{count}", target.0))
    }
    async fn drag(
        &self,
        target: &Ref,
        path: Vec<(i32, i32)>,
        button: Button,
        _dwell_ms: u64,
        _step_ms: u64,
    ) -> Result<(), ToolError> {
        self.record(format!(
            "drag {} {:?}..{:?} {button:?}",
            target.0,
            path.first(),
            path.last()
        ))
    }
    async fn scroll(
        &self,
        target: &Ref,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
    ) -> Result<(), ToolError> {
        self.record(format!("scroll {} ({x},{y}) by ({dx},{dy})", target.0))
    }
    async fn type_text(&self, target: &Ref, text: String) -> Result<(), ToolError> {
        self.record(format!("type {} {text:?}", target.0))
    }
    async fn key(
        &self,
        target: &Ref,
        keys: Vec<agent_desktop::platform::keymap::Chord>,
        _hold: std::time::Duration,
    ) -> Result<(), ToolError> {
        self.record(format!("key {} {}", target.0, keys.len()))
    }
}
