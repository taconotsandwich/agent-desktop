use super::*;
use crate::platform::keymap::parse_chord;

fn map(layout: &str, options: &str) -> xkb::Keymap {
    let context = xkb::Context::new(xkb::CONTEXT_NO_ENVIRONMENT_NAMES);
    xkb::Keymap::new_from_names(
        &context,
        "evdev",
        "pc105",
        layout,
        "",
        Some(options.into()),
        xkb::COMPILE_NO_FLAGS,
    )
    .unwrap()
}
fn keyboard(layout: &str) -> Keyboard {
    Keyboard::new(map(layout, ""), NativeState::default())
}
fn generated(keyboard: &Keyboard, stroke: &Stroke) -> xkb::Keysym {
    keyboard
        .state_with(&stroke.modifiers)
        .key_get_one_sym(stroke.key.into())
}

#[test]
fn literal_text_uses_each_layouts_actual_levels() {
    for (layout, text) in [
        ("us", "aAzZ019!?@#[]{}+-_/"),
        ("de", "aAzZyY019!?@€[]{}ßäÄ"),
        ("fr", "aAzZwW019!?@€[]{}é"),
        ("gr", "αΑωΩ019!?"),
    ] {
        let keyboard = keyboard(layout);
        for ch in text.chars() {
            let stroke = keyboard
                .literal(ch)
                .unwrap_or_else(|e| panic!("{layout} {ch:?}: {e}"));
            assert_eq!(
                generated(&keyboard, &stroke),
                super::super::keymap::literal_symbol(ch).unwrap(),
                "{layout} {ch:?}"
            );
        }
    }
}

#[test]
fn shortcut_case_does_not_implicitly_hold_shift_in_any_alphabet() {
    for (layout, upper, lower_case) in [
        ("us", "Z", "z"),
        ("de", "Ä", "ä"),
        ("fr", "É", "é"),
        ("gr", "Ω", "ω"),
    ] {
        let keyboard = keyboard(layout);
        let upper = keyboard
            .chord(&parse_chord(&format!("Ctrl+{upper}")).unwrap())
            .unwrap();
        let lower_case = keyboard
            .chord(&parse_chord(&format!("Ctrl+{lower_case}")).unwrap())
            .unwrap();
        assert_eq!(upper, lower_case, "{layout}");
        assert_eq!(
            upper.modifiers,
            keyboard.modifiers(&[Modifier::Ctrl]).unwrap()
        );
    }
}

#[test]
fn explicit_shift_and_symbol_required_shift_are_distinct() {
    let keyboard = keyboard("us");
    let undo = keyboard.chord(&parse_chord("Ctrl+Z").unwrap()).unwrap();
    let redo = keyboard
        .chord(&parse_chord("Ctrl+Shift+Z").unwrap())
        .unwrap();
    assert_eq!(undo.key, redo.key);
    assert_eq!(undo.modifiers.len(), 1);
    assert_eq!(redo.modifiers.len(), 2);
    assert_eq!(generated(&keyboard, &redo), xkb::Keysym::Z);
    let question = keyboard.chord(&parse_chord("Ctrl+?").unwrap()).unwrap();
    assert_eq!(generated(&keyboard, &question), xkb::Keysym::question);
    assert_eq!(question.modifiers.len(), 2);
    for name in ["F1", "F12", "Return", "Left", "Delete"] {
        let chord = parse_chord(name).unwrap();
        let stroke = keyboard.chord(&chord).unwrap();
        assert_eq!(generated(&keyboard, &stroke), chord.key);
    }
}

#[test]
fn caps_lock_preserves_literal_case_and_shortcut_identity() {
    let map = map("us", "");
    let caps = 1 << map.mod_get_index(xkb::MOD_NAME_CAPS);
    let keyboard = Keyboard::new(
        map,
        NativeState {
            locked: caps,
            ..Default::default()
        },
    );
    for ch in "aAzZ!?".chars() {
        let stroke = keyboard.literal(ch).unwrap();
        assert_eq!(
            generated(&keyboard, &stroke),
            xkb::utf32_to_keysym(ch as u32)
        );
    }
    let stroke = keyboard.chord(&parse_chord("Ctrl+Z").unwrap()).unwrap();
    assert_eq!(stroke.modifiers.len(), 1);
}

#[test]
fn layout_group_and_modifier_remapping_are_respected() {
    let map = map("us,de", "ctrl:swapcaps");
    let first = Keyboard::new(map.clone(), NativeState::default());
    let second = Keyboard::new(
        map,
        NativeState {
            group: 1,
            ..Default::default()
        },
    );
    let chord = parse_chord("Ctrl+Y").unwrap();
    let first_stroke = first.chord(&chord).unwrap();
    let second_stroke = second.chord(&chord).unwrap();
    assert_ne!(first_stroke.key, second_stroke.key);
    assert_eq!(
        first.keymap.key_get_name(first_stroke.modifiers[0].into()),
        Some("CAPS")
    );
    assert_eq!(generated(&second, &second_stroke), xkb::Keysym::y);
}

#[test]
fn already_held_modifiers_are_not_owned_or_released() {
    let map = map("us", "");
    let ctrl = 1 << map.mod_get_index(xkb::MOD_NAME_CTRL);
    let keyboard = Keyboard::new(
        map,
        NativeState {
            depressed: ctrl,
            ..Default::default()
        },
    );
    let stroke = keyboard.chord(&parse_chord("Ctrl+Z").unwrap()).unwrap();
    assert!(stroke.modifiers.is_empty());
}

#[test]
fn unavailable_symbols_fail_without_a_us_fallback() {
    assert!(
        keyboard("us")
            .chord(&parse_chord("Ctrl+Ω").unwrap())
            .is_err()
    );
    assert!(keyboard("us").literal('é').is_err());
    assert!(Keyboard::from_text("not an XKB keymap".into(), NativeState::default()).is_err());
}

#[test]
fn event_state_masks_follow_core_modifier_bits() {
    let map = map("us,de", "");
    let caps = 1 << map.mod_get_index(xkb::MOD_NAME_CAPS);
    let keyboard = Keyboard::new(map.clone(), NativeState::default());
    let keys = keyboard
        .modifiers(&[Modifier::Ctrl, Modifier::Shift])
        .unwrap();
    assert_eq!(keyboard.mask(&keys), 0x5);
    assert_eq!(keyboard.mask(&[]), 0);
    let stroke = keyboard.chord(&parse_chord("Ctrl+?").unwrap()).unwrap();
    assert_eq!(keyboard.mask(&stroke.modifiers), 0x5);
    let locked = Keyboard::new(
        map.clone(),
        NativeState {
            depressed: 1 << map.mod_get_index(xkb::MOD_NAME_CTRL),
            locked: caps,
            group: 1,
            ..Default::default()
        },
    )
    .ignoring_held();
    assert_eq!(locked.mask(&[]), caps as u16 | (1 << 13));
    let stroke = locked.chord(&parse_chord("Ctrl+Z").unwrap()).unwrap();
    assert_eq!(stroke.modifiers.len(), 1, "held Ctrl is ignored");
    assert_eq!(
        locked.mask(&stroke.modifiers),
        0x4 | caps as u16 | (1 << 13)
    );
}
