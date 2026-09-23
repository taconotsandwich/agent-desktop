//! Window-targeted X11 input over `SendEvent`.
//!
//! XTEST feeds the server's input pipeline, so its events reach whatever
//! window holds focus and move the real pointer. `SendEvent` with an empty
//! event mask hands a synthetic core event to the client that created the
//! destination window instead: the window manager never sees it, focus stays
//! where it is and the pointer does not move. This is the mechanism behind
//! `xdotool --window`. Chords press their modifier keys like a physical
//! keyboard and every event carries the modifier state in its `state` field,
//! so toolkits that track the modifier keys (Blender) and toolkits that
//! rebuild the keyboard state from the field (Qt) agree. Around the events
//! the client is told that its window has focus and then that it lost it
//! again: Qt fires a window's action shortcuts only while it believes the
//! window focused, and asks the window manager to activate a clicked window
//! it believes unfocused.

mod sender;

use self::sender::{Clock, Sender, undelivered};
use super::{
    X11, connection,
    input::button_code,
    windows::{client_of, window_of},
};
use crate::{
    error::BackendError,
    platform::{
        drivers::{Button, Probe, Ref, TargetedInputDriver, WindowInfo},
        keyboard::{Keyboard, Stroke},
        keymap,
    },
    types::ToolError,
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};

#[derive(Default)]
pub struct X11Targeted {
    clock: OnceLock<Clock>,
}

/// Keycodes of each stroke in press order, modifiers first, each paired with
/// the core `state` once it is down: the state its release reports and the
/// next press carries. Everything is resolved before anything is sent, so a
/// symbol the layout cannot produce fails with nothing typed and the caller's
/// paste fallback does not duplicate text. `Keyboard` wraps raw XKB pointers
/// and must not live across an await.
fn plan(
    x11: &X11,
    strokes: impl FnOnce(&Keyboard) -> Result<Vec<Stroke>, BackendError>,
) -> Result<Vec<Vec<(u8, u16)>>, BackendError> {
    let keyboard = super::keyboard::read(x11)?.ignoring_held();
    strokes(&keyboard)?
        .iter()
        .map(|stroke| {
            let keys: Vec<u32> = stroke
                .modifiers
                .iter()
                .chain(std::iter::once(&stroke.key))
                .copied()
                .collect();
            keys.iter()
                .enumerate()
                .map(|(index, &key)| -> Result<(u8, u16), BackendError> {
                    let keycode = u8::try_from(key)
                        .map_err(|_| keymap::unsupported("keycode does not fit a core event"))?;
                    Ok((keycode, keyboard.mask(&keys[..=index])))
                })
                .collect()
        })
        .collect()
}

impl X11Targeted {
    async fn clock(&self, x11: &X11) -> Result<Clock, BackendError> {
        if let Some(clock) = self.clock.get() {
            return Ok(*clock);
        }
        let clock = Clock::sample(x11).await?;
        let _ = self.clock.set(clock);
        Ok(*self.clock.get().expect("clock is set"))
    }

    async fn sender(&self, x11: &Arc<X11>, target: &Ref) -> Result<Sender, ToolError> {
        let window = window_of(target)?;
        let clock = self.clock(x11).await.map_err(|error| error.tool(false))?;
        let modifiers = super::keyboard::read(x11)
            .map_err(|error| error.tool(false))?
            .ignoring_held()
            .mask(&[]);
        Sender::new(x11, clock, window, modifiers).map_err(undelivered)
    }

    async fn click_in(
        &self,
        x11: &Arc<X11>,
        target: &Ref,
        x: i32,
        y: i32,
        button: Button,
        count: u32,
    ) -> Result<(), ToolError> {
        let mut sender = self.sender(x11, target).await?;
        sender
            .require_inside(x, y)
            .map_err(|error| error.tool(false))?;
        sender.lend_focus().await?;
        sender.motion(x, y).map_err(undelivered)?;
        for index in 0..count.max(1) {
            if index > 0 {
                // Well inside Qt's 400 ms double-click interval.
                tokio::time::sleep(Duration::from_millis(60)).await;
            }
            sender.click(button_code(button)).await?;
        }
        Ok(())
    }

    async fn drag_in(
        &self,
        x11: &Arc<X11>,
        target: &Ref,
        path: Vec<(i32, i32)>,
        button: Button,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), ToolError> {
        let (start, rest) = path
            .split_first()
            .ok_or_else(|| keymap::unsupported("drag needs ≥1 point").tool(false))?;
        let mut sender = self.sender(x11, target).await?;
        sender
            .require_inside(start.0, start.1)
            .map_err(|error| error.tool(false))?;
        sender.lend_focus().await?;
        let button = button_code(button);
        sender.motion(start.0, start.1).map_err(undelivered)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        sender.press_button(button).map_err(undelivered)?;
        tokio::time::sleep(Duration::from_millis(dwell_ms)).await;
        let mut previous = *start;
        for &(end_x, end_y) in rest {
            for step in 1..=10 {
                let x = previous.0 + (end_x - previous.0) * step / 10;
                let y = previous.1 + (end_y - previous.1) * step / 10;
                sender.motion(x, y).map_err(undelivered)?;
                tokio::time::sleep(Duration::from_millis(step_ms)).await;
            }
            previous = (end_x, end_y);
        }
        sender
            .release_button(button)
            .map_err(|error| error.tool(true))
    }

