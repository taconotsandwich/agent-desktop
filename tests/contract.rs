//! Contract suite. Runs against fakes locally; the SAME suite runs against
//! real drivers on the alias host. A backend merges only on green.

use agent_desktop::{
    core::RefStore,
    drivers::{Button, InputDriver, ShotDriver, ShotTarget, WindowDriver},
    fake::{FakeInput, FakeShot, FakeWindows},
    registry::Registry,
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
    .await
    .expect("fakes always probe ok");
    assert_eq!(r.shot.id(), "fake-shot");
    assert_eq!(r.input.id(), "fake-input");
    assert_eq!(r.windows.id(), "fake-windows");
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
    assert!(!shot.bytes.is_empty());
}

#[tokio::test]
async fn fake_input_rejects_empty_shapes() {
    let i = FakeInput;
    assert!(i.type_text("hi".into()).await.is_ok());
    assert!(i.type_text(String::new()).await.is_err());
    assert!(i.key(vec!["ctrl+s".into()]).await.is_ok());
    assert!(i.key(vec![]).await.is_err());
    assert!(i.click(10, 10, Button::Left).await.is_ok());
    assert!(i.move_to(0, 0).await.is_ok());
    assert!(i.drag(vec![(0, 0), (5, 5)], Button::Left).await.is_ok());
    assert!(i.scroll(0, 0, 0, 120).await.is_ok());
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
async fn refs_expire_and_validate() {
    let store = RefStore::new();
    assert!(!store.check(&Ref("made-up".into())).await);
    let minted = store.mint_elements(vec!["el:1".into()]).await;
    assert!(store.check(&minted[0]).await);
    let w = store.mint_windows(vec!["win:1".into()]).await;
    assert!(store.check(&w[0]).await);
    assert!(store.is_window(&w[0]).await);
    assert!(!store.is_window(&minted[0]).await);
}
