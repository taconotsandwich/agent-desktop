//! X11 input via the XTEST extension (the same mechanism xdotool uses).
//!
//! Events are injected into the server's normal input pipeline: they reach
//! whatever window holds focus, exactly like the previous xdotool driver.
//! Keyboard names resolve against the server's current keyboard mapping, so
//! there is no xmodmap subprocess and no US-layout assumption for named keys.

use super::{X11, connection, failed};
use crate::{
    error::BackendError,
    platform::{
        drivers::{Button, InputDriver, Probe},
        keymap,
    },
    types::ToolError,
};
use std::sync::Arc;
use std::time::Duration;
use x11rb::{
    connection::Connection as _,
    protocol::{
        xproto::{
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
            MOTION_NOTIFY_EVENT,
        },
        xtest::ConnectionExt as _,
    },
};

pub struct X11Input;

fn button_code(button: Button) -> u8 {
    match button {
        Button::Left => 1,
        Button::Middle => 2,
        Button::Right => 3,
    }
}

fn fake(x11: &X11, type_: u8, detail: u8, x: i16, y: i16) -> Result<(), BackendError> {
    x11.connection
        .xtest_fake_input(type_, detail, x11rb::CURRENT_TIME, x11.root, x, y, 0)
        .map_err(failed)?;
    x11.connection.flush().map_err(failed)
}

/// Best-effort release of anything still held if a dispatch fails midway.
struct HeldKeys {
    x11: Arc<X11>,
    keys: Vec<u32>,
    buttons: Vec<u8>,
}

impl HeldKeys {
    fn new(x11: &Arc<X11>) -> Self {
        Self {
            x11: x11.clone(),
            keys: Vec::new(),
            buttons: Vec::new(),
        }
    }
    fn press_key(&mut self, keycode: u32) -> Result<(), BackendError> {
        self.keys.push(keycode);
        fake(
            &self.x11,
            KEY_PRESS_EVENT,
            u8::try_from(keycode).map_err(failed)?,
            0,
            0,
        )
    }
    fn release_key(&mut self, keycode: u32) -> Result<(), BackendError> {
        self.keys.retain(|held| *held != keycode);
        fake(
            &self.x11,
            KEY_RELEASE_EVENT,
            u8::try_from(keycode).map_err(failed)?,
            0,
            0,
        )
    }
    fn press_button(&mut self, button: u8) -> Result<(), BackendError> {
        self.buttons.push(button);
        fake(&self.x11, BUTTON_PRESS_EVENT, button, 0, 0)
    }
    fn release_button(&mut self, button: u8) -> Result<(), BackendError> {
        self.buttons.retain(|held| *held != button);
        fake(&self.x11, BUTTON_RELEASE_EVENT, button, 0, 0)
    }
}

impl Drop for HeldKeys {
    fn drop(&mut self) {
        for button in self.buttons.drain(..).rev() {
            let _ = fake(&self.x11, BUTTON_RELEASE_EVENT, button, 0, 0);
        }
        for key in self.keys.drain(..).rev() {
            let _ = fake(&self.x11, KEY_RELEASE_EVENT, key as u8, 0, 0);
        }
    }
}

fn move_pointer(x11: &X11, x: i32, y: i32) -> Result<(), BackendError> {
    fake(x11, MOTION_NOTIFY_EVENT, 0, x as i16, y as i16)
}

