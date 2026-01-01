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
