use super::*;
use crate::platform::{drivers::Bbox, keymap::parse_chord, x11::testing};

fn drain(x11: &X11) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(event) = x11.connection.poll_for_event().unwrap() {
        events.push(event);
    }
    events
}

/// A mapped top-level window of the test connection.
fn top_level(x11: &X11, x: i16, y: i16, width: u16, height: u16) -> Window {
    let window = x11.connection.generate_id().unwrap();
    x11.connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            x11.root,
            x,
            y,
            width,
            height,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap()
        .check()
        .unwrap();
    x11.connection.map_window(window).unwrap().check().unwrap();
    window
}

fn info(window_ref: &str, client_protocol: Option<&str>) -> WindowInfo {
    WindowInfo {
        window_ref: Ref(window_ref.into()),
        title: "Untitled".into(),
        class: "kwrite".into(),
        app_id: String::new(),
        pid: Some(4242),
        minimized: false,
        client_protocol: client_protocol.map(str::to_owned),
        geometry: Bbox {
            x: 10,
            y: 20,
            w: 300,
            h: 200,
        },
        screen: 0,
        is_active: false,
    }
}

#[tokio::test]
async fn only_x11_window_refs_resolve_without_a_display() {
    let driver = X11Targeted::default();
    assert_eq!(
        driver
            .resolve(&info("x11:0x00400003", Some("x11")))
            .await
            .unwrap(),
        Some(Ref("x11:0x00400003".into()))
    );
    assert_eq!(
        driver
            .resolve(&info("kwin:12", Some("wayland")))
            .await
            .unwrap(),
        None
    );
    let mut anonymous = info("kwin:12", Some("x11"));
    anonymous.pid = None;
    assert_eq!(driver.resolve(&anonymous).await.unwrap(), None);
}

#[tokio::test]
#[ignore = "requires Xvfb; creates and removes its own X11 server"]
async fn compositor_windows_map_to_xwayland_clients_by_process_then_title() {
    let (_server, _display, x11, first) = testing::server();
    let atom = |name: &[u8]| {
        x11.connection
            .intern_atom(false, name)
            .unwrap()
            .reply()
            .unwrap()
            .atom
    };
    let (client_list, wm_pid, wm_name, utf8) = (
        atom(b"_NET_CLIENT_LIST"),
        atom(b"_NET_WM_PID"),
        atom(b"_NET_WM_NAME"),
        atom(b"UTF8_STRING"),
    );
    let second = x11.connection.generate_id().unwrap();
    x11.connection
        .create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            second,
            x11.root,
            0,
            0,
            10,
            10,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new(),
        )
        .unwrap()
        .check()
        .unwrap();
    for (window, title) in [(first, "Untitled"), (second, "Second")] {
        x11.connection
            .change_property32(
                PropMode::REPLACE,
                window,
                wm_pid,
                AtomEnum::CARDINAL,
                &[4242],
            )
            .unwrap();
        x11.connection
            .change_property8(PropMode::REPLACE, window, wm_name, utf8, title.as_bytes())
            .unwrap();
    }
    let clients = |windows: &[Window]| {
        x11.connection
            .change_property32(
                PropMode::REPLACE,
                x11.root,
                client_list,
                AtomEnum::WINDOW,
                windows,
            )
            .unwrap()
            .check()
            .unwrap();
    };
    clients(&[first, second]);
    assert_eq!(client_of(&x11, 4242, "Second").unwrap(), Some(second));
    assert_eq!(
        client_of(&x11, 4242, "Third").unwrap(),
        None,
        "two windows and neither carries the title"
    );
    assert_eq!(client_of(&x11, 4243, "Untitled").unwrap(), None);
    clients(&[first]);
    assert_eq!(
        client_of(&x11, 4242, "Untitled <2>").unwrap(),
        Some(first),
        "the only window of the process"
    );
}

