use ghostty_vt::{KeyAction, KeyEvent, KeyModifiers, Terminal};

fn key<'a>(key: &'a str) -> KeyEvent<'a> {
    KeyEvent {
        key,
        ..Default::default()
    }
}
fn shift_enter(t: &Terminal) -> Vec<u8> {
    t.encode_key(&KeyEvent {
        modifiers: KeyModifiers {
            shift: true,
            ..Default::default()
        },
        ..key("enter")
    })
}
#[test]
fn keyboard_modes_follow_terminal_and_restore() {
    let mut t = Terminal::new(80, 24).unwrap();
    assert_eq!(shift_enter(&t), b"\n");
    t.feed(b"\x1b[>1u").unwrap();
    assert_eq!(shift_enter(&t), b"\x1b[13;2u");
    t.feed(b"\x1b[>8u").unwrap();
    assert_eq!(t.encode_key(&key("enter")), b"\x1b[13u");
    t.feed(b"\x1b[<u").unwrap();
    assert_eq!(shift_enter(&t), b"\x1b[13;2u");
    t.feed(b"\x1b[<u").unwrap();
    assert_eq!(shift_enter(&t), b"\n");
    t.feed(b"\x1b[>4;2m").unwrap();
    assert_eq!(shift_enter(&t), b"\x1b[27;2;13~");
}
#[test]
fn cursor_and_keypad_modes_reach_encoder() {
    let mut t = Terminal::new(80, 24).unwrap();
    assert_eq!(t.encode_key(&key("up")), b"\x1b[A");
    t.feed(b"\x1b[?1h").unwrap();
    assert_eq!(t.encode_key(&key("up")), b"\x1bOA");
    t.feed(b"\x1b[?1l\x1b[?1035l\x1b=").unwrap();
    assert_eq!(t.encode_key(&key("numpad_enter")), b"\x1bOM");
}
#[test]
fn kitty_repeat_release_text_and_modifiers() {
    let mut t = Terminal::new(80, 24).unwrap();
    t.feed(b"\x1b[>31u").unwrap();
    let mut e = KeyEvent {
        text: "A",
        unshifted_codepoint: Some('a'),
        modifiers: KeyModifiers {
            shift: true,
            ..Default::default()
        },
        ..key("a")
    };
    assert_eq!(t.encode_key(&e), b"\x1b[97:65;2;65u");
    e.action = KeyAction::Repeat;
    assert_eq!(t.encode_key(&e), b"\x1b[97:65;2:2;65u");
    e.action = KeyAction::Release;
    // Releases have no associated text.
    assert_eq!(t.encode_key(&e), b"\x1b[97:65;2:3u");
}
#[test]
fn utf8_ctrl_and_alt_use_same_encoder() {
    let mut t = Terminal::new(80, 24).unwrap();
    let mut e = KeyEvent {
        text: "c",
        unshifted_codepoint: Some('c'),
        modifiers: KeyModifiers {
            control: true,
            ..Default::default()
        },
        ..key("c")
    };
    assert_eq!(t.encode_key(&e), b"\x03");
    t.feed(b"\x1b[>1u").unwrap();
    assert_eq!(t.encode_key(&e), b"\x1b[99;5u");
    e.modifiers.control = false;
    e.modifiers.alt = true;
    assert_eq!(t.encode_key(&e), b"\x1b[99;3u");
    let text = KeyEvent {
        text: "Привет 🦀",
        ..Default::default()
    };
    assert_eq!(t.encode_key(&text), "Привет 🦀".as_bytes());
}
#[test]
fn keyboard_state_is_per_screen_and_resettable() {
    let mut t = Terminal::new(80, 24).unwrap();
    t.feed(b"\x1b[>1u\x1b[?1049h").unwrap();
    assert_eq!(shift_enter(&t), b"\n");
    t.feed(b"\x1b[?1049l").unwrap();
    assert_eq!(shift_enter(&t), b"\x1b[13;2u");
    t.feed(b"\x1bc").unwrap();
    assert_eq!(shift_enter(&t), b"\n");
}
