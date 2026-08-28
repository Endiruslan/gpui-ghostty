//! Bytes a paste sends to the shell.
//!
//! When the foreground program enabled bracketed paste (DECSET 2004 — every
//! interactive coding agent does) the text is wrapped in `ESC[200~ … ESC[201~`
//! so a multi-line block arrives as one paste instead of submitting on the
//! first newline. Otherwise the bytes go through as typed.

use std::borrow::Cow;

pub(crate) const BRACKET_START: &str = "\x1b[200~";
pub(crate) const BRACKET_END: &str = "\x1b[201~";

/// Drop every bracketed-paste delimiter the payload itself carries.
///
/// A pasted `ESC[201~` would end the paste early and hand the shell the rest
/// of the text as *typed* input — the classic bracketed-paste escape, and the
/// reason xterm, iTerm2 and Ghostty all filter the terminator out of the
/// payload. `ESC[200~` goes with it for symmetry: a stray opener inside a
/// paste has no meaning either.
///
/// Borrows in the overwhelmingly common case (no delimiter in sight).
fn strip_delimiters(text: &str) -> Cow<'_, str> {
    if !text.contains(BRACKET_START) && !text.contains(BRACKET_END) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace(BRACKET_START, "").replace(BRACKET_END, ""))
}

pub(crate) fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let text = strip_delimiters(text);
        let mut out = Vec::with_capacity(text.len() + BRACKET_START.len() + BRACKET_END.len());
        out.extend_from_slice(BRACKET_START.as_bytes());
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(BRACKET_END.as_bytes());
        out
    } else {
        text.as_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_when_bracketed() {
        assert_eq!(
            paste_bytes("a\nb", true),
            b"\x1b[200~a\nb\x1b[201~".to_vec()
        );
    }

    #[test]
    fn raw_when_not_bracketed() {
        assert_eq!(paste_bytes("a\nb", false), b"a\nb".to_vec());
    }

    /// A payload carrying the terminator must not be able to close the paste
    /// early: exactly one `ESC[201~` leaves this function, the trailer.
    #[test]
    fn terminator_inside_payload_is_stripped() {
        let bytes = paste_bytes("a\x1b[201~rm -rf /\x1b[201~b", true);
        let text = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(text.matches(BRACKET_END).count(), 1);
        assert!(text.ends_with(BRACKET_END));
        assert_eq!(text, "\x1b[200~arm -rf /b\x1b[201~");
    }

    /// The opener goes too, so a payload cannot nest a paste of its own.
    #[test]
    fn opener_inside_payload_is_stripped() {
        let bytes = paste_bytes("a\x1b[200~b", true);
        let text = String::from_utf8(bytes).expect("utf-8");
        assert_eq!(text.matches(BRACKET_START).count(), 1);
        assert!(text.starts_with(BRACKET_START));
        assert_eq!(text, "\x1b[200~ab\x1b[201~");
    }

    /// Unbracketed paste is a byte pipe — nothing is filtered.
    #[test]
    fn delimiters_survive_when_not_bracketed() {
        assert_eq!(paste_bytes("a\x1b[201~b", false), b"a\x1b[201~b".to_vec());
    }
}
