use super::super::testing::{layout, server};
use super::*;
use x11rb::protocol::{
    Event,
    xproto::{ConnectionExt as _, ModMask},
};

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
    let (_server, display, x11, _window) = server();
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
