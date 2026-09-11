use super::windows::GnomeWindows;
use crate::{
    error::fail,
    platform::drivers::{Probe, Shot, ShotDriver, ShotFormat, ShotTarget, WindowDriver},
    types::ToolError,
};
use base64::Engine as _;
pub struct GnomeShot {
    windows: GnomeWindows,
}
impl GnomeShot {
    pub fn new(bus: zbus::Connection) -> Self {
        Self {
            windows: GnomeWindows::new(bus),
        }
    }
}
#[async_trait::async_trait]
impl ShotDriver for GnomeShot {
    fn id(&self) -> &'static str {
        "gnome-shell-screenshot"
    }
    async fn probe(&self) -> Probe {
        let probe = self.windows.probe().await;
        Probe {
            id: self.id(),
            ok: probe.ok,
            detail: probe.detail,
        }
    }
    async fn capture(&self, target: ShotTarget, edge: u32) -> Result<Shot, ToolError> {
        let ShotTarget::Window { id, .. } = target else {
            return Err(fail(
                "unsupported",
                "GNOME captures require an application target",
            ));
        };
        let encoded: String = self
            .windows
            .proxy()
            .await?
            .call("Screenshot", &(&id.0,))
            .await
            .map_err(|error| fail("screenshot_failed", error.to_string()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| fail("invalid_screenshot", error.to_string()))?;
        let image = image::load_from_memory(&bytes)
            .map_err(|error| fail("invalid_screenshot", error.to_string()))?;
        let (coord_w, coord_h) = (image.width(), image.height());
        let edge = edge.clamp(256, 1568);
        let image = if coord_w.max(coord_h) > edge {
            image.resize(edge, edge, image::imageops::FilterType::Triangle)
        } else {
            image
        };
        let mut bytes = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .map_err(|error| fail("invalid_screenshot", error.to_string()))?;
        Ok(Shot {
            bytes,
            format: ShotFormat::Png,
            coord_w,
            coord_h,
        })
    }
}
