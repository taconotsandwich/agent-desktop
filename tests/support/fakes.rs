#![allow(dead_code)]
use agent_desktop::platform::drivers::{
    Bbox, Button, InputDriver, Probe, Ref, Shot, ShotDriver, ShotFormat, ShotTarget, ToolError,
    WindowDriver, WindowInfo,
};

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
    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError> {
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
