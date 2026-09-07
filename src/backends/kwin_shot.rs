//! KWin `org.kde.KWin.ScreenShot2` capture → [`ShotDriver`].
//!
//! Raw ARGB32 over a pipe (read concurrently — multi-MB frames overflow the
//! 64KB pipe buffer), QImage decode, downscale to `max_long_edge`, PNG encode.
//! (Ported from kde-mcp `kwin/screenshot.rs`.)

use crate::drivers::{Probe, Shot, ShotDriver, ShotFormat, ShotTarget};
use crate::error::BackendError;
use crate::types::ToolError;
use image::GenericImageView;
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::OwnedFd;
use zbus::Connection;
use zvariant::Value;

#[zbus::proxy(
    interface = "org.kde.KWin.ScreenShot2",
    default_service = "org.kde.KWin",
    default_path = "/org/kde/KWin/ScreenShot2"
)]
trait ScreenShot2 {
    #[zbus(name = "CaptureWorkspace")]
    fn capture_workspace(
        &self,
        options: HashMap<&str, zvariant::Value<'_>>,
        pipe: zvariant::OwnedFd,
    ) -> zbus::Result<HashMap<String, zvariant::OwnedValue>>;

    #[zbus(name = "CaptureArea")]
    fn capture_area(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        options: HashMap<&str, zvariant::Value<'_>>,
        pipe: zvariant::OwnedFd,
    ) -> zbus::Result<HashMap<String, zvariant::OwnedValue>>;
}

pub struct KwinShot {
    bus: Connection,
}

impl KwinShot {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }
}

#[async_trait::async_trait]
impl ShotDriver for KwinShot {
    fn id(&self) -> &'static str {
        "kwin-screenshot2"
    }

    async fn probe(&self) -> Probe {
        super::kwin_probe(&self.bus, self.id()).await
    }

    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError> {
        let area = match target {
            ShotTarget::Full => None,
            _ => {
                return Err(BackendError::Unsupported {
                    reason: "window/element-targeted capture lands with the window driver (frameGeometry crop)".into(),
                }
                .tool(false));
            }
        };
        capture_workspace(&self.bus, area, max_long_edge)
            .await
            .map_err(|e| e.tool(true))
    }
}

async fn capture_workspace(
    bus: &Connection,
    area: Option<(i32, i32, u32, u32)>,
    max_long_edge: u32,
) -> Result<Shot, BackendError> {
    let (read_fd, write_fd) = nix::unistd::pipe().map_err(|e| BackendError::Io {
        path: "pipe()".into(),
        error: e.to_string(),
    })?;
    let read_owned: OwnedFd = read_fd;
    let write_owned = zvariant::OwnedFd::from(write_fd);
    let proxy = ScreenShot2Proxy::new(bus)
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: e.to_string(),
        })?;

    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("include-cursor", false.into());
    options.insert("native-resolution", false.into());

    let reader = tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
        let mut f = std::fs::File::from(read_owned);
        let mut buf = Vec::with_capacity(8 * 1024 * 1024);
        f.read_to_end(&mut buf)?;
        Ok(buf)
    });

    let results = match area {
        None => proxy.capture_workspace(options, write_owned).await,
        Some((x, y, w, h)) => proxy.capture_area(x, y, w, h, options, write_owned).await,
    }
    .map_err(|e| BackendError::BusDisconnected {
        detail: e.to_string(),
    })?;

    let bytes = reader
        .await
        .map_err(|e| BackendError::Io {
            path: "screenshot pipe join".into(),
            error: e.to_string(),
        })?
        .map_err(|e| BackendError::Io {
            path: "screenshot pipe".into(),
            error: e.to_string(),
        })?;
    if bytes.is_empty() {
        return Err(BackendError::ExternalCommandFailed {
            stderr: "screenshot pipe returned 0 bytes".into(),
        });
    }

    let width = read_u32(&results, "width")?;
    let height = read_u32(&results, "height")?;
    let stride = read_u32(&results, "stride")?;
    let qfmt = read_u32(&results, "format")?;
    let img = decode_qimage(&bytes, width, height, stride, qfmt)?;

    // Downscale to the vision sweet spot; record coord size for click mapping.
    let edge = max_long_edge.clamp(256, 1568);
    let (cw, ch) = img.dimensions();
    let scale = (edge as f32 / cw.max(ch) as f32).min(1.0);
    let (dw, dh) = ((cw as f32 * scale) as u32, (ch as f32 * scale) as u32);
    let img = if scale < 1.0 {
        img.resize_exact(dw, dh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut png = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut png),
        image::ImageFormat::Png,
    )
    .map_err(|e| BackendError::ExternalCommandFailed {
        stderr: format!("png encode: {e}"),
    })?;
    Ok(Shot {
        bytes: png,
        format: ShotFormat::Png,
        coord_w: cw,
        coord_h: ch,
    })
}

fn read_u32(map: &HashMap<String, zvariant::OwnedValue>, key: &str) -> Result<u32, BackendError> {
    map.get(key)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| BackendError::ExternalCommandFailed {
            stderr: format!("screenshot result missing/invalid u32 field: {key}"),
        })
}

/// Qt QImage formats seen from KWin: 4 RGB32, 5 ARGB32, 6 premultiplied,
/// 17/18 RGBA8888. Little-endian byte orders handled per format.
fn decode_qimage(
    bytes: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    qfmt: u32,
) -> Result<image::DynamicImage, BackendError> {
    if width == 0 || height == 0 {
        return Err(BackendError::ExternalCommandFailed {
            stderr: "screenshot zero dimensions".into(),
        });
    }
    let row_bytes = width as usize * 4;
    let stride = stride as usize;
    let h = height as usize;
    if bytes.len() < stride.saturating_mul(h) {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("screenshot bytes too short: got {}", bytes.len()),
        });
    }
    let mut out: Vec<u8> = Vec::with_capacity(row_bytes * h);
    match qfmt {
        4..=6 => {
            for row in 0..h {
                let line = &bytes[row * stride..row * stride + row_bytes];
                for px in line.chunks_exact(4) {
                    let a = if qfmt == 4 { 255 } else { px[3] };
                    out.extend_from_slice(&[px[2], px[1], px[0], a]);
                }
            }
        }
        17..=18 => {
            for row in 0..h {
                out.extend_from_slice(&bytes[row * stride..row * stride + row_bytes]);
            }
        }
        other => {
            return Err(BackendError::ExternalCommandFailed {
                stderr: format!("unsupported QImage::Format {other}"),
            });
        }
    }
    let buf = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_raw(width, height, out)
        .ok_or_else(|| BackendError::ExternalCommandFailed {
            stderr: "image buffer construction failed".into(),
        })?;
    Ok(image::DynamicImage::ImageRgba8(buf))
}
