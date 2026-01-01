use ghostty_vt::{KeyModifiers, encode_key_named};

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
