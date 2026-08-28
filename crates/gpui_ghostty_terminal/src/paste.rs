//! Bytes a paste sends to the shell.
//!
//! When the foreground program enabled bracketed paste (DECSET 2004 — every
//! interactive coding agent does) the text is wrapped in `ESC[200~ … ESC[201~`
//! so a multi-line block arrives as one paste instead of submitting on the
//! first newline. Otherwise the bytes go through as typed.

pub(crate) const BRACKET_START: &[u8] = b"\x1b[200~";
pub(crate) const BRACKET_END: &[u8] = b"\x1b[201~";

pub(crate) fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = Vec::with_capacity(text.len() + BRACKET_START.len() + BRACKET_END.len());
        out.extend_from_slice(BRACKET_START);
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(BRACKET_END);
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
}
