//! Window-targeted X11 input over `SendEvent`.
//!
//! XTEST feeds the server's input pipeline, so its events reach whatever
//! window holds focus and move the real pointer. `SendEvent` with an empty
//! event mask hands a synthetic core event to the client that created the
//! destination window instead: the window manager never sees it, focus stays
//! where it is and the pointer does not move. This is the mechanism behind
//! `xdotool --window`. Modifier keys are never pressed; every event carries
//! its modifiers in the `state` field, which toolkits such as Qt use to
//! rebuild a keyboard state for synthetic events.

use super::{
    X11, connection, failed,
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
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection as _,
    protocol::{
        Event,
        xproto::{
            AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent,
            ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
            KEY_PRESS_EVENT, KEY_RELEASE_EVENT, KeyButMask, KeyPressEvent, MOTION_NOTIFY_EVENT,
            Motion, MotionNotifyEvent, PropMode, Timestamp, Window, WindowClass,
        },
    },
    wrapper::ConnectionExt as _,
};

#[derive(Default)]
pub struct X11Targeted {
    clock: OnceLock<Clock>,
}

/// Server clock sample. Clients read `time` for double-click detection and
/// `_NET_WM_USER_TIME`, so `CURRENT_TIME` is not an option. The core protocol
/// has no request for the time, but a PropertyNotify on a private window
/// carries it: sample once, then extrapolate with the monotonic clock.
#[derive(Debug, Clone, Copy)]
struct Clock {
    server: Timestamp,
    sampled: Instant,
}

impl Clock {
    async fn sample(x11: &X11) -> Result<Self, BackendError> {
        let window = x11.connection.generate_id().map_err(failed)?;
        x11.connection
            .create_window(
                0,
                window,
                x11.root,
                0,
                0,
                1,
                1,
                0,
                WindowClass::INPUT_ONLY,
                x11rb::COPY_FROM_PARENT,
                &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
            )
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        x11.connection
            .change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_NAME,
                AtomEnum::STRING,
                b"clock",
            )
            .map_err(failed)?;
        x11.connection.flush().map_err(failed)?;
        let deadline = Instant::now() + Duration::from_secs(3);
        let server = loop {
            match x11.connection.poll_for_event().map_err(failed)? {
                Some(Event::PropertyNotify(event)) if event.window == window => break event.time,
                Some(_) => continue,
                None if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                None => return Err(failed("server clock sample timed out")),
            }
        };
        let sampled = Instant::now();
        // Destroying the window deletes its properties, and the server
        // reports each deletion as another PropertyNotify; stop listening
        // first so the stray event never reaches the shared event queue.
        x11.connection
            .change_window_attributes(
                window,
                &ChangeWindowAttributesAux::new().event_mask(EventMask::NO_EVENT),
            )
            .map_err(failed)?;
        x11.connection
            .destroy_window(window)
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        Ok(Self { server, sampled })
    }

    fn now(&self) -> Timestamp {
        self.server
            .wrapping_add(self.sampled.elapsed().as_millis() as u32)
    }
}

/// Event fields shared by pointer and key events.
struct Frame {
    time: Timestamp,
    root_x: i16,
    root_y: i16,
    event_x: i16,
    event_y: i16,
    state: KeyButMask,
}

/// One dispatch against one window. Whatever is still pressed when the
/// dispatch fails midway is released on drop.
struct Sender {
    x11: Arc<X11>,
    window: Window,
    /// Client-area origin in root coordinates, and its size.
    origin: (i32, i32),
    size: (u32, u32),
    clock: Clock,
    /// Modifier bits carried by the events in flight.
    modifiers: u16,
    /// Pointer position last reported to the window, in root coordinates.
    at: (i32, i32),
    held_keys: Vec<u8>,
    held_buttons: Vec<u8>,
}

