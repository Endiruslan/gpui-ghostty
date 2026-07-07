//! Regression: flag emoji (Regional Indicator pairs) must occupy 2 cells.
//!
//! DEC mode 2027 (grapheme clustering) must be enabled at Terminal init so a
//! flag emoji clusters into a single width-2 grapheme. With it OFF, each
//! Regional Indicator is printed separately at its own table width (RIS = 2),
//! so a flag takes 4 cells and TUIs that assume grapheme-clustered widths
//! (Claude Code, etc.) render misaligned tables.

#[test]
fn flag_emoji_occupies_two_cells() {
    // Baseline: two ASCII cells.
    let mut ascii = ghostty_vt::Terminal::new(80, 24).unwrap();
    ascii.feed(b"XX").unwrap();
    let (ascii_col, _) = ascii.cursor_position().unwrap();

    // 🇫🇷 = U+1F1EB U+1F1F7 (two Regional Indicators = one flag grapheme).
    let mut flag = ghostty_vt::Terminal::new(80, 24).unwrap();
    flag.feed("\u{1F1EB}\u{1F1F7}".as_bytes()).unwrap();
    let (flag_col, _) = flag.cursor_position().unwrap();

    assert_eq!(
        flag_col, ascii_col,
        "flag emoji must advance the cursor by 2 cells (same as two ASCII), \
         got flag_col={flag_col} vs ascii_col={ascii_col} — grapheme clustering off?"
    );
}
