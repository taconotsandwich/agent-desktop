//! xdg-desktop-portal drivers: Screenshot + RemoteDesktop→EIS.
//!
//! Used on GNOME Wayland (and any portal-backed compositor). Unlike KWin's
//! private interfaces these CAN prompt: first Start() shows the portal's
//! device-share dialog; the session persists afterwards. Probe reports
//! presence, not authorization; the public state includes probe details.

mod input;
pub use input::PortalConnector;

use crate::error::BackendError;
use crate::platform::drivers::{Probe, Shot, ShotDriver, ShotFormat, ShotTarget};
use crate::types::ToolError;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use zbus::Connection;
use zbus::proxy::Proxy;
use zvariant::{OwnedObjectPath, OwnedValue, Value};

static TOKEN_SEQ: AtomicU64 = AtomicU64::new(1);

fn new_token() -> String {
    let n = TOKEN_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("ad{n}_{}", std::process::id())
}

fn sender_token(conn: &Connection) -> Result<String, BackendError> {
    conn.unique_name()
        .ok_or_else(|| BackendError::BusDisconnected {
            detail: "no unique name on bus".into(),
        })
        .map(|n| n.as_str().trim_start_matches(':').replace('.', "_"))
}

/// Await the Response signal for one portal request handle.
struct PortalRequest {
    bus: Connection,
    handle: OwnedObjectPath,
    stream: zbus::MessageStream,
}

impl Drop for PortalRequest {
    fn drop(&mut self) {
        close_object(
            self.bus.clone(),
            self.handle.clone(),
            "org.freedesktop.portal.Request",
        );
    }
}

impl PortalRequest {
    fn verify_handle(&mut self, actual: OwnedObjectPath) -> Result<(), BackendError> {
        if actual != self.handle {
            self.handle = actual;
            return Err(BackendError::Failed(
                "Portal ignored the requested response handle".into(),
            ));
        }
        Ok(())
    }
}

pub struct PortalSession {
    bus: Connection,
    pub path: OwnedObjectPath,
}

impl Drop for PortalSession {
    fn drop(&mut self) {
        close_object(
            self.bus.clone(),
            self.path.clone(),
            "org.freedesktop.portal.Session",
        );
    }
}

fn close_object(bus: Connection, path: OwnedObjectPath, interface: &'static str) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let close = async {
                if let Ok(proxy) =
                    Proxy::new(&bus, "org.freedesktop.portal.Desktop", path, interface).await
                {
                    let _: Result<(), _> = proxy.call("Close", &()).await;
                }
            };
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), close).await;
        });
    }
}

async fn response_stream(
    conn: &Connection,
    handle: OwnedObjectPath,
) -> Result<PortalRequest, BackendError> {
    use zbus::MatchRule;
    use zbus::message::Type as MsgType;
    let rule = MatchRule::builder()
        .msg_type(MsgType::Signal)
        .interface("org.freedesktop.portal.Request")
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("match rule: {e}"),
        })?
        .member("Response")
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("match rule: {e}"),
        })?
        .path(handle.clone())
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("match rule: {e}"),
        })?
        .build();
    let stream = zbus::MessageStream::for_match_rule(rule, conn, None)
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("match stream: {e}"),
        })?;
    Ok(PortalRequest {
        bus: conn.clone(),
        handle,
        stream,
    })
}

async fn await_response(
    mut request: PortalRequest,
) -> Result<(u32, HashMap<String, OwnedValue>), BackendError> {
    let msg = tokio::time::timeout(std::time::Duration::from_secs(60), request.stream.next())
        .await
        .map_err(|_| BackendError::Timeout {
            detail: "portal request timed out (dialog unanswered?)".into(),
        })?
        .ok_or_else(|| BackendError::BusDisconnected {
            detail: "portal response stream ended".into(),
        })?
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("portal response: {e}"),
        })?;
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        msg.body()
            .deserialize()
            .map_err(|e| BackendError::ExternalCommandFailed {
                stderr: format!("portal response parse: {e}"),
            })?;
    Ok((code, results))
}

async fn portal_proxy<'a>(
    conn: &'a Connection,
    iface: &'static str,
) -> Result<Proxy<'a>, BackendError> {
    Proxy::new(
        conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        iface,
    )
    .await
    .map_err(|e| BackendError::BusDisconnected {
        detail: format!("portal proxy {iface}: {e}"),
    })
}

pub async fn portal_present(conn: &Connection) -> bool {
    let Ok(proxy) = zbus::fdo::DBusProxy::new(conn).await else {
        return false;
    };
    let Ok(names) = proxy.list_names().await else {
        return false;
    };
    names
        .iter()
        .any(|n| n.as_str() == "org.freedesktop.portal.Desktop")
}

// ---- Screenshot ----

pub struct PortalShot {
    bus: Connection,
}

impl PortalShot {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }
}

#[async_trait::async_trait]
impl ShotDriver for PortalShot {
    fn window_capture(&self) -> bool {
        false
    }
    fn id(&self) -> &'static str {
        "portal-screenshot"
    }

    async fn probe(&self) -> Probe {
        let ok = portal_present(&self.bus).await;
        Probe {
            id: self.id(),
            ok,
            detail: if ok {
                "portal Desktop present".into()
            } else {
                "org.freedesktop.portal.Desktop missing".into()
            },
        }
    }

    async fn capture(&self, target: ShotTarget, max_long_edge: u32) -> Result<Shot, ToolError> {
        if !matches!(target, ShotTarget::Full) {
            return Err(BackendError::Unsupported {
                reason: "The screenshot portal only provides full-desktop capture".into(),
            }
            .tool(false));
        }
        capture_full(&self.bus, max_long_edge)
            .await
            .map_err(|e| e.tool(true))
    }
}

