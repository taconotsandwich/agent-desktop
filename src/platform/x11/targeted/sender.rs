//! One dispatch against one window: the server clock its events are stamped
//! with, the event frames, and the release of whatever is still held when a
//! dispatch fails midway.

use super::super::{X11, failed, windows::modal_sibling};
use crate::{
    error::{BackendError, fail},
    platform::keymap,
    types::ToolError,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection as _,
    errors::ReplyError,
    protocol::{
        ErrorKind, Event,
        xproto::{
            AtomEnum, BUTTON_PRESS_EVENT, BUTTON_RELEASE_EVENT, ButtonPressEvent,
            ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux, EventMask,
            FOCUS_IN_EVENT, FOCUS_OUT_EVENT, FocusInEvent, KEY_PRESS_EVENT, KEY_RELEASE_EVENT,
            KeyButMask, KeyPressEvent, MOTION_NOTIFY_EVENT, Motion, MotionNotifyEvent,
            NotifyDetail, NotifyMode, PropMode, Timestamp, Window, WindowClass,
        },
    },
    wrapper::ConnectionExt as _,
};

/// Server clock sample. Clients read `time` for double-click detection and
/// `_NET_WM_USER_TIME`, so `CURRENT_TIME` is not an option. The core protocol
/// has no request for the time, but a PropertyNotify on a private window
/// carries it: sample once, then extrapolate with the monotonic clock.
#[derive(Debug, Clone, Copy)]
pub(super) struct Clock {
    server: Timestamp,
    sampled: Instant,
}

impl Clock {
    pub(super) async fn sample(x11: &X11) -> Result<Self, BackendError> {
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

/// `BadWindow`, or `BadDrawable` from a geometry request, means the
/// destination is gone; anything else is a backend failure.
fn classify(window: Window, error: ReplyError) -> BackendError {
    match error {
        ReplyError::X11Error(error)
            if matches!(error.error_kind, ErrorKind::Window | ErrorKind::Drawable) =>
        {
            BackendError::StaleHandle(format!("window 0x{window:x} no longer exists"))
        }
        error => failed(error),
    }
}

/// A release that finds the window gone is complete: the press it ends was
/// delivered, and the client closed the window in response, the way a
/// dialog closes on Escape.
fn gone_is_released(error: BackendError) -> Result<(), BackendError> {
    match error {
        BackendError::StaleHandle(_) => Ok(()),
        error => Err(error),
    }
}

/// Tool error for a press, motion or window lookup that did not go out.
pub(super) fn undelivered(error: BackendError) -> ToolError {
    match error {
        BackendError::StaleHandle(detail) => fail("stale_window", detail),
        error => error.tool(true),
    }
}

/// What the client is told once the dispatch is over: that the window lost
/// focus, or that the client's own window which really holds it has it back.
enum Handback {
    FocusOut,
    FocusIn(Window),
}

fn focus_event(response_type: u8, window: Window) -> FocusInEvent {
    FocusInEvent {
        response_type,
        detail: NotifyDetail::NONLINEAR,
        sequence: 0,
        event: window,
        mode: NotifyMode::NORMAL,
    }
}

/// Whether `focus` is `window` or one of its descendants. `NONE` and
/// `POINTER_ROOT` are not windows.
fn focused(x11: &X11, window: Window, mut focus: Window) -> Result<bool, BackendError> {
    for _ in 0..64 {
        if focus == window {
            return Ok(true);
        }
        if focus <= 1 || focus == x11.root {
            return Ok(false);
        }
        focus = match x11.connection.query_tree(focus).map_err(failed)?.reply() {
            Ok(tree) => tree.parent,
            Err(error) => match classify(focus, error) {
                BackendError::StaleHandle(_) => return Ok(false),
                error => return Err(error),
            },
        };
    }
    Ok(false)
}

/// One dispatch against one window. Whatever is still pressed when the
/// dispatch fails midway is released on drop.
pub(super) struct Sender {
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
    pub(super) held_keys: Vec<u8>,
    held_buttons: Vec<u8>,
    /// Set once the client was told its window has focus.
    handback: Option<Handback>,
}

impl Sender {
    pub(super) fn new(
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
            .map_err(|error| classify(window, error))?;
        let origin = x11
            .connection
            .translate_coordinates(window, x11.root, 0, 0)
            .map_err(failed)?
            .reply()
            .map_err(|error| classify(window, error))?;
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
            handback: None,
        })
    }

