use super::*;
use crate::{
    test_support::FakeWindows,
    types::{Ref, Trust, TrustedText},
};

#[tokio::test]
async fn observations_authorize_only_the_current_window_and_fresh_indices() {
    let registry = Registry::probe(vec![], vec![], vec![Arc::new(FakeWindows)]).await;
    let engine = Engine::new(registry, SessionType::X11, Arc::new(AtspiConnection::new()));
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