async fn capture_full(bus: &Connection, max_long_edge: u32) -> Result<Shot, BackendError> {
    let proxy = portal_proxy(bus, "org.freedesktop.portal.Screenshot").await?;
    let token = new_token();
    let sender = sender_token(bus)?;
    let handle: OwnedObjectPath =
        format!("/org/freedesktop/portal/desktop/request/{sender}/{token}")
            .try_into()
            .map_err(|e| BackendError::ExternalCommandFailed {
                stderr: format!("request path: {e}"),
            })?;
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", token.as_str().into());
    options.insert("modal", true.into());
    options.insert("interactive", false.into());
    let mut stream = response_stream(bus, handle).await?;
    let request: OwnedObjectPath = proxy
        .call("Screenshot", &("", options))
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("Screenshot call: {e}"),
        })?;
    stream.verify_handle(request)?;
    let (code, results) = await_response(stream).await?;
    if code != 0 {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("screenshot denied/cancelled (code {code})"),
        });
    }
    let uri = results
        .get("uri")
        .and_then(|v| <&str>::try_from(v).ok())
        .map(str::to_string)
        .ok_or_else(|| BackendError::ExternalCommandFailed {
            stderr: "screenshot response missing uri".into(),
        })?;
    let path = url::Url::parse(&uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok())
        .ok_or_else(|| {
            BackendError::Failed("Screenshot portal returned a non-local file URI".into())
        })?;
    let bytes = tokio::fs::read(&path).await.map_err(|e| BackendError::Io {
        path: path.display().to_string(),
        error: e.to_string(),
    })?;
    let img = image::load_from_memory(&bytes).map_err(|e| BackendError::ExternalCommandFailed {
        stderr: format!("decode portal screenshot: {e}"),
    })?;
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

// ---- RemoteDesktop → EIS input ----

pub async fn ensure_session(bus: &Connection) -> Result<PortalSession, BackendError> {
    let proxy = portal_proxy(bus, "org.freedesktop.portal.RemoteDesktop").await?;
    let sender = sender_token(bus)?;

    // 1. CreateSession
    let session_token = new_token();
    let handle_token = new_token();
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("session_handle_token", session_token.as_str().into());
    options.insert("handle_token", handle_token.as_str().into());
    let handle: OwnedObjectPath =
        format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}")
            .try_into()
            .map_err(|error| BackendError::Failed(format!("{error}")))?;
    let mut stream = response_stream(bus, handle).await?;
    let request: OwnedObjectPath = proxy
        .call("CreateSession", &(options,))
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("CreateSession: {e}"),
        })?;
    stream.verify_handle(request)?;
    let (code, results) = await_response(stream).await?;
    if code != 0 {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("CreateSession denied (code {code})"),
        });
    }
    let session: OwnedObjectPath = results
        .get("session_handle")
        .and_then(|v| OwnedObjectPath::try_from(v.clone()).ok())
        .ok_or_else(|| BackendError::ExternalCommandFailed {
            stderr: "CreateSession missing session_handle".into(),
        })?;
    let session_guard = PortalSession {
        bus: bus.clone(),
        path: session.clone(),
    };

    // 2. SelectDevices keyboard+pointer
    let handle_token = new_token();
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", handle_token.as_str().into());
    options.insert("types", 3u32.into()); // keyboard | pointer
    let handle: OwnedObjectPath =
        format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}")
            .try_into()
            .map_err(|error| BackendError::Failed(format!("{error}")))?;
    let mut stream = response_stream(bus, handle).await?;
    let request: OwnedObjectPath = proxy
        .call("SelectDevices", &(&session, options))
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("SelectDevices: {e}"),
        })?;
    stream.verify_handle(request)?;
    let (code, _) = await_response(stream).await?;
    if code != 0 {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("SelectDevices denied (code {code})"),
        });
    }

    // 3. Start
    let handle_token = new_token();
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", handle_token.as_str().into());
    let handle: OwnedObjectPath =
        format!("/org/freedesktop/portal/desktop/request/{sender}/{handle_token}")
            .try_into()
            .map_err(|error| BackendError::Failed(format!("{error}")))?;
    let mut stream = response_stream(bus, handle).await?;
    let request: OwnedObjectPath = proxy
        .call("Start", &(&session, "", options))
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("Start: {e}"),
        })?;
    stream.verify_handle(request)?;
    let (code, _) = await_response(stream).await?;
    if code != 0 {
        return Err(BackendError::ExternalCommandFailed {
            stderr: format!("Start denied — approve the share dialog (code {code})"),
        });
    }
    Ok(session_guard)
}

/// One EIS fd from an established session (no dialog after Start).
pub async fn connect_eis_fd(
    bus: &Connection,
    session: &OwnedObjectPath,
) -> Result<std::os::fd::OwnedFd, BackendError> {
    use std::os::fd::{FromRawFd, IntoRawFd};
    let proxy = portal_proxy(bus, "org.freedesktop.portal.RemoteDesktop").await?;
    let options: HashMap<&str, Value<'_>> = HashMap::new();
    let fd: zvariant::OwnedFd = proxy
        .call("ConnectToEIS", &(session, options))
        .await
        .map_err(|e| BackendError::BusDisconnected {
            detail: format!("ConnectToEIS: {e}"),
        })?;
    let std_fd: std::os::fd::OwnedFd = fd.into();
    let raw = std_fd.into_raw_fd();
    Ok(unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) })
}
