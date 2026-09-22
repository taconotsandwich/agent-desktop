use super::{X11, failed};
use crate::{
    error::BackendError,
    platform::keyboard::{Keyboard, NativeState},
};
use xkbcommon::xkb;

pub(super) fn read(x11: &X11) -> Result<Keyboard, BackendError> {
    let (mut major, mut minor, mut event, mut error) = (0, 0, 0, 0);
    if !xkb::x11::setup_xkb_extension(
        &x11.connection,
        1,
        0,
        xkb::x11::SetupXkbExtensionFlags::NoFlags,
        &mut major,
        &mut minor,
        &mut event,
        &mut error,
    ) {
        return Err(failed("XKB extension unavailable"));
    }
    let device = xkb::x11::get_core_keyboard_device_id(&x11.connection);
    if device < 0 {
        return Err(failed("XKB core keyboard unavailable"));
    }
    let context = xkb::Context::new(xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
    // SAFETY: the context and XCB connection remain alive for both calls.
    // Check null results before handing ownership to the Rust wrappers.
    let keymap = unsafe {
        let ptr = xkb::x11::ffi::xkb_x11_keymap_new_from_device(
            context.get_raw_ptr(),
            x11.connection.get_raw_xcb_connection().cast(),
            device,
            xkb::COMPILE_NO_FLAGS,
        );
        if ptr.is_null() {
            return Err(failed("cannot read the XKB keymap"));
        }
        xkb::Keymap::from_raw_ptr(ptr)
    };
    let state = unsafe {
        let ptr = xkb::x11::ffi::xkb_x11_state_new_from_device(
            keymap.get_raw_ptr(),
            x11.connection.get_raw_xcb_connection().cast(),
            device,
        );
        if ptr.is_null() {
            return Err(failed("cannot read the XKB keyboard state"));
        }
        xkb::State::from_raw_ptr(ptr)
    };
    Ok(Keyboard::new(keymap, NativeState::capture(&state)))
}
