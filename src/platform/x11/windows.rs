//! X11 window control over EWMH client messages — the same protocol messages
//! wmctrl sends; no wmctrl/xprop subprocess and no text parsing.
//!
//! Focus is `_NET_ACTIVE_WINDOW` with a pager source indication, exactly the
//! activation wmctrl performs: the window manager keeps its focus-stealing
//! policy in the loop.

use super::{X11, connection, failed};
use crate::{
    error::{BackendError, fail},
    platform::drivers::{Bbox, Probe, Ref, WindowDriver, WindowInfo},
    types::ToolError,
};
use std::sync::OnceLock;
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Window},
};

/// EWMH root messages must reach the window manager.
fn root_event_mask() -> EventMask {
    EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY
}

struct Atoms {
    net_supported: Atom,
    net_client_list: Atom,
    net_active_window: Atom,
    net_wm_name: Atom,
    wm_name: Atom,
    wm_class: Atom,
    gtk_application_id: Atom,
    net_wm_pid: Atom,
    net_wm_state: Atom,
    net_wm_state_hidden: Atom,
    net_wm_window_type: Atom,
    net_wm_window_type_normal: Atom,
    net_wm_window_type_dialog: Atom,
    utf8_string: Atom,
    net_wm_state_maximized_vert: Atom,
    net_wm_state_maximized_horz: Atom,
    net_close_window: Atom,
    net_moveresize_window: Atom,
    wm_change_state: Atom,
}

static ATOMS: OnceLock<Atoms> = OnceLock::new();

fn atoms(x11: &X11) -> Result<&'static Atoms, BackendError> {
    if let Some(atoms) = ATOMS.get() {
        return Ok(atoms);
    }
    let intern = |name: &str| -> Result<Atom, BackendError> {
        Ok(x11
            .connection
            .intern_atom(false, name.as_bytes())
            .map_err(failed)?
            .reply()
            .map_err(failed)?
            .atom)
    };
    let atoms = Atoms {
        net_supported: intern("_NET_SUPPORTED")?,
        net_client_list: intern("_NET_CLIENT_LIST")?,
        net_active_window: intern("_NET_ACTIVE_WINDOW")?,
        net_wm_name: intern("_NET_WM_NAME")?,
        wm_name: intern("WM_NAME")?,
        wm_class: intern("WM_CLASS")?,
        gtk_application_id: intern("_GTK_APPLICATION_ID")?,
        net_wm_pid: intern("_NET_WM_PID")?,
        net_wm_state: intern("_NET_WM_STATE")?,
        net_wm_state_hidden: intern("_NET_WM_STATE_HIDDEN")?,
        net_wm_window_type: intern("_NET_WM_WINDOW_TYPE")?,
        net_wm_window_type_normal: intern("_NET_WM_WINDOW_TYPE_NORMAL")?,
        net_wm_window_type_dialog: intern("_NET_WM_WINDOW_TYPE_DIALOG")?,
        utf8_string: intern("UTF8_STRING")?,
        net_wm_state_maximized_vert: intern("_NET_WM_STATE_MAXIMIZED_VERT")?,
        net_wm_state_maximized_horz: intern("_NET_WM_STATE_MAXIMIZED_HORZ")?,
        net_close_window: intern("_NET_CLOSE_WINDOW")?,
        net_moveresize_window: intern("_NET_MOVERESIZE_WINDOW")?,
        wm_change_state: intern("WM_CHANGE_STATE")?,
    };
    let _ = ATOMS.set(atoms);
    Ok(ATOMS.get().expect("atoms are initialized"))
}

pub(super) fn window_of(window_ref: &Ref) -> Result<Window, ToolError> {
    let text = window_ref
        .0
        .strip_prefix("x11:")
        .ok_or_else(|| fail("stale_window", "Invalid X11 window identity"))?;
    u32::from_str_radix(text.trim_start_matches("0x"), 16)
        .map_err(|_| fail("stale_window", "Invalid X11 window identity"))
}

fn atom_property(x11: &X11, window: Window, property: Atom) -> Result<Vec<Atom>, BackendError> {
    let reply = x11
        .connection
        .get_property(false, window, property, AtomEnum::ATOM, 0, 1024)
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(reply
        .value32()
        .map(|values| values.collect())
        .unwrap_or_default())
}