    async fn scroll_in(
        &self,
        x11: &Arc<X11>,
        target: &Ref,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
    ) -> Result<(), ToolError> {
        let mut sender = self.sender(x11, target).await?;
        sender
            .require_inside(x, y)
            .map_err(|error| error.tool(false))?;
        sender.lend_focus().await?;
        sender.motion(x, y).map_err(undelivered)?;
        // Buttons 4/5/6/7 = up/down/left/right, one click per 120-unit notch.
        for (delta, negative, positive) in [(dx, 6u8, 7u8), (dy, 4u8, 5u8)] {
            if delta == 0 {
                continue;
            }
            let button = if delta < 0 { negative } else { positive };
            for _ in 0..delta.unsigned_abs().div_ceil(120).max(1) {
                sender.press_button(button).map_err(undelivered)?;
                sender
                    .release_button(button)
                    .map_err(|error| error.tool(true))?;
            }
        }
        Ok(())
    }

    async fn type_in(&self, x11: &Arc<X11>, target: &Ref, text: &str) -> Result<(), ToolError> {
        let mut sender = self.sender(x11, target).await?;
        let strokes = plan(x11, |keyboard| {
            text.chars().map(|ch| keyboard.literal(ch)).collect()
        })
        .map_err(|error| error.tool(false))?;
        sender.lend_focus().await?;
        for keys in strokes {
            sender.stroke(&keys, Duration::from_millis(5)).await?;
        }
        Ok(())
    }

    async fn key_in(
        &self,
        x11: &Arc<X11>,
        target: &Ref,
        keys: &[keymap::Chord],
        hold: Duration,
    ) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(keymap::unsupported("empty chord").tool(false));
        }
        let mut sender = self.sender(x11, target).await?;
        let strokes = plan(x11, |keyboard| {
            keys.iter().map(|chord| keyboard.chord(chord)).collect()
        })
        .map_err(|error| error.tool(false))?;
        sender.lend_focus().await?;
        for keys in strokes {
            sender.stroke(&keys, hold).await?;
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl TargetedInputDriver for X11Targeted {
    fn id(&self) -> &'static str {
        "x11-sendevent"
    }

    async fn probe(&self) -> Probe {
        let result = async {
            let x11 = connection()?;
            super::keyboard::read(&x11)?;
            self.clock(&x11).await?;
            Ok::<(), BackendError>(())
        }
        .await;
        Probe {
            id: self.id(),
            ok: result.is_ok(),
            detail: result
                .map(|()| "SendEvent to X11 windows available".into())
                .unwrap_or_else(|error| error.to_string()),
        }
    }

    /// X11 refs address their window directly. A compositor window is
    /// addressable only when an Xwayland client backs it: the client with
    /// the window's process, told apart from its siblings by title.
    /// Coordinates pass through unchanged, which assumes the compositor
    /// scales Xwayland (the Plasma 6 and GNOME default) so X11 and logical
    /// coordinates agree.
    async fn resolve(&self, window: &WindowInfo) -> Result<Option<Ref>, ToolError> {
        if window.window_ref.0.starts_with("x11:") {
            return Ok(Some(window.window_ref.clone()));
        }
        let Some(pid) = window
            .pid
            .filter(|_| window.client_protocol.as_deref() != Some("wayland"))
        else {
            return Ok(None);
        };
        let x11 = connection().map_err(|error| error.tool(false))?;
        Ok(client_of(&x11, pid, &window.title)
            .map_err(|error| error.tool(true))?
            .map(|client| Ref(format!("x11:0x{client:08x}"))))
    }

    async fn click(
        &self,
        target: &Ref,
        x: i32,
        y: i32,
        button: Button,
        count: u32,
    ) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        self.click_in(&x11, target, x, y, button, count).await
    }

    async fn drag(
        &self,
        target: &Ref,
        path: Vec<(i32, i32)>,
        button: Button,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        self.drag_in(&x11, target, path, button, dwell_ms, step_ms)
            .await
    }

    async fn scroll(
        &self,
        target: &Ref,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
    ) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        self.scroll_in(&x11, target, x, y, dx, dy).await
    }

    async fn type_text(&self, target: &Ref, text: String) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        self.type_in(&x11, target, &text).await
    }

    async fn key(
        &self,
        target: &Ref,
        keys: Vec<keymap::Chord>,
        hold: Duration,
    ) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        self.key_in(&x11, target, &keys, hold).await
    }
}

#[cfg(test)]
mod tests;
