//! KWin EIS input → [`InputDriver`]. (Handshake ported from kde-mcp
//! `kwin/eis.rs`; dispatch mirrors `tools/mouse.rs` + `tools/keyboard.rs`.)
//!
//! !Send isolation: reis's event stream holds a `!Send` callback, so the
//! handshake runs on a dedicated current-thread runtime inside
//! `spawn_blocking`. Only Send pieces (Context, Device, serial) cross back.

use crate::drivers::{Button, InputDriver, Probe};
use crate::error::BackendError;
use crate::keymap::{self, Modifier};
use crate::types::ToolError;
use futures_util::StreamExt;
use reis::{ei, enumflags2, event::DeviceCapability};
use std::os::unix::io::{FromRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use zbus::Connection;

pub const CAP_KEYBOARD: i32 = 1;
pub const CAP_POINTER: i32 = 2;

#[zbus::proxy(
    interface = "org.kde.KWin.EIS.RemoteDesktop",
    default_service = "org.kde.KWin",
    default_path = "/org/kde/KWin/EIS/RemoteDesktop"
)]
trait RemoteDesktop {
    #[zbus(name = "connectToEIS")]
    fn connect_to_eis(&self, flags: i32) -> zbus::Result<(zvariant::OwnedFd, i32)>;
}

pub struct KwinInput {
    bus: Connection,
}

impl KwinInput {
    pub fn new(bus: Connection) -> Self {
        Self { bus }
    }
}

#[async_trait::async_trait]
impl InputDriver for KwinInput {
    fn id(&self) -> &'static str {
        "kwin-eis"
    }

    async fn probe(&self) -> Probe {
        super::kwin_probe(&self.bus, self.id()).await
    }

    async fn click(
        &self,
        x: i32,
        y: i32,
        button: Button,
        hold: Vec<Modifier>,
    ) -> Result<(), ToolError> {
        if hold.is_empty() {
            let s = EisSession::open(&self.bus, CAP_POINTER, false, true)
                .await
                .map_err(|e| e.tool(true))?;
            s.click(x, y, button_code(button), 1)
                .await
                .map_err(|e| e.tool(true))
        } else {
            let s = EisSession::open(&self.bus, CAP_KEYBOARD | CAP_POINTER, true, true)
                .await
                .map_err(|e| e.tool(true))?;
            s.click_with_modifiers(x, y, button_code(button), &hold)
                .await
                .map_err(|e| e.tool(true))
        }
    }

    async fn move_to(&self, x: i32, y: i32) -> Result<(), ToolError> {
        let s = EisSession::open(&self.bus, CAP_POINTER, false, true)
            .await
            .map_err(|e| e.tool(true))?;
s.pointer_move(x, y).await.map_err(|e| e.tool(true))
    }

    async fn drag(&self, path: Vec<(i32, i32)>, button: Button) -> Result<(), ToolError> {
        let (start, rest) = path.split_first().ok_or_else(|| {
            BackendError::Unsupported {
                reason: "drag needs ≥1 point".into(),
            }
            .tool(false)
        })?;
        let s = EisSession::open(&self.bus, CAP_POINTER, false, true)
            .await
            .map_err(|e| e.tool(true))?;
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
        if hold.is_empty() {
            let s = EisSession::open(&self.bus, CAP_POINTER, false, true)
                .await
                .map_err(|e| e.tool(true))?;
            s.scroll(x, y, dx as f32 * 15.0, dy as f32 * 15.0)
                .await
                .map_err(|e| e.tool(true))
        } else {
            let s = EisSession::open(&self.bus, CAP_KEYBOARD | CAP_POINTER, true, true)
                .await
                .map_err(|e| e.tool(true))?;
            s.scroll_with_modifiers(x, y, dx as f32 * 15.0, dy as f32 * 15.0, &hold)
                .await
                .map_err(|e| e.tool(true))
        }
    }

    async fn type_text(&self, text: String) -> Result<(), ToolError> {
        if let Some(ch) = text.chars().find(|c| keymap::keycode_for_char(*c).is_none()) {
            return Err(BackendError::Unsupported {
                reason: format!("no evdev mapping for {ch:?}; use clipboard paste path"),
            }
            .tool(false));
        }
        let s = EisSession::open(&self.bus, CAP_KEYBOARD, true, false)
            .await
            .map_err(|e| e.tool(true))?;
s.type_ascii(&text).await.map_err(|e| e.tool(true))
    }

    async fn key(&self, keys: Vec<String>) -> Result<(), ToolError> {
        if keys.is_empty() {
            return Err(BackendError::Unsupported {
                reason: "empty chord".into(),
            }
            .tool(false));
        }
        // Multi-chord sequences ("ctrl+s Enter") run left to right.
        let s = EisSession::open(&self.bus, CAP_KEYBOARD, true, false)
            .await
            .map_err(|e| e.tool(true))?;
        for chord_str in &keys {
            let chord = keymap::parse_chord(chord_str).map_err(|e| e.tool(false))?;
            s.chord(&chord.modifiers, chord.key)
                .await
                .map_err(|e| e.tool(true))?;
        }
        Ok(())
    }
}

