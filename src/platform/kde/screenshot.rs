//! KWin `org.kde.KWin.ScreenShot2` capture → [`ShotDriver`].
//!
//! Raw ARGB32 over a pipe (read concurrently — multi-MB frames overflow the
//! 64KB pipe buffer), QImage decode, downscale to `max_long_edge`, PNG encode.
//! (Ported from kde-mcp `kwin/screenshot.rs`.)

use crate::error::BackendError;
use crate::platform::drivers::{Probe, Shot, ShotDriver, ShotFormat, ShotTarget};
use crate::types::ToolError;
use image::GenericImageView;
use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};
use zbus::Connection;
use zvariant::Value;

#[zbus::proxy(
    interface = "org.kde.KWin.ScreenShot2",
    default_service = "org.kde.KWin",
    default_path = "/org/kde/KWin/ScreenShot2"
)]
trait ScreenShot2 {
    #[zbus(name = "CaptureWindow")]
    fn capture_window(
        &self,
        handle: &str,
        options: HashMap<&str, zvariant::Value<'_>>,
        pipe: zvariant::OwnedFd,
    ) -> zbus::Result<HashMap<String, zvariant::OwnedValue>>;
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
        let mut probe = super::kwin_probe(&self.bus, self.id()).await;
        if probe.ok {
            probe.detail =
                "KWin service present; screenshot authorization is checked on capture".into();
        }
        probe
    }

    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError> {
        // KWin resolves restricted D-Bus interfaces through a cached KService
        // database. A capture issued immediately after a private session
        // registers its desktop entry can be denied while that view catches
        // up, so absorb a transient denial instead of failing the first shot.
        let mut attempt = 0;
        let mut retried_transfer = false;
        loop {
            attempt += 1;
            match capture_workspace(&self.bus, target.clone(), max_long_edge).await {
                Err(BackendError::ExternalCommandFailed { stderr })
                    if stderr.starts_with("screenshot bytes too short") && !retried_transfer =>
                {
                    retried_transfer = true;
                    tracing::warn!(%stderr,"Retrying incomplete KWin screenshot transfer");
                }
                Err(error @ BackendError::PermissionDenied { .. }) if attempt < 6 => {
                    tracing::debug!(%error, attempt, "KWin authorization not ready; retrying");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                result => return result.map_err(|error| error.tool(true)),
            }
        }
    }
}

async fn capture_workspace(
    bus: &Connection,
    target: ShotTarget,
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
    options.insert("include-cursor", true.into());
    options.insert("include-decoration", true.into());
    options.insert("include-shadow", false.into());
    options.insert("native-resolution", false.into());

    let request = async {
        match target {
            ShotTarget::Full => proxy.capture_workspace(options, write_owned).await,
            ShotTarget::Area(g) => {
                proxy
                    .capture_area(g.x, g.y, g.w, g.h, options, write_owned)
                    .await
            }
            ShotTarget::Window { id, .. } => {
                proxy
                    .capture_window(
                        id.0.strip_prefix("kwin:").unwrap_or(&id.0),
                        options,
                        write_owned,
                    )
                    .await
            }
        }
        .map_err(|error| match &error {
            zbus::Error::MethodError(name, _, _)
                if name.as_str() == "org.kde.KWin.ScreenShot2.Error.NoAuthorized" =>
            {
                BackendError::PermissionDenied {
                    reason: "KWin requires an authorized .desktop entry for this server executable in the compositor's application directory. Install the host application entry or restart the server joined to its private seat.".into(),
                }
            }
            _ => BackendError::BusDisconnected { detail: error.to_string() },
        })
    };
    let (results, bytes) = tokio::try_join!(request, read_pipe(read_owned))?;
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
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
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

async fn read_pipe(fd: OwnedFd) -> Result<Vec<u8>, BackendError> {
    let io = |error: std::io::Error| BackendError::Io {
        path: "screenshot pipe".into(),
        error: error.to_string(),
    };
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        return Err(io(std::io::Error::last_os_error()));
    }
    let fd = tokio::io::unix::AsyncFd::new(std::fs::File::from(fd)).map_err(io)?;
    let read = async {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            let mut ready = fd.readable().await.map_err(io)?;
            match ready.try_io(|fd| fd.get_ref().read(&mut chunk)) {
                Ok(Ok(0)) => return Ok(bytes),
                Ok(Ok(count)) => {
                    if bytes.len() + count > 128 * 1024 * 1024 {
                        return Err(BackendError::Failed("Screenshot exceeds 128 MiB".into()));
                    }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                Ok(Err(error)) => return Err(io(error)),
                Err(_) => {}
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), read)
        .await
        .map_err(|_| BackendError::Timeout {
            detail: "screenshot transfer".into(),
        })?
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
    if stride < row_bytes || bytes.len() < stride.saturating_mul(h) {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!(
                "screenshot bytes too short: got {}, need {}, width {width}, height {height}, stride {stride}",
                bytes.len(),
                stride.saturating_mul(h)
            ),
        });
    }
    let mut out: Vec<u8> = Vec::with_capacity(row_bytes * h);
    match qfmt {
        4..=6 => {
            for row in 0..h {
                let line = &bytes[row * stride..row * stride + row_bytes];
                for px in line.as_chunks::<4>().0 {
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