    pub(super) fn require_inside(&self, x: i32, y: i32) -> Result<(), BackendError> {
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

    /// Tells the client its window has focus for the dispatch. Qt delivers
    /// keys to the window it believes focused, fires a window's action
    /// shortcuts only there, and asks the window manager to activate a
    /// clicked window it believes unfocused, which would move the real
    /// focus. A window that a modal window of the same client blocks is
    /// refused instead: the client would relay the focus to the modal
    /// window by activating it for real.
    pub(super) async fn lend_focus(&mut self) -> Result<(), ToolError> {
        let focus = self
            .x11
            .connection
            .get_input_focus()
            .map_err(failed)
            .and_then(|cookie| cookie.reply().map_err(failed))
            .map_err(|error| error.tool(true))?
            .focus;
        if focused(&self.x11, self.window, focus).map_err(|error| error.tool(true))? {
            return Ok(());
        }
        if let Some(title) =
            modal_sibling(&self.x11, self.window).map_err(|error| error.tool(true))?
        {
            return Err(fail(
                "blocked_window",
                format!(
                    "The application's modal window {title:?} blocks input to this window \
                     until it is closed"
                ),
            ));
        }
        self.send(focus_event(FOCUS_IN_EVENT, self.window))
            .map_err(undelivered)?;
        let mask = self.x11.connection.setup().resource_id_mask;
        self.handback = Some(if focus > 1 && focus & !mask == self.window & !mask {
            Handback::FocusIn(focus)
        } else {
            Handback::FocusOut
        });
        // The client applies the focus change when it has drained its event
        // queue; a click read in the same batch would still find the old
        // focus window.
        tokio::time::sleep(Duration::from_millis(50)).await;
        Ok(())
    }

    fn hand_back(&mut self) {
        let Some(handback) = self.handback.take() else {
            return;
        };
        let _ = self.send(focus_event(FOCUS_OUT_EVENT, self.window));
        if let Handback::FocusIn(window) = handback {
            let _ = self.send_to(window, focus_event(FOCUS_IN_EVENT, window));
        }
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
        self.send_to(self.window, event)
    }

    fn send_to(&self, window: Window, event: impl Into<[u8; 32]>) -> Result<(), BackendError> {
        self.x11
            .connection
            .send_event(false, window, EventMask::NO_EVENT, event)
            .map_err(failed)?
            .check()
            .map_err(|error| classify(window, error))?;
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

    pub(super) fn motion(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
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

    pub(super) fn press_button(&mut self, button: u8) -> Result<(), BackendError> {
        let event = self.button_event(BUTTON_PRESS_EVENT, button)?;
        self.held_buttons.push(button);
        self.send(event)
    }

    pub(super) fn release_button(&mut self, button: u8) -> Result<(), BackendError> {
        // `state` describes the buttons held before the event, so the
        // released button is still part of it.
        let event = self.button_event(BUTTON_RELEASE_EVENT, button)?;
        self.held_buttons.retain(|held| *held != button);
        self.send(event).or_else(gone_is_released)
    }

    pub(super) fn press_key(&mut self, keycode: u8) -> Result<(), BackendError> {
        let event = self.key_event(KEY_PRESS_EVENT, keycode)?;
        self.held_keys.push(keycode);
        self.send(event)
    }

    pub(super) fn release_key(&mut self, keycode: u8) -> Result<(), BackendError> {
        let event = self.key_event(KEY_RELEASE_EVENT, keycode)?;
        self.held_keys.retain(|held| *held != keycode);
        self.send(event).or_else(gone_is_released)
    }

    pub(super) async fn click(&mut self, button: u8) -> Result<(), ToolError> {
        self.press_button(button).map_err(undelivered)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        self.release_button(button)
            .map_err(|error| error.tool(true))
    }

    /// Presses the keys in order and releases them in reverse, the way the
    /// XTEST driver plays a stroke: modifiers first, then the key. Each key
    /// comes with the modifier state once it is down, which its own release
    /// and the next press report, since `state` describes the keyboard
    /// before an event.
    pub(super) async fn stroke(
        &mut self,
        keys: &[(u8, u16)],
        hold: Duration,
    ) -> Result<(), ToolError> {
        let base = self.modifiers;
        for &(keycode, down) in keys {
            self.press_key(keycode).map_err(undelivered)?;
            self.modifiers = down;
        }
        tokio::time::sleep(hold).await;
        for &(keycode, down) in keys.iter().rev() {
            self.modifiers = down;
            self.release_key(keycode)
                .map_err(|error| error.tool(true))?;
        }
        self.modifiers = base;
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
        self.hand_back();
    }
}