fn button_code(b: Button) -> u32 {
    match b {
        Button::Left => keymap::BTN_LEFT,
        Button::Right => keymap::BTN_RIGHT,
        Button::Middle => keymap::BTN_MIDDLE,
    }
}

pub struct EisSession {
    pub context: ei::Context,
    pub device: reis::event::Device,
    pub keyboard_device: Option<reis::event::Device>,
    pub last_serial: u32,
}

impl EisSession {
    pub async fn open(
        bus: &Connection,
        caps_flags: i32,
        expect_keyboard: bool,
        expect_pointer: bool,
    ) -> Result<Self, BackendError> {
        let proxy = RemoteDesktopProxy::new(bus)
            .await
            .map_err(|e| BackendError::BusDisconnected {
                detail: e.to_string(),
            })?;
        let (owned_fd, _conn_id) = proxy
            .connect_to_eis(caps_flags)
            .await
            .map_err(|e| BackendError::BusDisconnected {
                detail: e.to_string(),
            })?;
        let std_fd: std::os::fd::OwnedFd = owned_fd.into();
        let raw_fd = std_fd.into_raw_fd();
        Self::open_from_fd(raw_fd, expect_keyboard, expect_pointer).await
    }

    /// Handshake over an already-connected EIS fd (portal RemoteDesktop reuses this).
    pub async fn open_from_fd(
        raw_fd: std::os::fd::RawFd,
        expect_keyboard: bool,
        expect_pointer: bool,
    ) -> Result<Self, BackendError> {
        let join = tokio::task::spawn_blocking(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| BackendError::Io {
                    path: "tokio current_thread rt".into(),
                    error: e.to_string(),
                })?;
            rt.block_on(handshake_and_wait_for_device(
                raw_fd,
                expect_keyboard,
                expect_pointer,
            ))
        })
        .await
        .map_err(|e| BackendError::InputDispatchFailed {
            detail: format!("spawn_blocking join: {e}"),
        })?;
        let (context, device, keyboard_device, last_serial) = join?;
        Ok(EisSession {
            context,
            device,
            keyboard_device,
            last_serial,
        })
    }

    pub fn now_us() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64
    }

    pub fn flush(&self) -> Result<(), BackendError> {
        self.context.flush().map_err(|e| BackendError::InputDispatchFailed {
            detail: format!("flush: {e}"),
        })
    }

    fn pointer_abs(&self) -> Result<ei::PointerAbsolute, BackendError> {
        self.device.interface::<ei::PointerAbsolute>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing PointerAbsolute interface".into(),
            }
        })
    }

    fn button_iface(&self) -> Result<ei::Button, BackendError> {
        self.device.interface::<ei::Button>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing Button interface".into(),
            }
        })
    }

    fn keyboard_iface(&self) -> Result<(ei::Device, ei::Keyboard), BackendError> {
        let inner = self.device.device().clone();
        let kb = self.device.interface::<ei::Keyboard>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing Keyboard interface".into(),
            }
        })?;
        Ok((inner, kb))
    }

    fn frame(&self, dev: &ei::Device) -> Result<(), BackendError> {
        dev.frame(self.last_serial, Self::now_us());
        self.flush()
    }

    pub async fn pointer_move(&self, x: i32, y: i32) -> Result<(), BackendError> {
        let dev = self.device.device().clone();
        let abs = self.pointer_abs()?;
        dev.start_emulating(self.last_serial, 0);
        self.flush()?;
        abs.motion_absolute(x as f32, y as f32);
        self.frame(&dev)?;
        dev.stop_emulating(self.last_serial);
        self.flush()
    }

    pub async fn click(&self, x: i32, y: i32, button: u32, count: u32) -> Result<(), BackendError> {
        let dev = self.device.device().clone();
        let abs = self.pointer_abs()?;
        let btn = self.button_iface()?;
        dev.start_emulating(self.last_serial, 0);
        self.flush()?;
        abs.motion_absolute(x as f32, y as f32);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        for _ in 0..count.max(1) {
            btn.button(button, ei::button::ButtonState::Press);
            self.frame(&dev)?;
            tokio::time::sleep(Duration::from_millis(30)).await;
            btn.button(button, ei::button::ButtonState::Released);
            self.frame(&dev)?;
            if count > 1 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        dev.stop_emulating(self.last_serial);
        self.flush()
    }

    pub async fn drag(
        &self,
        start: &(i32, i32),
        rest: &[(i32, i32)],
        button: u32,
    ) -> Result<(), BackendError> {
        let dev = self.device.device().clone();
        let abs = self.pointer_abs()?;
        let btn = self.button_iface()?;
        dev.start_emulating(self.last_serial, 0);
        self.flush()?;
        abs.motion_absolute(start.0 as f32, start.1 as f32);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        btn.button(button, ei::button::ButtonState::Press);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(30)).await;
        // Interpolate between consecutive points, 10 steps per segment.
        let mut prev = *start;
        for &(ex, ey) in rest {
            for i in 1..=10 {
                let fx = prev.0 as f32 + (ex - prev.0) as f32 * (i as f32 / 10.0);
                let fy = prev.1 as f32 + (ey - prev.1) as f32 * (i as f32 / 10.0);
                abs.motion_absolute(fx, fy);
                self.frame(&dev)?;
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            prev = (ex, ey);
        }
        btn.button(button, ei::button::ButtonState::Released);
        self.frame(&dev)?;
        dev.stop_emulating(self.last_serial);
        self.flush()
    }

    pub async fn scroll(&self, x: i32, y: i32, dx: f32, dy: f32) -> Result<(), BackendError> {
        let dev = self.device.device().clone();
        let abs = self.pointer_abs()?;
        let scroll = self.device.interface::<ei::Scroll>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing Scroll interface".into(),
            }
        })?;
        dev.start_emulating(self.last_serial, 0);
        self.flush()?;
        abs.motion_absolute(x as f32, y as f32);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        scroll.scroll(dx, dy);
        self.frame(&dev)?;
        dev.stop_emulating(self.last_serial);
        self.flush()
    }

    pub async fn type_ascii(&self, text: &str) -> Result<(), BackendError> {
        let (dev, kb) = self.keyboard_iface()?;
        dev.start_emulating(self.last_serial, 0);
        self.flush()?;
        for ch in text.chars() {
            let code = keymap::keycode_for_char(ch).ok_or_else(|| BackendError::Unsupported {
                reason: format!("no evdev mapping for {ch:?}"),
            })?;
            let shift = keymap::shift_required(ch);
            if shift {
                kb.key(keymap::KEY_LEFTSHIFT, ei::keyboard::KeyState::Press);
                self.frame(&dev)?;
            }
            kb.key(code, ei::keyboard::KeyState::Press);
            self.frame(&dev)?;
            tokio::time::sleep(Duration::from_millis(5)).await;
            kb.key(code, ei::keyboard::KeyState::Released);
            self.frame(&dev)?;
            if shift {
                kb.key(keymap::KEY_LEFTSHIFT, ei::keyboard::KeyState::Released);
                self.frame(&dev)?;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        dev.stop_emulating(self.last_serial);
        self.flush()
    }

    pub async fn chord(&self, modifiers: &[Modifier], key: u32) -> Result<(), BackendError> {
        let (dev, kb) = self.keyboard_iface()?;
        dev.start_emulating(self.last_serial, 0);
        self.flush()?;
        for m in modifiers {
            kb.key(m.keycode(), ei::keyboard::KeyState::Press);
            self.frame(&dev)?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        kb.key(key, ei::keyboard::KeyState::Press);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        kb.key(key, ei::keyboard::KeyState::Released);
        self.frame(&dev)?;
        for m in modifiers.iter().rev() {
            tokio::time::sleep(Duration::from_millis(10)).await;
            kb.key(m.keycode(), ei::keyboard::KeyState::Released);
            self.frame(&dev)?;
        }
        dev.stop_emulating(self.last_serial);
        self.flush()
    }

    /// Modifier-wrapped click (Ctrl+click etc.): press modifiers on the
    /// keyboard aux device around a pointer click.
    pub async fn click_with_modifiers(
        &self,
        x: i32,
        y: i32,
        button: u32,
        modifiers: &[Modifier],
    ) -> Result<(), BackendError> {
        let (kdev_inner, kb) = self.modifier_hold(modifiers).await?;
        let outcome = self.click(x, y, button, 1).await;
        self.modifier_release(modifiers, &kdev_inner, &kb).await?;
        outcome
    }

    /// Modifier-wrapped scroll (Ctrl+scroll zoom etc.): same hold pattern.
    pub async fn scroll_with_modifiers(
        &self,
        x: i32,
        y: i32,
        dx: f32,
        dy: f32,
        modifiers: &[Modifier],
    ) -> Result<(), BackendError> {
        let (kdev_inner, kb) = self.modifier_hold(modifiers).await?;
        let outcome = self.scroll(x, y, dx, dy).await;
        self.modifier_release(modifiers, &kdev_inner, &kb).await?;
        outcome
    }

    async fn modifier_hold(
        &self,
        modifiers: &[Modifier],
    ) -> Result<(ei::Device, ei::Keyboard), BackendError> {
        let kdev = self.keyboard_device.clone().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "modifier combo requested but no keyboard device materialised".into(),
            }
        })?;
        let kdev_inner = kdev.device().clone();
        let kb = kdev.interface::<ei::Keyboard>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "keyboard aux device missing Keyboard interface".into(),
            }
        })?;
        kdev_inner.start_emulating(self.last_serial, 0);
        self.flush()?;
        for m in modifiers {
            kb.key(m.keycode(), ei::keyboard::KeyState::Press);
            kdev_inner.frame(self.last_serial, Self::now_us());
            self.flush()?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Ok((kdev_inner, kb))
    }

    async fn modifier_release(
        &self,
        modifiers: &[Modifier],
        kdev_inner: &ei::Device,
        kb: &ei::Keyboard,
    ) -> Result<(), BackendError> {
        for m in modifiers.iter().rev() {
            tokio::time::sleep(Duration::from_millis(10)).await;
            kb.key(m.keycode(), ei::keyboard::KeyState::Released);
            kdev_inner.frame(self.last_serial, Self::now_us());
            self.flush()?;
        }
        kdev_inner.stop_emulating(self.last_serial);
        Ok(())
    }
}