#[async_trait::async_trait]
impl InputDriver for X11Input {
    fn id(&self) -> &'static str {
        "x11-xtest"
    }

    async fn probe(&self) -> Probe {
        let result = async {
            let x11 = connection()?;
            x11.connection
                .xtest_get_version(2, 2)
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
                .map(|()| "XTEST extension available".into())
                .unwrap_or_else(|error| error.to_string()),
        }
    }

    async fn click(
        &self,
        x: i32,
        y: i32,
        button: Button,
        hold: Vec<keymap::Modifier>,
    ) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let modifiers = super::keyboard::read(&x11)
            .and_then(|keyboard| keyboard.modifiers(&hold))
            .map_err(|error| error.tool(false))?;
        let mut held = HeldKeys::new(&x11);
        move_pointer(&x11, x, y).map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        for &keycode in &modifiers {
            held.press_key(keycode).map_err(|error| error.tool(true))?;
        }
        held.press_button(button_code(button))
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        held.release_button(button_code(button))
            .map_err(|error| error.tool(true))?;
        for &keycode in modifiers.iter().rev() {
            held.release_key(keycode)
                .map_err(|error| error.tool(true))?;
        }
        Ok(())
    }

    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        move_pointer(&x11, x, y).map_err(|error| error.tool(true))
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
        let x11 = connection().map_err(|error| error.tool(false))?;
        let mut held = HeldKeys::new(&x11);
        move_pointer(&x11, start.0, start.1).map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        held.press_button(button_code(button))
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(dwell_ms)).await;
        let mut previous = *start;
        for &(end_x, end_y) in rest {
            for step in 1..=10 {
                let x = previous.0 + (end_x - previous.0) * step / 10;
                let y = previous.1 + (end_y - previous.1) * step / 10;
                move_pointer(&x11, x, y).map_err(|error| error.tool(true))?;
                tokio::time::sleep(Duration::from_millis(step_ms)).await;
            }
            previous = (end_x, end_y);
        }
        held.release_button(button_code(button))
            .map_err(|error| error.tool(true))?;
        Ok(())
    }

    async fn scroll(
        &self,
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        hold: Vec<keymap::Modifier>,
    ) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let modifiers = super::keyboard::read(&x11)
            .and_then(|keyboard| keyboard.modifiers(&hold))
            .map_err(|error| error.tool(false))?;
        let mut held = HeldKeys::new(&x11);
        move_pointer(&x11, x, y).map_err(|error| error.tool(true))?;
        for &keycode in &modifiers {
            held.press_key(keycode).map_err(|error| error.tool(true))?;
        }
        // Buttons 4/5/6/7 = up/down/left/right, one click per 120-unit notch.
        for (delta, negative, positive) in [(dx, 6u8, 7u8), (dy, 4u8, 5u8)] {
            if delta == 0 {
                continue;
            }
            let button = if delta < 0 { negative } else { positive };
            for _ in 0..delta.unsigned_abs().div_ceil(120).max(1) {
                held.press_button(button)
                    .map_err(|error| error.tool(true))?;
                held.release_button(button)
                    .map_err(|error| error.tool(true))?;
            }
        }
        for &keycode in modifiers.iter().rev() {
            held.release_key(keycode)
                .map_err(|error| error.tool(true))?;
        }
        Ok(())
    }

    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let mut held = HeldKeys::new(&x11);
        for ch in text.chars() {
            let stroke = super::keyboard::read(&x11)
                .and_then(|keyboard| keyboard.literal(ch))
                .map_err(|error| error.tool(true))?;
            dispatch(&mut held, stroke, Duration::from_millis(5)).await?;
        }
        Ok(())
    }

    async fn key(&self, keys: Vec<keymap::Chord>) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(keymap::unsupported("empty chord").tool(false));
        }
        let x11 = connection().map_err(|error| error.tool(false))?;
        let mut held = HeldKeys::new(&x11);
        for chord in keys {
            let stroke = super::keyboard::read(&x11)
                .and_then(|keyboard| keyboard.chord(&chord))
                .map_err(|error| error.tool(false))?;
            dispatch(&mut held, stroke, Duration::from_millis(10)).await?;
        }
        Ok(())
    }
}

async fn dispatch(
    held: &mut HeldKeys,
    stroke: crate::platform::keyboard::Stroke,
    hold: Duration,
) -> Result<(), ToolError> {
    for &key in &stroke.modifiers {
        held.press_key(key).map_err(|e| e.tool(true))?;
    }
    held.press_key(stroke.key).map_err(|e| e.tool(true))?;
    tokio::time::sleep(hold).await;
    held.release_key(stroke.key).map_err(|e| e.tool(true))?;
    for &key in stroke.modifiers.iter().rev() {
        held.release_key(key).map_err(|e| e.tool(true))?;
    }
    tokio::time::sleep(Duration::from_millis(10)).await;
    Ok(())
}
