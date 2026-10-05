use ghostty_vt::{FAINT_CLOSE, FAINT_OPEN, Terminal};

fn tail(t: &Terminal) -> String {
    t.dump_active_tail(100)
        .unwrap()
        .replace(FAINT_OPEN, "<")
        .replace(FAINT_CLOSE, ">")
}

#[test]
fn faint_text_is_bracketed_and_plain_text_is_not() {
    let mut t = Terminal::new(20, 2).unwrap();
    t.feed("❯ \x1b[2mTry this\x1b[22m done".as_bytes()).unwrap();
    assert_eq!(tail(&t).trim_end(), "❯ <Try this> done");
}

#[test]
fn a_faint_run_wrapped_onto_the_next_row_closes_at_the_line_break() {
    let mut t = Terminal::new(6, 3).unwrap();
    t.feed(b"ab\x1b[2mcdefgh\x1b[0mi").unwrap();
    assert_eq!(tail(&t).trim_end(), "ab<cdef>\n<gh>i");
}

#[test]
fn a_wide_character_in_a_faint_run_is_one_bracketed_glyph() {
    let mut t = Terminal::new(10, 1).unwrap();
    t.feed("a\x1b[2m漢字\x1b[22mb".as_bytes()).unwrap();
    assert_eq!(tail(&t).trim_end(), "a<漢字>b");
}

#[test]
fn bold_and_coloured_text_is_not_faint() {
    let mut t = Terminal::new(20, 1).unwrap();
    t.feed(b"\x1b[1mbold\x1b[0m \x1b[38;2;153;153;153mgrey\x1b[0m")
        .unwrap();
    assert_eq!(tail(&t).trim_end(), "bold grey");
}