impl Sender {
    fn new(
        x11: &Arc<X11>,
        clock: Clock,
        window: Window,
        modifiers: u16,
    ) -> Result<Self, BackendError> {
        let geometry = x11
            .connection
            .get_geometry(window)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let origin = x11
            .connection
            .translate_coordinates(window, x11.root, 0, 0)
            .map_err(failed)?
            .reply()
            .map_err(failed)?;
        let origin = (origin.dst_x as i32, origin.dst_y as i32);
        Ok(Self {
            x11: x11.clone(),
            window,
            origin,
            size: (geometry.width as u32, geometry.height as u32),
            clock,
            modifiers,
            at: origin,
            held_keys: Vec::new(),
            held_buttons: Vec::new(),
        })
    }

    fn require_inside(&self, x: i32, y: i32) -> Result<(), BackendError> {
        let (left, top) = self.origin;
        let (width, height) = self.size;
        if (left..left + width as i32).contains(&x) && (top..top + height as i32).contains(&y) {
            return Ok(());
        }
        Err(keymap::unsupported(format!(
            "({x}, {y}) is outside the window's client area, {width}x{height} at ({left}, {top}); \
             decorations belong to the window manager and cannot receive targeted input"
        )))
    }

    fn state(&self) -> KeyButMask {
        let buttons = self
            .held_buttons
            .iter()
            .filter(|&&button| (1..=5).contains(&button))
            .fold(0u16, |mask, &button| mask | 1 << (7 + u16::from(button)));
        KeyButMask::from(self.modifiers | buttons)
    }

    fn frame(&self) -> Result<Frame, BackendError> {
        let fit = |value: i32| {
            i16::try_from(value).map_err(|_| {
                keymap::unsupported(format!("coordinate {value} does not fit an X11 event"))
            })
        };
        Ok(Frame {
            time: self.clock.now(),
            root_x: fit(self.at.0)?,
            root_y: fit(self.at.1)?,
            event_x: fit(self.at.0 - self.origin.0)?,
            event_y: fit(self.at.1 - self.origin.1)?,
            state: self.state(),
        })
    }

    fn send(&self, event: impl Into<[u8; 32]>) -> Result<(), BackendError> {
        self.x11
            .connection
            .send_event(false, self.window, EventMask::NO_EVENT, event)
            .map_err(failed)?
            .check()
            .map_err(failed)?;
        self.x11.connection.flush().map_err(failed)
    }

    fn button_event(
        &self,
        response_type: u8,
        button: u8,
    ) -> Result<ButtonPressEvent, BackendError> {
        let frame = self.frame()?;
        Ok(ButtonPressEvent {
            response_type,
            detail: button,
            sequence: 0,
            time: frame.time,
            root: self.x11.root,
            event: self.window,
            child: x11rb::NONE,
            root_x: frame.root_x,
            root_y: frame.root_y,
            event_x: frame.event_x,
            event_y: frame.event_y,
            state: frame.state,
            same_screen: true,
        })
    }

    fn key_event(&self, response_type: u8, keycode: u8) -> Result<KeyPressEvent, BackendError> {
        let frame = self.frame()?;
        Ok(KeyPressEvent {
            response_type,
            detail: keycode,
            sequence: 0,
            time: frame.time,
            root: self.x11.root,
            event: self.window,
            child: x11rb::NONE,
            root_x: frame.root_x,
            root_y: frame.root_y,
            event_x: frame.event_x,
            event_y: frame.event_y,
            state: frame.state,
            same_screen: true,
        })
    }

    fn motion(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        self.at = (x, y);
        let frame = self.frame()?;
        self.send(MotionNotifyEvent {
            response_type: MOTION_NOTIFY_EVENT,
            detail: Motion::NORMAL,
            sequence: 0,
            time: frame.time,
            root: self.x11.root,
            event: self.window,
            child: x11rb::NONE,
            root_x: frame.root_x,
            root_y: frame.root_y,
            event_x: frame.event_x,
            event_y: frame.event_y,
            state: frame.state,
            same_screen: true,
        })
    }

    fn press_button(&mut self, button: u8) -> Result<(), BackendError> {
        let event = self.button_event(BUTTON_PRESS_EVENT, button)?;
        self.held_buttons.push(button);
        self.send(event)
    }

