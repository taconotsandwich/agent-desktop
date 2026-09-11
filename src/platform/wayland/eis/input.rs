use super::session::EisSession;
use crate::{
    error::BackendError,
    platform::drivers::{Button, InputDriver, Probe},
    platform::keymap::{self, Modifier},
    types::ToolError,
};
use std::os::fd::{IntoRawFd, OwnedFd};
use tokio::sync::{Mutex, MutexGuard};

#[async_trait::async_trait]
pub trait EisConnector: Send + Sync {
    fn id(&self) -> &'static str;
    async fn probe(&self) -> Probe;
    async fn connect(&self) -> Result<OwnedFd, BackendError>;
}

pub struct EisInput<C> {
    connector: C,
    session: Mutex<Option<EisSession>>,
}
impl<C: EisConnector> EisInput<C> {
    pub fn new(connector: C) -> Self {
        Self {
            connector,
            session: Mutex::new(None),
        }
    }
    async fn session(&self) -> Result<Lease<'_>, BackendError> {
        let mut guard = self.session.lock().await;
        if guard
            .as_ref()
            .is_some_and(|session| !session.is_connected())
        {
            guard.take();
        }
        if guard.is_none() {
            let fd = self.connector.connect().await?;
            *guard = Some(EisSession::open_from_fd(fd.into_raw_fd(), true, true).await?);
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        Ok(Lease(guard))
    }
}
struct Lease<'a>(MutexGuard<'a, Option<EisSession>>);
impl std::ops::Deref for Lease<'_> {
    type Target = EisSession;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("connected EIS session")
    }
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        self.release_held();
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
impl<C: EisConnector> InputDriver for EisInput<C> {
    fn id(&self) -> &'static str {
        self.connector.id()
    }
    async fn probe(&self) -> Probe {
        self.connector.probe().await
    }
    async fn prepare(&self) -> Result<(), ToolError> {
        let _session = self.session().await.map_err(|error| error.tool(false))?;
        Ok(())
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
                let s = self.session().await?;
                s.click(x, y, button_code(button), 1).await
            } else {
                let s = self.session().await?;
                s.click_with_modifiers(x, y, button_code(button), &hold)
                    .await
            }
        };
        run.await.map_err(|e: BackendError| e.tool(true))
    }

    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError> {
        let s = self.session().await.map_err(|e| e.tool(true))?;
        s.pointer_move(x, y).await.map_err(|e| e.tool(true))
    }

    async fn drag(
        &self,
        path: Vec<(i32, i32)>,
        button: Button,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), ToolError> {
        let (start, rest) = path.split_first().ok_or_else(|| {
            BackendError::Unsupported {
                reason: "drag needs ≥1 point".into(),
            }
            .tool(false)
        })?;
        let s = self.session().await.map_err(|e| e.tool(true))?;
        s.drag(start, rest, button_code(button), dwell_ms, step_ms)
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
                let s = self.session().await?;
                s.scroll(x, y, dx as f32, dy as f32).await
            } else {
                let s = self.session().await?;
                s.scroll_with_modifiers(x, y, dx as f32, dy as f32, &hold)
                    .await
            }
        };
        run.await.map_err(|e: BackendError| e.tool(true))
    }

    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        if let Some(ch) = text
            .chars()
            .find(|c| keymap::keycode_for_char(*c).is_none())
        {
            return Err(BackendError::Unsupported {
                reason: format!("no evdev mapping for {ch:?}; use clipboard paste path"),
            }
            .tool(false));
        }
        let s = self.session().await.map_err(|e| e.tool(true))?;
        s.type_ascii(&text).await.map_err(|e| e.tool(true))
    }

    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(BackendError::Unsupported {
                reason: "empty chord".into(),
            }
            .tool(false));
        }
        let s = self.session().await.map_err(|e| e.tool(true))?;
        for chord_str in &keys {
            let chord = keymap::parse_chord(chord_str).map_err(|e| e.tool(false))?;
            s.chord(&chord.modifiers, chord.key)
                .await
                .map_err(|e| e.tool(true))?;
        }
        Ok(())
    }
}
