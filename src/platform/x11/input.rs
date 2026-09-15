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
            BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ConnectionExt as _, KEY_PRESS_EVENT,
            KEY_RELEASE_EVENT, MOTION_NOTIFY_EVENT,
        },
        xtest::ConnectionExt as _,
    },
};

const XK_SHIFT_L: u32 = 0xffe1;
const XK_CONTROL_L: u32 = 0xffe3;
const XK_ALT_L: u32 = 0xffe9;
const XK_SUPER_L: u32 = 0xffeb;

pub struct X11Input;

fn modifier_keysym(modifier: keymap::Modifier) -> u32 {
    match modifier {
        keymap::Modifier::Ctrl => XK_CONTROL_L,
        keymap::Modifier::Shift => XK_SHIFT_L,
        keymap::Modifier::Alt => XK_ALT_L,
        keymap::Modifier::Super => XK_SUPER_L,
    }
}

/// Named keys as X11 keysyms. F-keys are not contiguous in evdev (F11/F12
/// sit at 87/88), so each maps explicitly.
fn named_keysym(code: u32) -> Option<u32> {
    Some(match code {
        keymap::KEY_ESC => 0xff1b,
        keymap::KEY_BACKSPACE => 0xff08,
        keymap::KEY_TAB => 0xff09,
        keymap::KEY_ENTER => 0xff0d,
        keymap::KEY_SPACE => 0x0020,
        keymap::KEY_HOME => 0xff50,
        keymap::KEY_UP => 0xff52,
        keymap::KEY_PAGEUP => 0xff55,
        keymap::KEY_LEFT => 0xff51,
        keymap::KEY_RIGHT => 0xff53,
        keymap::KEY_END => 0xff57,
        keymap::KEY_DOWN => 0xff54,
        keymap::KEY_PAGEDOWN => 0xff56,
        keymap::KEY_INSERT => 0xff63,
        keymap::KEY_DELETE => 0xffff,
        keymap::KEY_F1 => 0xffbe,
        keymap::KEY_F2 => 0xffbf,
        keymap::KEY_F3 => 0xffc0,
        keymap::KEY_F4 => 0xffc1,
        keymap::KEY_F5 => 0xffc2,
        keymap::KEY_F6 => 0xffc3,
        keymap::KEY_F7 => 0xffc4,
        keymap::KEY_F8 => 0xffc5,
        keymap::KEY_F9 => 0xffc6,
        keymap::KEY_F10 => 0xffc7,
        keymap::KEY_F11 => 0xffc8,
        keymap::KEY_F12 => 0xffc9,
        _ => return None,
    })
}

fn chord_keysym(text: &str, chord: &keymap::Chord) -> Option<u32> {
    let token = text.rsplit('+').next()?.trim();
    let mut chars = token.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) if ch.is_ascii() => Some(ch as u32),
        (Some(_), None) => None,
        _ => named_keysym(chord.key),
    }
}

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

struct Keyboard {
    min_keycode: u8,
    per_keycode: usize,
    keysyms: Vec<u32>,
}

impl Keyboard {
    fn read(x11: &X11) -> Result<Self, BackendError> {
        let (min_keycode, max_keycode) = {
            let setup = x11.connection.setup();
            (setup.min_keycode, setup.max_keycode)
        };
        let reply = x11
            .connection
            .get_keyboard_mapping(min_keycode, max_keycode - min_keycode + 1)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        Ok(Self {
            min_keycode,
            per_keycode: reply.keysyms_per_keycode as usize,
            keysyms: reply.keysyms,
        })
    }

    /// First key position carrying `keysym`, with whether that level needs
    /// Shift held.
    fn lookup(&self, keysym: u32) -> Option<(u8, bool)> {
        self.keysyms
            .chunks(self.per_keycode.max(1))
            .enumerate()
            .find_map(|(index, syms)| {
                syms.iter()
                    .position(|sym| *sym == keysym)
                    .map(|level| ((self.min_keycode as usize + index) as u8, level % 2 == 1))
            })
    }
}