fn string_property(
    x11: &X11,
    window: Window,
    property: Atom,
    type_: Atom,
) -> Result<Option<String>, BackendError> {
    let reply = x11
        .connection
        .get_property(false, window, property, type_, 0, 4096)
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(reply
        .value8()
        .and_then(|bytes| String::from_utf8(bytes.collect()).ok())
        .filter(|text| !text.is_empty()))
}

fn cardinal_property(
    x11: &X11,
    window: Window,
    property: Atom,
) -> Result<Option<u32>, BackendError> {
    let reply = x11
        .connection
        .get_property(false, window, property, AtomEnum::CARDINAL, 0, 1)
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(reply.value32().and_then(|mut values| values.next()))
}

fn window_list(x11: &X11, atoms: &Atoms) -> Result<Vec<Window>, BackendError> {
    let reply = x11
        .connection
        .get_property(
            false,
            x11.root,
            atoms.net_client_list,
            AtomEnum::WINDOW,
            0,
            4096,
        )
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(reply
        .value32()
        .map(|values| values.collect())
        .unwrap_or_default())
}

fn active_window(x11: &X11, atoms: &Atoms) -> Result<Option<Window>, BackendError> {
    let reply = x11
        .connection
        .get_property(
            false,
            x11.root,
            atoms.net_active_window,
            AtomEnum::WINDOW,
            0,
            1,
        )
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(reply
        .value32()
        .and_then(|mut values| values.next())
        .filter(|window| *window != 0))
}

fn window_info(
    x11: &X11,
    atoms: &Atoms,
    window: Window,
    active: Option<Window>,
) -> Result<Option<WindowInfo>, BackendError> {
    let types = atom_property(x11, window, atoms.net_wm_window_type)?;
    if !types.is_empty()
        && !types.contains(&atoms.net_wm_window_type_normal)
        && !types.contains(&atoms.net_wm_window_type_dialog)
    {
        return Ok(None);
    }
    let title = string_property(x11, window, atoms.net_wm_name, atoms.utf8_string)?
        .or(string_property(
            x11,
            window,
            atoms.wm_name,
            AtomEnum::STRING.into(),
        )?)
        .unwrap_or_default();
    let class = string_property(x11, window, atoms.wm_class, AtomEnum::STRING.into())?
        .and_then(|raw| raw.split('\0').nth(1).map(str::to_owned))
        .unwrap_or_default();
    let app_id = string_property(x11, window, atoms.gtk_application_id, atoms.utf8_string)?
        .unwrap_or_default();
    let state = atom_property(x11, window, atoms.net_wm_state)?;
    let geometry = x11
        .connection
        .get_geometry(window)
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    let translated = x11
        .connection
        .translate_coordinates(window, x11.root, 0, 0)
        .map_err(failed)?
        .reply()
        .map_err(failed)?;
    Ok(Some(WindowInfo {
        window_ref: Ref(format!("x11:0x{window:08x}")),
        title,
        class,
        app_id,
        pid: cardinal_property(x11, window, atoms.net_wm_pid)?.filter(|pid| *pid > 0),
        minimized: state.contains(&atoms.net_wm_state_hidden),
        client_protocol: Some("x11".into()),
        geometry: Bbox {
            x: translated.dst_x as i32,
            y: translated.dst_y as i32,
            w: geometry.width as u32,
            h: geometry.height as u32,
        },
        screen: 0,
        is_active: active == Some(window),
    }))
}

pub struct X11Windows;

impl X11Windows {
    /// Verify the window is still live, then send one root client message.
    /// A vanished window must fail like the old wmctrl invocation did.
    fn control(&self, id: &Ref, message: Atom, data: [u32; 5]) -> Result<(), ToolError> {
        let window = window_of(id)?;
        let x11 = connection().map_err(|error| error.tool(false))?;
        x11.connection
            .get_window_attributes(window)
            .map_err(|error| failed(error).tool(false))?
            .reply()
            .map_err(|error| failed(error).tool(false))?;
        let event = ClientMessageEvent::new(32, window, message, data);
        x11.connection
            .send_event(false, x11.root, root_event_mask(), event)
            .map_err(|error| failed(error).tool(false))?;
        x11.connection
            .flush()
            .map_err(|error| failed(error).tool(false))
    }
}

