use ghostty_vt::{KeyEvent, KeyModifiers, Terminal};

fn encode_key_named(key: &str, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    Some(Terminal::new(80, 24).unwrap().encode_key(&KeyEvent {
        key,
        modifiers,
        ..Default::default()
    }))
}

#[test]
fn encodes_common_special_keys() {
    assert_eq!(
        encode_key_named("up", KeyModifiers::default()).as_deref(),
        Some(&b"\x1b[A"[..])
    );
    assert_eq!(
        encode_key_named("f1", KeyModifiers::default()).as_deref(),
        Some(&b"\x1bOP"[..])
    );
    assert_eq!(
        encode_key_named("pageup", KeyModifiers::default()).as_deref(),
        Some(&b"\x1b[5~"[..])
    );
}

#[test]
fn encoding_changes_with_modifiers_for_special_keys() {
    let no_mods = encode_key_named("up", KeyModifiers::default()).unwrap();
    let ctrl = encode_key_named(
        "up",
        KeyModifiers {
            control: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_ne!(no_mods, ctrl);
}

#[test]
fn shift_enter_sends_newline_without_changing_enter() {
    assert_eq!(
        encode_key_named("enter", KeyModifiers::default()).as_deref(),
        Some(&b"\r"[..])
    );
    assert_eq!(
        encode_key_named(
            "enter",
            KeyModifiers {
                shift: true,
                ..Default::default()
            }
        )
        .as_deref(),
        Some(&b"\n"[..])
    );
}

#[test]
fn shift_enter_with_other_modifiers_keeps_ghostty_encoding() {
    for modifiers in [
        KeyModifiers {
            shift: true,
            control: true,
            ..Default::default()
        },
        KeyModifiers {
            shift: true,
            alt: true,
            ..Default::default()
        },
        KeyModifiers {
            shift: true,
            super_key: true,
            ..Default::default()
        },
    ] {
        assert_ne!(
            encode_key_named("enter", modifiers).as_deref(),
            Some(&b"\n"[..])
        );
    }
}
