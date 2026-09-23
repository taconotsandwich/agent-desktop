use super::*;
use crate::{
    test_support::{FakeTargeted, FakeWindows},
    types::{Ref, Trust, TrustedText},
};

/// Engine over the fake window driver, with the fake window observed and a
/// screenshot frame on file so coordinate actions map to desktop pixels.
async fn observed(registry: Registry, session: SessionType) -> Engine {
    let engine = Engine::new(registry, session, Arc::new(AtspiConnection::new()));
    let window = engine
        .registry
        .windows()
        .unwrap()
        .query()
        .await
        .unwrap()
        .remove(0);
    let element = FlatElement {
        id: Ref("element".into()),
        bus_guid: "bus".into(),
        parent_id: None,
        child_count: 0,
        role: TrustedText {
            trust: Trust::Trusted,
            text: "button".into(),
        },
        name: TrustedText {
            trust: Trust::External,
            text: "Apply".into(),
        },
        value: None,
        states: vec!["showing".into()],
        bbox: Some([0, 0, 20, 20]),
        actions: vec!["Press".into()],
    };
    {
        let mut state = engine.state.lock().await;
        state.apps.insert(
            "fake.desktop".into(),
            Application {
                id: "fake.desktop".into(),
                name: "Fake".into(),
                desktop_file: "/test/fake.desktop".into(),
                startup_class: "fake".into(),
                executable: "fake".into(),
            },
        );
        state.observations.insert(
            "fake.desktop".into(),
            Observation {
                window: Some(window.window_ref.0.clone()),
                observed_at: Some(Instant::now()),
                observed_geometry: Some(window.geometry.clone()),
                elements: HashMap::from([(7, element)]),
                frame: Some(Frame {
                    geometry: window.geometry,
                    image_width: 800,
                    image_height: 600,
                }),
                ..Observation::default()
            },
        );
    }
    engine
}

#[tokio::test]
async fn observations_authorize_only_the_current_window_and_fresh_indices() {
    let registry = Registry::probe(vec![], vec![], vec![Arc::new(FakeWindows)], vec![]).await;
    let engine = observed(registry, SessionType::X11).await;
    assert!(engine.element("fake.desktop", 7).await.is_ok());
    assert!(engine.element("fake.desktop", 8).await.is_err());
    engine.invalidate("fake.desktop").await;
    assert_eq!(
        engine.element("fake.desktop", 7).await.unwrap_err().code,
        "stale_element"
    );
    engine
        .state
        .lock()
        .await
        .observations
        .get_mut("fake.desktop")
        .unwrap()
        .window = Some("another-window".into());
    assert_eq!(
        engine
            .point("fake.desktop", &PointerTarget::Coordinates([10.0, 10.0]))
            .await
            .unwrap_err()
            .code,
        "stale_screenshot"
    );
}

#[tokio::test]
async fn capabilities_report_where_coordinate_input_goes() {
    let plain = Registry::probe(vec![], vec![], vec![Arc::new(FakeWindows)], vec![]).await;
    let engine = Engine::new(plain, SessionType::X11, Arc::new(AtspiConnection::new()));
    let targeting = engine.targeting();
    assert_eq!(targeting.coordinate_input, "foreground");
    assert_eq!(targeting.targeted_input, None);
    assert_eq!(targeting.targeted_clients, None);
    for (session, coordinate_input, clients) in [
        (SessionType::X11, "window", "all"),
        (SessionType::Wayland, "foreground", "xwayland"),
    ] {
        let registry = Registry::probe(
            vec![],
            vec![],
            vec![Arc::new(FakeWindows)],
            vec![Arc::new(FakeTargeted::default())],
        )
        .await;
        let engine = Engine::new(registry, session, Arc::new(AtspiConnection::new()));
        let targeting = engine.targeting();
        assert_eq!(targeting.coordinate_input, coordinate_input);
        assert_eq!(targeting.targeted_input, Some("fake-targeted"));
        assert_eq!(targeting.targeted_clients, Some(clients));
    }
}

#[tokio::test]
async fn targeted_windows_take_input_without_the_seat() {
    let request = |method: &str, args: serde_json::Value| -> Request {
        serde_json::from_value(json!({"target":"fake.desktop","method":method,"args":args}))
            .unwrap()
    };
    // No input driver: the foreground path cannot even activate the window.
    let plain = Registry::probe(vec![], vec![], vec![Arc::new(FakeWindows)], vec![]).await;
    let engine = observed(plain, SessionType::X11).await;
    let error = engine
        .dispatch(request("click", json!({"target":[400.0,300.0]})))
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsupported");
    assert!(error.message.contains("input"), "{}", error.message);

    let targeted = Arc::new(FakeTargeted::default());
    let registry = Registry::probe(
        vec![],
        vec![],
        vec![Arc::new(FakeWindows)],
        vec![targeted.clone()],
    )
    .await;
    let engine = observed(registry, SessionType::X11).await;
    for (method, args) in [
        (
            "click",
            json!({"target":[400.0,300.0],"clickCount":2,"mouseButton":"right"}),
        ),
        ("click", json!({"target":7,"mouseButton":"right"})),
        ("drag", json!({"from":[0.0,0.0],"to":[400.0,300.0]})),
        (
            "scroll",
            json!({"target":[400.0,300.0],"direction":"down","pages":0.5}),
        ),
        ("pressKey", json!({"key":"ctrl+z"})),
        ("typeText", json!({"text":"hello"})),
    ] {
        engine.dispatch(request(method, args)).await.unwrap();
        // Every action invalidates the observation; keep the element usable.
        observed_again(&engine).await;
    }
    assert_eq!(
        *targeted.calls.lock().unwrap(),
        [
            "click fake:1 (400,300) Right x2",
            "click fake:1 (10,10) Right x1",
            "drag fake:1 Some((0, 0))..Some((400, 300)) Left",
            "scroll fake:1 (400,300) by (0,300)",
            "key fake:1 1",
            "type fake:1 \"hello\"",
        ]
    );
}

async fn observed_again(engine: &Engine) {
    let mut state = engine.state.lock().await;
    let observation = state.observations.get_mut("fake.desktop").unwrap();
    observation.observed_at = Some(Instant::now());
    if observation.elements.is_empty() {
        observation.elements.insert(
            7,
            FlatElement {
                id: Ref("element".into()),
                bus_guid: "bus".into(),
                parent_id: None,
                child_count: 0,
                role: TrustedText {
                    trust: Trust::Trusted,
                    text: "button".into(),
                },
                name: TrustedText {
                    trust: Trust::External,
                    text: "Apply".into(),
                },
                value: None,
                states: vec!["showing".into()],
                bbox: Some([0, 0, 20, 20]),
                actions: vec![],
            },
        );
    }
}
