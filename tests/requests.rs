use agent_desktop::request::{Operation, PointerTarget, Request};
use agent_desktop::types::Button;
use serde_json::json;

#[test]
fn javascript_envelopes_accept_empty_options_and_preserve_action_defaults() {
    for method in ["getState", "listApps", "getScreenshot"] {
        serde_json::from_value::<Request>(json!({"method":method,"target":null,"args":{}}))
            .unwrap();
    }
    let request: Request = serde_json::from_value(json!({
        "method":"click", "target":"application.desktop", "args":{"target":[20,30]}
    }))
    .unwrap();
    let Operation::Click(click) = request.operation else {
        panic!("click operation")
    };
    assert_eq!(click.click_count, 1);
    assert_eq!(click.mouse_button, Button::Left);
    assert!(matches!(
        click.target,
        PointerTarget::Coordinates([20.0, 30.0])
    ));
    let request: Request = serde_json::from_value(json!({
        "method":"getAXState", "target":"application.desktop", "args":{"emit":false}
    }))
    .unwrap();
    assert!(
        matches!(request.operation, Operation::GetAxState(options) if !options.disable_diffing)
    );
}

#[test]
fn malformed_actions_are_rejected_before_dispatch() {
    for args in [
        json!({}),
        json!({"target":[1]}),
        json!({"target":["x",2]}),
        json!({"target":7,"clickCount":"2"}),
        json!({"target":7,"mouseButton":"invalid"}),
    ] {
        assert!(
            serde_json::from_value::<Request>(json!({
                "method":"click", "target":"application.desktop", "args":args
            }))
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<Request>(json!({
            "method":"typeText", "target":"application.desktop", "args":{"text":42}
        }))
        .is_err()
    );
}