#[tokio::test]
#[ignore = "requires Xvfb and setxkbmap; creates and removes its own X11 server"]
async fn synthetic_events_reach_the_window_without_moving_focus_or_pointer() {
    let (_server, display, x11, focused) = testing::server();
    testing::layout(&display, "us", "");
    let target = top_level(&x11, 100, 50, 300, 200);
    let target_ref = Ref(format!("x11:0x{target:08x}"));
    let driver = X11Targeted::default();
    let pointer = x11
        .connection
        .query_pointer(x11.root)
        .unwrap()
        .reply()
        .unwrap();
    let pointer_before = (pointer.root_x, pointer.root_y);
    let focus = || {
        x11.connection
            .get_input_focus()
            .unwrap()
            .reply()
            .unwrap()
            .focus
    };
    assert_eq!(focus(), focused);

    // Double click at (150, 75): window-relative (50, 25), timestamps real
    // and inside the double-click interval, button bit only on release.
    driver
        .click_in(&x11, &target_ref, 150, 75, Button::Left, 2)
        .await
        .unwrap();
    let events = drain(&x11);
    assert_eq!(events.len(), 5, "{events:?}");
    let Event::MotionNotify(motion) = &events[0] else {
        panic!("{events:?}");
    };
    assert_eq!(
        (
            motion.event,
            motion.event_x,
            motion.event_y,
            motion.root_x,
            motion.root_y
        ),
        (target, 50, 25, 150, 75)
    );
    assert_ne!(motion.time, x11rb::CURRENT_TIME);
    let mut presses = Vec::new();
    for event in &events[1..] {
        match event {
            Event::ButtonPress(press) => {
                assert_eq!(press.detail, 1);
                assert_ne!(press.response_type & 0x80, 0, "send_event flag");
                assert!(!press.state.contains(KeyButMask::BUTTON1));
                assert_eq!((press.event_x, press.event_y), (50, 25));
                presses.push(press.time);
            }
            Event::ButtonRelease(release) => {
                assert_eq!(release.detail, 1);
                assert!(release.state.contains(KeyButMask::BUTTON1));
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(presses.len(), 2);
    assert!(presses[1] > presses[0] && presses[1] - presses[0] < 400);

    // Ctrl+Z presses Control, then Z, and releases them in reverse; the
    // state field carries Control from the Z press to the Control release.
    driver
        .key_in(
            &x11,
            &target_ref,
            &[parse_chord("Ctrl+Z").unwrap()],
            Duration::from_millis(1),
        )
        .await
        .unwrap();
    let (control, z) = {
        let stroke = super::super::keyboard::read(&x11)
            .unwrap()
            .chord(&parse_chord("Ctrl+Z").unwrap())
            .unwrap();
        (stroke.modifiers[0], stroke.key)
    };
    let keys: Vec<(bool, u32, bool)> = drain(&x11)
        .iter()
        .map(|event| match event {
            Event::KeyPress(press) => (
                true,
                u32::from(press.detail),
                press.state.contains(KeyButMask::CONTROL),
            ),
            Event::KeyRelease(release) => (
                false,
                u32::from(release.detail),
                release.state.contains(KeyButMask::CONTROL),
            ),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            (true, control, false),
            (true, z, true),
            (false, z, true),
            (false, control, true)
        ]
    );

    // Literal text: '?' needs Shift, 'a' does not. Shift goes down for '?'
    // only, and its own press still reports the state before it.
    driver.type_in(&x11, &target_ref, "a?").await.unwrap();
    let shifted: Vec<bool> = drain(&x11)
        .into_iter()
        .filter_map(|event| match event {
            Event::KeyPress(press) => Some(press.state.contains(KeyButMask::SHIFT)),
            _ => None,
        })
        .collect();
    assert_eq!(shifted, vec![false, false, true], "a, Shift, ?");

    // A character the layout cannot produce fails before anything is sent.
    let error = driver.type_in(&x11, &target_ref, "aé").await.unwrap_err();
    assert_eq!(error.code, "unsupported");
    assert!(drain(&x11).is_empty());

    // Two notches down are two button-5 clicks.
    driver
        .scroll_in(&x11, &target_ref, 150, 75, 0, 240)
        .await
        .unwrap();
    let buttons: Vec<u8> = drain(&x11)
        .iter()
        .filter_map(|event| match event {
            Event::ButtonPress(press) => Some(press.detail),
            _ => None,
        })
        .collect();
    assert_eq!(buttons, vec![5, 5]);

    // Points outside the client area are rejected before anything is sent.
    let error = driver
        .click_in(&x11, &target_ref, 10, 10, Button::Left, 1)
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported");
    assert!(drain(&x11).is_empty());

    // A cancelled drag releases the button.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(150),
            driver.drag_in(
                &x11,
                &target_ref,
                vec![(150, 75), (200, 100)],
                Button::Left,
                1000,
                15,
            ),
        )
        .await
        .is_err()
    );
    let events = drain(&x11);
    assert!(
        matches!(events.last(), Some(Event::ButtonRelease(release)) if release.detail == 1),
        "{events:?}"
    );

    let pointer = x11
        .connection
        .query_pointer(x11.root)
        .unwrap()
        .reply()
        .unwrap();
    assert_eq!((pointer.root_x, pointer.root_y), pointer_before);
    assert_eq!(focus(), focused);
}

#[tokio::test]
#[ignore = "requires Xvfb; creates and removes its own X11 server"]
async fn a_window_closed_by_a_press_still_takes_its_release() {
    let (_server, _display, x11, _focused) = testing::server();
    let target = top_level(&x11, 100, 50, 300, 200);
    let clock = Clock::sample(&x11).await.unwrap();
    let mut sender = Sender::new(&x11, clock, target, 0).unwrap();
    sender.press_key(38).unwrap();
    x11.connection
        .destroy_window(target)
        .unwrap()
        .check()
        .unwrap();
    sender.release_key(38).unwrap();
    assert!(sender.held_keys.is_empty());
    let error = undelivered(sender.press_key(38).unwrap_err());
    assert_eq!(error.code, "stale_window", "{}", error.message);
    drop(sender);

    // The driver notices before it sends anything.
    let error = X11Targeted::default()
        .key_in(
            &x11,
            &Ref(format!("x11:0x{target:08x}")),
            &[parse_chord("a").unwrap()],
            Duration::from_millis(1),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "stale_window", "{}", error.message);
}