#[async_trait::async_trait]
impl WindowDriver for X11Windows {
    fn id(&self) -> &'static str {
        "x11-ewmh"
    }

    async fn probe(&self) -> Probe {
        let result = async {
            let x11 = connection()?;
            let atoms = atoms(&x11)?;
            let reply = x11
                .connection
                .get_property(
                    false,
                    x11.root,
                    atoms.net_supported,
                    AtomEnum::ATOM,
                    0,
                    4096,
                )
                .map_err(failed)?
                .reply()
                .map_err(failed)?;
            let supported: Vec<Atom> = reply
                .value32()
                .map(|values| values.collect())
                .unwrap_or_default();
            if supported.contains(&atoms.net_active_window)
                && supported.contains(&atoms.net_client_list)
            {
                Ok::<(), BackendError>(())
            } else {
                Err(BackendError::Unavailable {
                    backend: "x11-ewmh",
                    detail: "window manager does not advertise EWMH support".into(),
                })
            }
        }
        .await;
        Probe {
            id: self.id(),
            ok: result.is_ok(),
            detail: result
                .map(|()| "EWMH window manager reachable".into())
                .unwrap_or_else(|error| error.to_string()),
        }
    }

    async fn query(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(true))?;
        let clients = window_list(&x11, atoms).map_err(|error| error.tool(true))?;
        let active = active_window(&x11, atoms).map_err(|error| error.tool(true))?;
        let mut windows = Vec::new();
        for window in clients {
            // EWMH enumeration is inherently racy: windows destroyed between
            // the client list fetch and the per-window reads are skipped.
            match window_info(&x11, atoms, window, active) {
                Ok(Some(info)) => windows.push(info),
                Ok(None) | Err(_) => continue,
            }
        }
        Ok(windows)
    }

    async fn focus(&self, id: &Ref) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(false))?;
        // Source indication 2 (pager) matches wmctrl -a activation.
        self.control(id, atoms.net_active_window, [2, 0, 0, 0, 0])
    }

    async fn minimize(&self, id: &Ref) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(false))?;
        // ICCCM iconify via WM_CHANGE_STATE (what XIconifyWindow sends).
        self.control(id, atoms.wm_change_state, [3, 0, 0, 0, 0])
    }

    async fn maximize(&self, id: &Ref) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(false))?;
        self.control(
            id,
            atoms.net_wm_state,
            [
                1,
                atoms.net_wm_state_maximized_vert,
                atoms.net_wm_state_maximized_horz,
                2,
                0,
            ],
        )
    }

    async fn restore(&self, id: &Ref) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(false))?;
        self.control(
            id,
            atoms.net_wm_state,
            [
                0,
                atoms.net_wm_state_maximized_vert,
                atoms.net_wm_state_maximized_horz,
                2,
                0,
            ],
        )?;
        self.focus(id).await
    }

    async fn close(&self, id: &Ref) -> Result<(), ToolError> {
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(false))?;
        self.control(id, atoms.net_close_window, [0, 2, 0, 0, 0])
    }

    async fn move_resize(&self, id: &Ref, geo: Bbox) -> Result<(), ToolError> {
        if geo.w == 0 || geo.h == 0 {
            return Err(fail("invalid_geometry", "Window size must be positive"));
        }
        let x11 = connection().map_err(|error| error.tool(false))?;
        let atoms = atoms(&x11).map_err(|error| error.tool(false))?;
        // data.l[0] = gravity (0) | source indication (2) << 8, per EWMH.
        self.control(
            id,
            atoms.net_moveresize_window,
            [2 << 8, geo.x as u32, geo.y as u32, geo.w, geo.h],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_refs_parse_the_wmctrl_compatible_form() {
        assert_eq!(
            window_of(&Ref("x11:0x03e0000a".into())).unwrap(),
            0x03e0_000a
        );
        assert_eq!(
            window_of(&Ref(format!("x11:0x{:08x}", 0x0120_0003))).unwrap(),
            0x0120_0003
        );
        assert!(window_of(&Ref("kwin:abc".into())).is_err());
        assert!(window_of(&Ref("x11:nothex".into())).is_err());
    }
}
