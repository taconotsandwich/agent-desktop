//! X11 capture via `GetImage` — the same X11 request maim/import issue, with
//! no external binary. Raw BGRX pixels are converted to RGBA in one pass,
//! then downscaled and PNG-encoded like every other shot driver.

use super::{X11, connection, failed};
use crate::{
    error::BackendError,
    platform::drivers::{Bbox, Probe, Shot, ShotDriver, ShotFormat, ShotTarget},
    types::ToolError,
};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{ConnectionExt as _, Drawable, ImageFormat, ImageOrder},
};

pub struct X11Shot;

fn capture_image(
    x11: &X11,
    drawable: Drawable,
    area: Bbox,
) -> Result<image::DynamicImage, BackendError> {
    if x11.connection.setup().image_byte_order != ImageOrder::LSB_FIRST {
        return Err(BackendError::Unsupported {
            reason: "big-endian X11 servers are not supported".into(),
        });
    }
    let depth = x11
        .connection
        .get_geometry(drawable)
        .map_err(failed)?
        .reply()
        .map_err(failed)?
        .depth;
    let bits_per_pixel = x11
        .connection
        .setup()
        .pixmap_formats
        .iter()
        .find(|format| format.depth == depth)
        .map(|format| format.bits_per_pixel)
        .ok_or_else(|| BackendError::Unsupported {
            reason: format!("X11 root has no pixmap format for depth {depth}"),
        })?;
    if bits_per_pixel != 32 {
        return Err(BackendError::Unsupported {
            reason: format!("unsupported X11 bits-per-pixel {bits_per_pixel}"),
        });
    }
    let reply = x11
        .connection
        .get_image(
            ImageFormat::Z_PIXMAP,
            drawable,
            area.x as i16,
            area.y as i16,
            area.w as u16,
            area.h as u16,
            u32::MAX,
        )
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    let width = area.w;
    let height = area.h;
    let pixels = width as usize * height as usize;
    if reply.data.len() < pixels * 4 {
        return Err(BackendError::Failed(format!(
            "truncated X11 image: got {}, need {}",
            reply.data.len(),
            pixels * 4
        )));
    }
    let mut rgba = Vec::with_capacity(pixels * 4);
    for chunk in reply.data.as_chunks::<4>().0.iter().take(pixels) {
        let alpha = if depth == 32 { chunk[3] } else { 255 };
        rgba.extend_from_slice(&[chunk[2], chunk[1], chunk[0], alpha]);
    }
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| BackendError::Failed("image buffer construction failed".into()))?;
    Ok(image::DynamicImage::ImageRgba8(buffer))
}

#[async_trait::async_trait]
impl ShotDriver for X11Shot {
    fn id(&self) -> &'static str {
        "x11-getimage"
    }

    async fn probe(&self) -> Probe {
        let result = async {
            let x11 = connection()?;
            x11.connection
                .get_geometry(x11.root)
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            Ok::<(), BackendError>(())
        }
        .await;
        Probe {
            id: self.id(),
            ok: result.is_ok(),
            detail: result
                .map(|()| "X11 root window reachable".into())
                .unwrap_or_else(|error| error.to_string()),
        }
    }

    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let (drawable, area) = match &target {
            ShotTarget::Full => (
                x11.root,
                Bbox {
                    x: 0,
                    y: 0,
                    w: x11.root_width as u32,
                    h: x11.root_height as u32,
                },
            ),
            ShotTarget::Area(g) => {
                if g.x < 0
                    || g.y < 0
                    || g.w == 0
                    || g.h == 0
                    || g.x as u64 + g.w as u64 > x11.root_width as u64
                    || g.y as u64 + g.h as u64 > x11.root_height as u64
                {
                    return Err(crate::error::fail(
                        "invalid_geometry",
                        "Crop is outside the X11 root",
                    ));
                }
                (x11.root, g.clone())
            }
            ShotTarget::Window { id, .. } => {
                let window = super::windows::window_of(id)?;
                let geometry = x11
                    .connection
                    .get_geometry(window)
                    .map_err(|error| failed(error).tool(true))?
                    .reply()
                    .map_err(|error| failed(error).tool(true))?;
                (
                    window,
                    Bbox {
                        x: 0,
                        y: 0,
                        w: geometry.width as u32,
                        h: geometry.height as u32,
                    },
                )
            }
        };
        let image = capture_image(&x11, drawable, area).map_err(|error| error.tool(true))?;

        let edge = max_long_edge.clamp(256, 1568);
        let (coord_w, coord_h) = (image.width(), image.height());
        let scale = (edge as f32 / coord_w.max(coord_h) as f32).min(1.0);
        let image = if scale < 1.0 {
            image.resize_exact(
                (coord_w as f32 * scale) as u32,
                (coord_h as f32 * scale) as u32,
                image::imageops::FilterType::Triangle,
            )
        } else {
            image
        };
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|error| {
                BackendError::ExternalCommandFailed {
                    stderr: format!("png encode: {error}"),
                }
                .tool(true)
            })?;
        Ok(Shot {
            bytes: png,
            format: ShotFormat::Png,
            coord_w,
            coord_h,
        })
    }
}
