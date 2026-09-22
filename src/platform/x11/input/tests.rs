use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::mpsc,
};
use x11rb::{
    protocol::{
        Event,
        xproto::{
            ConnectionExt as _, CreateWindowAux, EventMask, InputFocus, ModMask, WindowClass,
        },
    },
    xcb_ffi::XCBConnection,
};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn server() -> (Server, String, Arc<X11>) {
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
    )
}
fn layout(display: &str, layout: &str, options: &str) {
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
async fn chord(x11: &Arc<X11>, text: &str) -> Vec<x11rb::protocol::xproto::KeyPressEvent> {
    let stroke = super::super::keyboard::read(x11)
        .unwrap()
        .chord(&keymap::parse_chord(text).unwrap())
        .unwrap();
    let mut held = HeldKeys::new(x11);
    dispatch(&mut held, stroke, Duration::from_millis(1))
        .await
        .unwrap();
    assert!(
        x11.connection
            .query_keymap()
            .unwrap()
            .reply()
            .unwrap()
            .keys
            .iter()
            .all(|&key| key == 0)
    );
    let mut events = Vec::new();
    while let Some(event) = x11.connection.poll_for_event().unwrap() {
        if let Event::KeyPress(event) = event {
            events.push(event);
        }
    }
    events
}

#[tokio::test]
#[ignore = "requires Xvfb and setxkbmap; creates and removes its own X11 server"]
async fn native_layout_changes_and_cancelled_chords_release_keys() {
    let (_server, display, x11) = server();
    layout(&display, "us", "");
    let undo = chord(&x11, "Ctrl+Z").await;
    let redo = chord(&x11, "Ctrl+Shift+Z").await;
    let undo = undo.last().unwrap();
    let redo = redo.last().unwrap();
    assert_eq!(undo.detail, redo.detail);
    assert!(undo.state.contains(ModMask::CONTROL));
    assert!(!undo.state.contains(ModMask::SHIFT));
    assert!(redo.state.contains(ModMask::SHIFT));
    let question = chord(&x11, "Ctrl+?").await;
    assert!(question.last().unwrap().state.contains(ModMask::SHIFT));
    let us_y = chord(&x11, "Ctrl+Y").await.last().unwrap().detail;
    layout(&display, "de", "ctrl:swapcaps");
    let de_y = chord(&x11, "Ctrl+Y").await;
    assert_ne!(us_y, de_y.last().unwrap().detail);
    assert_eq!(de_y[0].detail, 66);
    let at = chord(&x11, "@").await;
    assert!(at.last().unwrap().state.contains(ModMask::M5));
    {
        let stroke = super::super::keyboard::read(&x11)
            .unwrap()
            .chord(&keymap::parse_chord("Ctrl+Shift+Z").unwrap())
            .unwrap();
        let mut held = HeldKeys::new(&x11);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                dispatch(&mut held, stroke, Duration::from_secs(1))
            )
            .await
            .is_err()
        );
    }
    assert!(
        x11.connection
            .query_keymap()
            .unwrap()
            .reply()
            .unwrap()
            .keys
            .iter()
            .all(|&key| key == 0),
        "cancelled input left keys held"
    );
    chord(&x11, "F12").await;
}
