use super::{display, has, run};
use crate::{
    error::BackendError,
    platform::drivers::{Probe, Shot, ShotDriver, ShotFormat, ShotTarget},
    types::ToolError,
};

pub struct X11Shot;

#[async_trait::async_trait]
impl ShotDriver for X11Shot {
    fn id(&self) -> &'static str {
        "x11-maim"
    }
    async fn probe(&self) -> Probe {
        match display() {
            Err(e) => Probe {
                id: self.id(),
                ok: false,
                detail: e.tool(false).message,
            },
            Ok(_) if has("maim") => Probe {
                id: self.id(),
                ok: true,
                detail: "maim on PATH".into(),
            },
            Ok(_) if has("import") => Probe {
                id: "x11-import",
                ok: true,
                detail: "import (ImageMagick) on PATH".into(),
            },
            Ok(_) => Probe {
                id: self.id(),
                ok: false,
                detail: "neither maim nor import found".into(),
            },
        }
    }
    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError> {
        let window = match &target {
            ShotTarget::Window { id, .. } => Some(id.0.strip_prefix("x11:").unwrap_or(&id.0)),
            _ => None,
        };
        let d = display().map_err(|e| e.tool(false))?;
        let raw = if has("maim") {
            let mut args = vec!["--format=png"];
            if let Some(id) = window {
                args.extend(["--window", id]);
            }
            args.push("/dev/stdout");
            run("screenshot", "maim", &args, &d)
                .await
                .map_err(|e| e.tool(true))?
        } else if has("import") {
            run(
                "screenshot",
                "import",
                &["-window", window.unwrap_or("root"), "png:-"],
                &d,
            )
            .await
            .map_err(|e| e.tool(true))?
        } else {
            return Err(BackendError::Unavailable {
                backend: "x11-maim",
                detail: "neither maim nor import found".into(),
            }
            .tool(false));
        };
        let img = image::load_from_memory(&raw).map_err(|e| {
            BackendError::ExternalCommandFailed {
                stderr: format!("decode x11 screenshot: {e}"),
            }
            .tool(true)
        })?;
        let img = if let ShotTarget::Area(g) = target {
            if g.x < 0
                || g.y < 0
                || g.w == 0
                || g.h == 0
                || g.x as u64 + g.w as u64 > img.width() as u64
                || g.y as u64 + g.h as u64 > img.height() as u64
            {
                return Err(crate::error::fail(
                    "invalid_geometry",
                    "Crop is outside the X11 root",
                ));
            }
            img.crop_imm(g.x as u32, g.y as u32, g.w, g.h)
        } else {
            img
        };
        let edge = max_long_edge.clamp(256, 1568);
        let (cw, ch) = (img.width(), img.height());
        let scale = (edge as f32 / cw.max(ch) as f32).min(1.0);
        let img = if scale < 1.0 {
            img.resize_exact(
                (cw as f32 * scale) as u32,
                (ch as f32 * scale) as u32,
                image::imageops::FilterType::Triangle,
            )
        } else {
            img
        };
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| {
                BackendError::ExternalCommandFailed {
                    stderr: format!("png encode: {e}"),
                }
                .tool(true)
            })?;
        Ok(Shot {
            bytes: png,
            format: ShotFormat::Png,
            coord_w: cw,
            coord_h: ch,
        })
    }
}
