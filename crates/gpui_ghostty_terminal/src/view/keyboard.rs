//! Translate GPUI input once; Ghostty owns all terminal protocol decisions.
use crate::TerminalSession;
use ghostty_vt::{KeyAction, KeyEvent, KeyModifiers};
use gpui::{Keystroke, Modifiers, NativeKeyboardEvent};

pub(super) fn modifiers(m: Modifiers) -> KeyModifiers {
    KeyModifiers {
        shift: m.shift,
        control: m.control,
        alt: m.alt,
        super_key: m.platform,
    }
}

pub(super) fn encode(
    session: &TerminalSession,
    key: &Keystroke,
    native: Option<&NativeKeyboardEvent>,
    action: KeyAction,
    composing: bool,
) -> Vec<u8> {
    let printable = key.key == "space" || key.key.chars().count() == 1;
    let mods = modifiers(native.map_or(key.modifiers, |n| n.modifiers));
    let text = if printable {
        native
            .map(|n| n.text.as_str())
            .or(key.key_char.as_deref())
            .unwrap_or(if key.key == "space" { " " } else { &key.key })
    } else {
        ""
    };
    let unshifted = native.and_then(|n| n.unshifted_codepoint).or_else(|| {
        if key.key == "space" {
            Some(' ')
        } else if printable {
            key.key.chars().next().and_then(|c| c.to_lowercase().next())
        } else {
            None
        }
    });
    session.encode_key(&KeyEvent {
        key: &key.key,
        text,
        modifiers: mods,
        consumed_modifiers: KeyModifiers {
            shift: printable && mods.shift && !text.is_empty(),
            ..Default::default()
        },
        unshifted_codepoint: unshifted,
        native_keycode: native.map(|n| n.keycode),
        caps_lock: native.is_some_and(|n| n.capslock),
        action,
        composing,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TerminalConfig;

    #[test]
    fn normalized_shift_symbol_retains_native_key_identity() {
        let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
        session.feed(b"\x1b[>31u").unwrap();
        let key = Keystroke::parse("@").unwrap();
        let native = NativeKeyboardEvent {
            keycode: 19,
            is_down: true,
            is_repeat: false,
            modifiers: Modifiers {
                shift: true,
                ..Default::default()
            },
            capslock: false,
            unshifted_codepoint: Some('2'),
            text: "@".into(),
        };
        assert_eq!(
            encode(&session, &key, Some(&native), KeyAction::Press, false),
            b"\x1b[50:64;2;64u"
        );
        assert!(encode(&session, &key, Some(&native), KeyAction::Press, true).is_empty());
    }

    #[test]
    fn non_latin_text_and_ctrl_use_native_layout_and_physical_key() {
        let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
        let mut key = Keystroke::parse("с").unwrap().with_simulated_ime();
        let mut native = NativeKeyboardEvent {
            keycode: 8,
            is_down: true,
            is_repeat: false,
            modifiers: Modifiers::default(),
            capslock: false,
            unshifted_codepoint: Some('с'),
            text: "с".into(),
        };
        assert_eq!(
            encode(&session, &key, Some(&native), KeyAction::Press, false),
            "с".as_bytes()
        );
        native.modifiers.control = true;
        key.modifiers.control = true;
        assert_eq!(
            encode(&session, &key, Some(&native), KeyAction::Press, false),
            b"\x03"
        );
        session.feed(b"\x1b[>1u").unwrap();
        assert_eq!(
            encode(&session, &key, Some(&native), KeyAction::Press, false),
            b"\x1b[1089;5u"
        );
    }
}
