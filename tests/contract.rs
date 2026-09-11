use fakes::{FakeInput, FakeShot, FakeWindows};
#[path = "support/fakes.rs"]
mod fakes;

use agent_desktop::{
    platform::drivers::{Button, InputDriver, ShotDriver, ShotTarget, WindowDriver},
    platform::registry::Registry,
    types::Ref,
};
use std::sync::Arc;

#[tokio::test]
async fn registry_picks_first_ok_per_capability() {
    let r = Registry::probe(
        vec![Arc::new(FakeShot)],
        vec![Arc::new(FakeInput)],
        vec![Arc::new(FakeWindows)],
    )
    .await;
    assert_eq!(r.shot().unwrap().id(), "fake-shot");
    assert_eq!(r.input().unwrap().id(), "fake-input");
    assert_eq!(r.windows().unwrap().id(), "fake-windows");
    assert_eq!(r.probes.len(), 3);
    assert!(r.probes.iter().all(|p| p.ok));
}

#[tokio::test]
async fn fake_shot_returns_mappable_frame() {
    let s = FakeShot;
    let shot = s
        .capture(ShotTarget::Full, 1280)
        .await
        .expect("fake capture ok");
    assert_eq!((shot.coord_w, shot.coord_h), (1600, 900));
    let frame = agent_desktop::desktop::geometry::Frame::from_shot(
        &shot,
        agent_desktop::types::Bbox {
            x: 0,
            y: 0,
            w: 1600,
            h: 900,
        },
    )
    .unwrap();
    assert_eq!(frame.point(400.0, 225.0).unwrap(), (800, 450));
    assert!(frame.point(800.0, 0.0).is_err());
}

#[tokio::test]
async fn fake_input_rejects_empty_shapes() {
    let i = FakeInput;
    assert!(i.type_text("hi".into()).await.is_ok());
    assert!(i.type_text(String::new()).await.is_err());
    assert!(i.key(vec!["ctrl+s".into()]).await.is_ok());
    assert!(i.key(vec![]).await.is_err());
    assert!(i.click(10, 10, Button::Left, vec![]).await.is_ok());
    assert!(i.move_to(0, 0).await.is_ok());
    assert!(
        i.drag(vec![(0, 0), (5, 5)], Button::Left, 300, 15)
            .await
            .is_ok()
    );
    assert!(i.scroll(0, 0, 0, 120, vec![]).await.is_ok());
}

#[tokio::test]
async fn fake_windows_lists_one_active() {
    let w = FakeWindows;
    let wins = w.query().await.expect("fake query ok");
    assert_eq!(wins.len(), 1);
    assert!(wins[0].is_active);
    assert!(w.focus(&Ref("fake:1".into())).await.is_ok());
}

#[tokio::test]
async fn missing_input_does_not_disable_available_observations() {
    let registry = Registry::probe(
        vec![Arc::new(FakeShot)],
        vec![],
        vec![Arc::new(FakeWindows)],
    )
    .await;
    assert!(registry.shot().is_ok());
    assert!(registry.windows().is_ok());
    assert!(registry.input().is_err());
    assert_eq!(registry.probes.len(), 2);
}
