use crate::{
    error::BackendError,
    platform::keymap::{self, Modifier},
};
use reis::ei;
use std::time::Duration;

pub struct EisSession {
    link: super::connection::Link,
    emulating: std::sync::Mutex<std::collections::HashSet<ei::Device>>,
    sequence: std::sync::atomic::AtomicU32,
    held_keys: std::sync::Mutex<std::collections::BTreeSet<u32>>,
    held_buttons: std::sync::Mutex<std::collections::BTreeSet<u32>>,
}

impl EisSession {
    /// Handshake over an already-connected EIS fd (portal RemoteDesktop reuses this).
    pub async fn open_from_fd(
        raw_fd: std::os::fd::RawFd,
        expect_keyboard: bool,
        expect_pointer: bool,
    ) -> Result<Self, BackendError> {
        Ok(Self {
            link: super::connection::connect(raw_fd, expect_keyboard, expect_pointer).await?,
            emulating: Default::default(),
            sequence: Default::default(),
            held_keys: Default::default(),
            held_buttons: Default::default(),
        })
    }

    fn key_event(&self, keyboard: &ei::Keyboard, key: u32, state: ei::keyboard::KeyState) {
        let mut held = self.held_keys.lock().expect("held keys");
        if state == ei::keyboard::KeyState::Press {
            held.insert(key);
        } else {
            held.remove(&key);
        }
        keyboard.key(key, state);
    }
    fn button_event(&self, buttons: &ei::Button, button: u32, state: ei::button::ButtonState) {
        let mut held = self.held_buttons.lock().expect("held buttons");
        if state == ei::button::ButtonState::Press {
            held.insert(button);
        } else {
            held.remove(&button);
        }
        buttons.button(button, state);
    }

    pub fn now_us() -> u64 {
        nix::time::clock_gettime(nix::time::ClockId::CLOCK_MONOTONIC)
            .map(|time| time.tv_sec() as u64 * 1_000_000 + time.tv_nsec() as u64 / 1_000)
            .unwrap_or_default()
    }

    pub fn flush(&self) -> Result<(), BackendError> {
        if !self.link.alive.load(std::sync::atomic::Ordering::Acquire) {
            return Err(BackendError::InputDispatchFailed {
                detail: "EIS device paused or disconnected; observe before retrying".into(),
            });
        }
        self.link
            .context
            .flush()
            .map_err(|e| BackendError::InputDispatchFailed {
                detail: format!("flush: {e}"),
            })
    }

    fn pointer_abs(&self) -> Result<ei::PointerAbsolute, BackendError> {
        self.link
            .device
            .interface::<ei::PointerAbsolute>()
            .ok_or_else(|| BackendError::InputDispatchFailed {
                detail: "device missing PointerAbsolute interface".into(),
            })
    }

