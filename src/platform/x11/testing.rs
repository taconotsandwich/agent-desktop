//! Private Xvfb server for X11 driver tests.
use super::X11;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, mpsc},
    time::Duration,
};
use x11rb::{
    connection::Connection as _,
    protocol::xproto::{
        ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, Window, WindowClass,
    },
    xcb_ffi::XCBConnection,
};

pub(super) struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start an Xvfb server and connect to it. The returned window is mapped at
/// the origin, selects key events and holds the input focus.
pub(super) fn server() -> (Server, String, Arc<X11>, Window) {
    let mut server = Server(
        Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "800x600x24",
                "-nolisten",
                "tcp",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("Xvfb is required"),
    );
    let output = server.0.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut display = String::new();
        let result = BufReader::new(output)
            .read_line(&mut display)
            .map(|_| format!(":{}", display.trim()));
        let _ = tx.send(result);
    });
    let display = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    let name = std::ffi::CString::new(display.clone()).unwrap();
    let (connection, screen) = XCBConnection::connect(Some(&name)).unwrap();
    let root = connection.setup().roots[screen].root;
    let window = connection.generate_id().unwrap();
    connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            400,
            300,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new().event_mask(EventMask::KEY_PRESS | EventMask::KEY_RELEASE),
        )
        .unwrap()
        .check()
        .unwrap();
    connection.map_window(window).unwrap().check().unwrap();
    connection
        .set_input_focus(InputFocus::PARENT, window, x11rb::CURRENT_TIME)
        .unwrap()
        .check()
        .unwrap();
    (
        server,
        display,
        Arc::new(X11 {
            connection,
            root,
            root_width: 800,
            root_height: 600,
        }),
        window,
    )
}

pub(super) fn layout(display: &str, layout: &str, options: &str) {
    assert!(
        Command::new("setxkbmap")
            .args([
                "-display", display, "-layout", layout, "-option", "", "-option", options
            ])
            .status()
            .unwrap()
            .success()
    );
}