    fn release_button(&mut self, button: u8) -> Result<(), BackendError> {
        // `state` describes the buttons held before the event, so the
        // released button is still part of it.
        let event = self.button_event(BUTTON_RELEASE_EVENT, button)?;
        self.send(event)?;
        self.held_buttons.retain(|held| *held != button);
        Ok(())
    }

    fn press_key(&mut self, keycode: u8) -> Result<(), BackendError> {
        let event = self.key_event(KEY_PRESS_EVENT, keycode)?;
        self.held_keys.push(keycode);
        self.send(event)
    }

    fn release_key(&mut self, keycode: u8) -> Result<(), BackendError> {
        let event = self.key_event(KEY_RELEASE_EVENT, keycode)?;
        self.send(event)?;
        self.held_keys.retain(|held| *held != keycode);
        Ok(())
    }

    async fn click(&mut self, button: u8) -> Result<(), ToolError> {
        self.press_button(button)
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.release_button(button)
            .map_err(|error| error.tool(true))
    }

    async fn stroke(&mut self, keycode: u8, state: u16, hold: Duration) -> Result<(), ToolError> {
        self.modifiers = state;
        self.press_key(keycode).map_err(|error| error.tool(true))?;
        tokio::time::sleep(hold).await;
        self.release_key(keycode)
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        Ok(())
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        for keycode in self.held_keys.clone().into_iter().rev() {
            let _ = self
                .key_event(KEY_RELEASE_EVENT, keycode)
                .and_then(|event| self.send(event));
        }
        for button in self.held_buttons.clone().into_iter().rev() {
            let _ = self
                .button_event(BUTTON_RELEASE_EVENT, button)
                .and_then(|event| self.send(event));
        }
    }
}

/// Keycode and core `state` for each stroke, resolved before anything is
/// sent: a symbol the layout cannot produce fails with nothing typed, so the
/// caller's paste fallback does not duplicate text. `Keyboard` wraps raw XKB
/// pointers and must not live across an await.
fn plan(
    x11: &X11,
    strokes: impl FnOnce(&Keyboard) -> Result<Vec<Stroke>, BackendError>,
) -> Result<Vec<(u8, u16)>, BackendError> {
    let keyboard = super::keyboard::read(x11)?.ignoring_held();
    strokes(&keyboard)?
        .iter()
        .map(|stroke| {
            let keycode = u8::try_from(stroke.key)
                .map_err(|_| keymap::unsupported("keycode does not fit a core event"))?;
            Ok((keycode, keyboard.mask(&stroke.modifiers)))
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
        Sender::new(x11, clock, window, modifiers).map_err(|error| error.tool(false))
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
        sender.motion(x, y).map_err(|error| error.tool(true))?;
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
        let button = button_code(button);
        sender
            .motion(start.0, start.1)
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        sender
            .press_button(button)
            .map_err(|error| error.tool(true))?;
        tokio::time::sleep(Duration::from_millis(dwell_ms)).await;
        let mut previous = *start;
        for &(end_x, end_y) in rest {
            for step in 1..=10 {
                let x = previous.0 + (end_x - previous.0) * step / 10;
                let y = previous.1 + (end_y - previous.1) * step / 10;
                sender.motion(x, y).map_err(|error| error.tool(true))?;
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
        sender.motion(x, y).map_err(|error| error.tool(true))?;
        // Buttons 4/5/6/7 = up/down/left/right, one click per 120-unit notch.
        for (delta, negative, positive) in [(dx, 6u8, 7u8), (dy, 4u8, 5u8)] {
            if delta == 0 {
                continue;
            }
            let button = if delta < 0 { negative } else { positive };
            for _ in 0..delta.unsigned_abs().div_ceil(120).max(1) {
                sender
                    .press_button(button)
                    .map_err(|error| error.tool(true))?;
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
        for (keycode, state) in strokes {
            sender
                .stroke(keycode, state, Duration::from_millis(5))
                .await?;
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
        for (keycode, state) in strokes {
            sender.stroke(keycode, state, hold).await?;
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