/// Best-effort release of anything still held if a dispatch fails midway.
struct HeldKeys {
    x11: Arc<X11>,
    keys: Vec<u8>,
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
    fn press_key(&mut self, keycode: u8) -> Result<(), BackendError> {
        self.keys.push(keycode);
        fake(&self.x11, KEY_PRESS_EVENT, keycode, 0, 0)
    }
    fn release_key(&mut self, keycode: u8) -> Result<(), BackendError> {
        self.keys.retain(|held| *held != keycode);
        fake(&self.x11, KEY_RELEASE_EVENT, keycode, 0, 0)
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
            let _ = fake(&self.x11, KEY_RELEASE_EVENT, key, 0, 0);
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
        let keyboard = Keyboard::read(&x11).map_err(|error| error.tool(true))?;
        let mut held = HeldKeys::new(&x11);
        move_pointer(&x11, x, y).map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        for modifier in &hold {
            let (keycode, _) = keyboard
                .lookup(modifier_keysym(*modifier))
                .ok_or_else(|| unknown_key(format!("{modifier:?}")))?;
            held.press_key(keycode).map_err(|error| error.tool(true))?;
        }
        held.press_button(button_code(button))
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        held.release_button(button_code(button))
            .map_err(|error| error.tool(true))?;
        for modifier in hold.iter().rev() {
            let (keycode, _) = keyboard
                .lookup(modifier_keysym(*modifier))
                .ok_or_else(|| unknown_key(format!("{modifier:?}")))?;
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
        let keyboard = Keyboard::read(&x11).map_err(|error| error.tool(true))?;
        let mut held = HeldKeys::new(&x11);
        move_pointer(&x11, x, y).map_err(|error| error.tool(true))?;
        for modifier in &hold {
            let (keycode, _) = keyboard
                .lookup(modifier_keysym(*modifier))
                .ok_or_else(|| unknown_key(format!("{modifier:?}")))?;
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
        for modifier in hold.iter().rev() {
            let (keycode, _) = keyboard
                .lookup(modifier_keysym(*modifier))
                .ok_or_else(|| unknown_key(format!("{modifier:?}")))?;
            held.release_key(keycode)
                .map_err(|error| error.tool(true))?;
        }
        Ok(())
    }

    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let keyboard = Keyboard::read(&x11).map_err(|error| error.tool(true))?;
        let mut held = HeldKeys::new(&x11);
        let (shift, _) = keyboard
            .lookup(XK_SHIFT_L)
            .ok_or_else(|| unknown_key("Shift".into()))?;
        for ch in text.chars() {
            if !ch.is_ascii() {
                return Err(BackendError::Unsupported {
                    reason: format!("no direct X11 typing for {ch:?}"),
                }
                .tool(true));
            }
            let keysym = ch as u32;
            let (keycode, needs_shift) = keyboard
                .lookup(keysym)
                .ok_or_else(|| unknown_key(format!("{ch:?}")))?;
            if needs_shift {
                held.press_key(shift).map_err(|error| error.tool(true))?;
            }
            held.press_key(keycode).map_err(|error| error.tool(true))?;
            tokio::time::sleep(Duration::from_millis(5)).await;
            held.release_key(keycode)
                .map_err(|error| error.tool(true))?;
            if needs_shift {
                held.release_key(shift).map_err(|error| error.tool(true))?;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        Ok(())
    }

    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(BackendError::Unsupported {
                reason: "empty chord".into(),
            }
            .tool(false));
        }
        let x11 = connection().map_err(|error| error.tool(false))?;
        let keyboard = Keyboard::read(&x11).map_err(|error| error.tool(true))?;
        let mut held = HeldKeys::new(&x11);
        for chord_text in keys {
            let chord = keymap::parse_chord(&chord_text).map_err(|error| error.tool(false))?;
            let keysym =
                chord_keysym(&chord_text, &chord).ok_or_else(|| unknown_key(chord_text.clone()))?;
            let (keycode, needs_shift) = keyboard
                .lookup(keysym)
                .ok_or_else(|| unknown_key(chord_text.clone()))?;
            let mut modifiers: Vec<u8> = chord
                .modifiers
                .iter()
                .map(|modifier| {
                    keyboard
                        .lookup(modifier_keysym(*modifier))
                        .map(|(keycode, _)| keycode)
                        .ok_or_else(|| unknown_key(format!("{modifier:?}")))
                })
                .collect::<Result<_, _>>()?;
            if needs_shift && !chord.modifiers.contains(&keymap::Modifier::Shift) {
                let (shift, _) = keyboard
                    .lookup(XK_SHIFT_L)
                    .ok_or_else(|| unknown_key("Shift".into()))?;
                modifiers.push(shift);
            }
            for modifier in &modifiers {
                held.press_key(*modifier)
                    .map_err(|error| error.tool(true))?;
            }
            held.press_key(keycode).map_err(|error| error.tool(true))?;
            tokio::time::sleep(Duration::from_millis(10)).await;
            held.release_key(keycode)
                .map_err(|error| error.tool(true))?;
            for modifier in modifiers.iter().rev() {
                held.release_key(*modifier)
                    .map_err(|error| error.tool(true))?;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok(())
    }
}

fn unknown_key(detail: String) -> ToolError {
    BackendError::Unsupported {
        reason: format!("no X11 keycode for {detail}"),
    }
    .tool(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_keys_map_to_contiguous_keysyms() {
        assert_eq!(named_keysym(keymap::KEY_F1), Some(0xffbe));
        assert_eq!(named_keysym(keymap::KEY_F10), Some(0xffc7));
        assert_eq!(named_keysym(keymap::KEY_F11), Some(0xffc8));
        assert_eq!(named_keysym(keymap::KEY_F12), Some(0xffc9));
    }

    #[test]
    fn lookup_reports_shift_levels() {
        let keyboard = Keyboard {
            min_keycode: 8,
            per_keycode: 2,
            keysyms: vec![0, 0, 0x73, 0x53, 0xff1b, 0],
        };
        assert_eq!(keyboard.lookup(0x73), Some((9, false)));
        assert_eq!(keyboard.lookup(0x53), Some((9, true)));
        assert_eq!(keyboard.lookup(0xff1b), Some((10, false)));
        assert_eq!(keyboard.lookup(0xdead), None);
    }

    #[test]
    fn chord_keysym_prefers_single_char_tokens() {
        let chord = keymap::parse_chord("ctrl+s").unwrap();
        assert_eq!(chord_keysym("ctrl+s", &chord), Some('s' as u32));
        let chord = keymap::parse_chord("shift+Insert").unwrap();
        assert_eq!(chord_keysym("shift+Insert", &chord), Some(0xff63));
        let chord = keymap::parse_chord("ctrl+Return").unwrap();
        assert_eq!(chord_keysym("ctrl+Return", &chord), Some(0xff0d));
    }
}
