//! Portal RemoteDesktop input → [`InputDriver`].
//!
//! Session is established once (portal dialog on first Start); each tool call
//! takes a fresh EIS fd from the cached session and runs the shared libei
//! handshake (`EisSession::open_from_fd`). Dispatch is identical to KWin's.

use super::kwin_input::EisSession;
use super::portal::{connect_eis_fd, ensure_session, portal_present};
use crate::drivers::{Button, InputDriver, Probe};
use crate::error::BackendError;
use crate::keymap::{self, Modifier};
use crate::types::ToolError;
use std::os::fd::IntoRawFd;
use tokio::sync::OnceCell;
use zbus::Connection;
use zvariant::OwnedObjectPath;

pub struct PortalInput {
    bus: Connection,
    session: OnceCell<OwnedObjectPath>,
}

impl PortalInput {
    pub fn new(bus: Connection) -> Self {
        Self {
            bus,
            session: OnceCell::new(),
        }
    }

    async fn session(&self) -> Result<OwnedObjectPath, BackendError> {
        self.session
            .get_or_try_init(|| ensure_session(&self.bus))
            .await
            .cloned()
    }

    async fn open_pointer(&self) -> Result<EisSession, BackendError> {
        let session = self.session().await?;
        let fd = connect_eis_fd(&self.bus, &session).await?;
        EisSession::open_from_fd(fd.into_raw_fd(), false, true).await
    }

    async fn open_keyboard(&self) -> Result<EisSession, BackendError> {
        let session = self.session().await?;
        let fd = connect_eis_fd(&self.bus, &session).await?;
        EisSession::open_from_fd(fd.into_raw_fd(), true, false).await
    }

    async fn open_combined(&self) -> Result<EisSession, BackendError> {
        let session = self.session().await?;
        let fd = connect_eis_fd(&self.bus, &session).await?;
        EisSession::open_from_fd(fd.into_raw_fd(), true, true).await
    }
}

fn button_code(b: Button) -> u32 {
    match b {
        Button::Left => keymap::BTN_LEFT,
        Button::Right => keymap::BTN_RIGHT,
        Button::Middle => keymap::BTN_MIDDLE,
    }
}

#[async_trait::async_trait]
impl InputDriver for PortalInput {
    fn id(&self) -> &'static str {
        "portal-remotedesktop"
    }

    async fn probe(&self) -> Probe {
        let ok = portal_present(&self.bus).await;
        Probe {
            id: self.id(),
            ok,
            detail: if ok {
                "portal Desktop present (dialog on first Start)".into()
            } else {
                "org.freedesktop.portal.Desktop missing".into()
            },
        }
    }

    async fn click(
        &self,
        x: i32,
        y: i32,
        button: Button,
        hold: Vec<Modifier>,
    ) -> Result<(), ToolError> {
        let run = async {
            if hold.is_empty() {
                let s = self.open_pointer().await?;
                s.click(x, y, button_code(button), 1).await
            } else {
                let s = self.open_combined().await?;
                s.click_with_modifiers(x, y, button_code(button), &hold).await
            }
        };
        run.await.map_err(|e: BackendError| e.tool(true))
    }

    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError> {
        let s = self.open_pointer().await.map_err(|e| e.tool(true))?;
        s.pointer_move(x, y).await.map_err(|e| e.tool(true))
    }

    async fn drag(&self, path: Vec<(i32, i32)>, button: Button) -> Result<(), ToolError> {
        let (start, rest) = path.split_first().ok_or_else(|| {
            BackendError::Unsupported {
                reason: "drag needs ≥1 point".into(),
            }
            .tool(false)
        })?;
        let s = self.open_pointer().await.map_err(|e| e.tool(true))?;
        s.drag(start, rest, button_code(button))
            .await
            .map_err(|e| e.tool(true))
    }

    async fn scroll(
        &self,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        hold: Vec<Modifier>,
    ) -> Result<(), ToolError> {
        let run = async {
            if hold.is_empty() {
                let s = self.open_pointer().await?;
                s.scroll(x, y, dx as f32 * 15.0, dy as f32 * 15.0).await
            } else {
                let s = self.open_combined().await?;
                s.scroll_with_modifiers(x, y, dx as f32 * 15.0, dy as f32 * 15.0, &hold)
                    .await
            }
        };
        run.await.map_err(|e: BackendError| e.tool(true))
    }

    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        if let Some(ch) = text.chars().find(|c| keymap::keycode_for_char(*c).is_none()) {
            return Err(BackendError::Unsupported {
                reason: format!("no evdev mapping for {ch:?}; use clipboard paste path"),
            }
            .tool(false));
        }
        let s = self.open_keyboard().await.map_err(|e| e.tool(true))?;
        s.type_ascii(&text).await.map_err(|e| e.tool(true))
    }

    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(BackendError::Unsupported {
                reason: "empty chord".into(),
            }
            .tool(false));
        }
        let s = self.open_keyboard().await.map_err(|e| e.tool(true))?;
        for chord_str in &keys {
            let chord = keymap::parse_chord(chord_str).map_err(|e| e.tool(false))?;
            s.chord(&chord.modifiers, chord.key)
                .await
                .map_err(|e| e.tool(true))?;
        }
        Ok(())
    }
}