async fn handshake_and_wait_for_device(
    raw_fd: std::os::fd::RawFd,
    expect_keyboard: bool,
    expect_pointer: bool,
) -> Result<
    (
        ei::Context,
        reis::event::Device,
        Option<reis::event::Device>,
        u32,
    ),
    BackendError,
> {
    let stream = unsafe { UnixStream::from_raw_fd(raw_fd) };
    stream.set_nonblocking(true).map_err(|e| BackendError::Io {
        path: "eis stream".into(),
        error: e.to_string(),
    })?;
    let context = ei::Context::new(stream).map_err(|e| BackendError::Io {
        path: "ei::Context".into(),
        error: e.to_string(),
    })?;
    let (_conn, mut events) = context
        .handshake_tokio("agent-desktop", ei::handshake::ContextType::Sender)
        .await
        .map_err(|e| BackendError::InputDispatchFailed {
            detail: format!("handshake: {e}"),
        })?;

    let mut chosen_dev: Option<reis::event::Device> = None;
    let mut keyboard_aux: Option<reis::event::Device> = None;
    let mut last_serial: u32 = 0;
    let combined = expect_keyboard && expect_pointer;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let next = tokio::time::timeout(remaining, events.next()).await;
        let Ok(Some(result)) = next else {
            break;
        };
        let event = result.map_err(|e| BackendError::InputDispatchFailed {
            detail: format!("event stream: {e}"),
        })?;
        match &event {
            reis::event::EiEvent::SeatAdded(evt) => {
                let mut bind: enumflags2::BitFlags<DeviceCapability> =
                    DeviceCapability::Scroll.into();
                if expect_keyboard {
                    bind |= DeviceCapability::Keyboard;
                }
                if expect_pointer {
                    bind |= DeviceCapability::Pointer
                        | DeviceCapability::PointerAbsolute
                        | DeviceCapability::Button;
                }
                evt.seat.bind_capabilities(bind);
                context.flush().map_err(|e| BackendError::InputDispatchFailed {
                    detail: format!("flush after bind: {e}"),
                })?;
            }
            reis::event::EiEvent::DeviceAdded(evt) => {
                let has_kb = evt.device.has_capability(DeviceCapability::Keyboard);
                let has_ptr = evt.device.has_capability(DeviceCapability::PointerAbsolute)
                    && evt.device.has_capability(DeviceCapability::Button);
                if combined {
                    if has_ptr && chosen_dev.is_none() {
                        chosen_dev = Some(evt.device.clone());
                    } else if has_kb && keyboard_aux.is_none() {
                        keyboard_aux = Some(evt.device.clone());
                    }
                } else if (expect_keyboard && has_kb) || (expect_pointer && has_ptr) {
                    chosen_dev = Some(evt.device.clone());
                }
            }
            reis::event::EiEvent::DeviceResumed(evt) => {
                last_serial = evt.serial;
                let ready = if combined {
                    chosen_dev.is_some() && keyboard_aux.is_some()
                } else {
                    chosen_dev.is_some()
                };
                if ready {
                    break;
                }
            }
            _ => {}
        }
    }
    let device = chosen_dev.ok_or_else(|| BackendError::InputDispatchFailed {
        detail: "no matching device materialised within 3s".into(),
    })?;
    Ok((context, device, keyboard_aux, last_serial))
}