    fn button_iface(&self) -> Result<ei::Button, BackendError> {
        self.link.device.interface::<ei::Button>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing Button interface".into(),
            }
        })
    }

    fn keyboard_iface(&self) -> Result<(ei::Device, ei::Keyboard), BackendError> {
        let device = self
            .link
            .keyboard_device
            .as_ref()
            .unwrap_or(&self.link.device);
        let inner = device.device().clone();
        let kb = device.interface::<ei::Keyboard>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing Keyboard interface".into(),
            }
        })?;
        Ok((inner, kb))
    }

    fn frame(&self, dev: &ei::Device) -> Result<(), BackendError> {
        dev.frame(self.link.connection.serial(), Self::now_us());
        self.flush()
    }

    pub async fn pointer_move(&self, x: i32, y: i32) -> Result<(), BackendError> {
        let dev = self.link.device.device().clone();
        let abs = self.pointer_abs()?;
        self.start(&dev);
        self.flush()?;
        abs.motion_absolute(x as f32, y as f32);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.stop(&dev);
        self.flush()
    }

    pub async fn click(&self, x: i32, y: i32, button: u32, count: u32) -> Result<(), BackendError> {
        let dev = self.link.device.device().clone();
        let abs = self.pointer_abs()?;
        let btn = self.button_iface()?;
        self.start(&dev);
        self.flush()?;
        abs.motion_absolute(x as f32, y as f32);
        self.frame(&dev)?;
        // The compositor must pick the surface under a newly moved pointer
        // before the button event; software-rendered sessions need a frame here.
        tokio::time::sleep(Duration::from_millis(100)).await;
        for _ in 0..count.max(1) {
            self.button_event(&btn, button, ei::button::ButtonState::Press);
            self.frame(&dev)?;
            tokio::time::sleep(Duration::from_millis(30)).await;
            self.button_event(&btn, button, ei::button::ButtonState::Released);
            self.frame(&dev)?;
            if count > 1 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        self.stop(&dev);
        self.flush()
    }

    pub async fn drag(
        &self,
        start: &(i32, i32),
        rest: &[(i32, i32)],
        button: u32,
        dwell_ms: u64,
        step_ms: u64,
    ) -> Result<(), BackendError> {
        let dev = self.link.device.device().clone();
        let abs = self.pointer_abs()?;
        let btn = self.button_iface()?;
        self.start(&dev);
        self.flush()?;
        abs.motion_absolute(start.0 as f32, start.1 as f32);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        self.button_event(&btn, button, ei::button::ButtonState::Press);
        self.frame(&dev)?;
        // Dwell with the button held so apps initiate drag gestures
        // (box-select/orbit/move) instead of seeing a fast press-release flick.
        tokio::time::sleep(Duration::from_millis(dwell_ms)).await;
        // Interpolate between consecutive points, 10 steps per segment.
        let mut prev = *start;
        for &(ex, ey) in rest {
            for i in 1..=10 {
                let fx = prev.0 as f32 + (ex - prev.0) as f32 * (i as f32 / 10.0);
                let fy = prev.1 as f32 + (ey - prev.1) as f32 * (i as f32 / 10.0);
                abs.motion_absolute(fx, fy);
                self.frame(&dev)?;
                tokio::time::sleep(Duration::from_millis(step_ms)).await;
            }
            prev = (ex, ey);
        }
        self.button_event(&btn, button, ei::button::ButtonState::Released);
        self.frame(&dev)?;
        self.stop(&dev);
        self.flush()
    }

    pub async fn scroll(&self, x: i32, y: i32, dx: f32, dy: f32) -> Result<(), BackendError> {
        let dev = self.link.device.device().clone();
        let abs = self.pointer_abs()?;
        let scroll = self.link.device.interface::<ei::Scroll>().ok_or_else(|| {
            BackendError::InputDispatchFailed {
                detail: "device missing Scroll interface".into(),
            }
        })?;
        self.start(&dev);
        self.flush()?;
        abs.motion_absolute(x as f32, y as f32);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(100)).await;
        scroll.scroll(dx, dy);
        self.frame(&dev)?;
        self.stop(&dev);
        self.flush()
    }

    pub async fn type_ascii(&self, text: &str) -> Result<(), BackendError> {
        let (dev, kb) = self.keyboard_iface()?;
        self.start(&dev);
        self.flush()?;
        for ch in text.chars() {
            let code = keymap::keycode_for_char(ch).ok_or_else(|| BackendError::Unsupported {
                reason: format!("no evdev mapping for {ch:?}"),
            })?;
            let shift = keymap::shift_required(ch);
            if shift {
                self.key_event(&kb, keymap::KEY_LEFTSHIFT, ei::keyboard::KeyState::Press);
                self.frame(&dev)?;
            }
            self.key_event(&kb, code, ei::keyboard::KeyState::Press);
            self.frame(&dev)?;
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.key_event(&kb, code, ei::keyboard::KeyState::Released);
            self.frame(&dev)?;
            if shift {
                self.key_event(&kb, keymap::KEY_LEFTSHIFT, ei::keyboard::KeyState::Released);
                self.frame(&dev)?;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        self.stop(&dev);
        self.flush()
    }

    pub async fn chord(&self, modifiers: &[Modifier], key: u32) -> Result<(), BackendError> {
        let (dev, kb) = self.keyboard_iface()?;
        self.start(&dev);
        self.flush()?;
        for m in modifiers {
            self.key_event(&kb, m.keycode(), ei::keyboard::KeyState::Press);
            self.frame(&dev)?;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.key_event(&kb, key, ei::keyboard::KeyState::Press);
        self.frame(&dev)?;
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.key_event(&kb, key, ei::keyboard::KeyState::Released);
        self.frame(&dev)?;
        for m in modifiers.iter().rev() {
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.key_event(&kb, m.keycode(), ei::keyboard::KeyState::Released);
            self.frame(&dev)?;
        }
        self.stop(&dev);
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
        let kdev =
            self.link
                .keyboard_device
                .clone()
                .ok_or_else(|| BackendError::InputDispatchFailed {
                    detail: "modifier combo requested but no keyboard device materialised".into(),
                })?;
        let kdev_inner = kdev.device().clone();
        let kb =
            kdev.interface::<ei::Keyboard>()
                .ok_or_else(|| BackendError::InputDispatchFailed {
                    detail: "keyboard aux device missing Keyboard interface".into(),
                })?;
        self.start(&kdev_inner);
        self.flush()?;
        for m in modifiers {
            self.key_event(&kb, m.keycode(), ei::keyboard::KeyState::Press);
            kdev_inner.frame(self.link.connection.serial(), Self::now_us());
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
            self.key_event(kb, m.keycode(), ei::keyboard::KeyState::Released);
            kdev_inner.frame(self.link.connection.serial(), Self::now_us());
            self.flush()?;
        }
        self.stop(kdev_inner);
        Ok(())
    }
}

impl EisSession {
    pub fn release_held(&self) {
        let keys = std::mem::take(&mut *self.held_keys.lock().expect("held keys"));
        let held_buttons = std::mem::take(&mut *self.held_buttons.lock().expect("held buttons"));
        for device in std::iter::once(&self.link.device).chain(self.link.keyboard_device.iter()) {
            if !self
                .emulating
                .lock()
                .expect("emulating devices")
                .contains(device.device())
            {
                continue;
            }
            if let Some(keyboard) = device.interface::<ei::Keyboard>() {
                for key in &keys {
                    keyboard.key(*key, ei::keyboard::KeyState::Released);
                }
            }
            if let Some(buttons) = device.interface::<ei::Button>() {
                for button in &held_buttons {
                    buttons.button(*button, ei::button::ButtonState::Released);
                }
            }
            device
                .device()
                .frame(self.link.connection.serial(), Self::now_us());
            self.stop(device.device());
        }
        let _ = self.link.context.flush();
    }
}

impl EisSession {
    pub fn is_connected(&self) -> bool {
        self.link.alive.load(std::sync::atomic::Ordering::Acquire)
    }
    fn start(&self, device: &ei::Device) {
        if !self
            .emulating
            .lock()
            .expect("emulating devices")
            .insert(device.clone())
        {
            return;
        }
        let sequence = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        device.start_emulating(sequence, self.link.connection.serial());
    }
    fn stop(&self, device: &ei::Device) {
        if !self.held_keys.lock().expect("held keys").is_empty()
            || !self.held_buttons.lock().expect("held buttons").is_empty()
        {
            return;
        }
        if self
            .emulating
            .lock()
            .expect("emulating devices")
            .remove(device)
        {
            device.stop_emulating(self.link.connection.serial());
        }
    }
}
impl Drop for EisSession {
    fn drop(&mut self) {
        self.release_held();
        self.link.stop.take();
    }
}
