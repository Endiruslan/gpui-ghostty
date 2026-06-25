//! Verify DECSTBM scroll-region semantics produce the CORRECT grid (vs spec /
//! hand-computed truth), not merely self-consistent dumps. Claude Code's
//! fullscreen renderer sets a scroll region (e.g. `ESC[2;18r`) to animate the
//! spinner / scroll the conversation while keeping a fixed bottom row. If the
//! VT scrolls the wrong rows or fails to blank exposed rows, content from
//! different lines merges in the grid.

use ghostty_vt::Terminal;

fn rows(t: &Terminal) -> Vec<String> {
    let v = t.dump_viewport().unwrap_or_default();
    v.strip_suffix('\n')
        .unwrap_or(&v)
        .split('\n')
        .map(|s| s.trim_end().to_string())
        .collect()
}

#[test]
fn lf_at_bottom_margin_scrolls_region_and_blanks_exposed_row() {
    // 6 rows. Region = rows 2..5 (1-based, DECSTBM `2;5r`). Rows 1 and 6 fixed.
    let mut t = Terminal::new(20, 6).unwrap();
    t.feed(b"\x1b[1;1HTOP").unwrap(); // row 1 (outside region, fixed)
    t.feed(b"\x1b[6;1HBOTTOM").unwrap(); // row 6 (outside region, fixed)
    t.feed(b"\x1b[2;5r").unwrap(); // scroll region rows 2..5
    // Fill region rows 2,3,4,5 with A,B,C,D (cursor moves are absolute).
    t.feed(b"\x1b[2;1HA\x1b[3;1HB\x1b[4;1HC\x1b[5;1HD").unwrap();

    let before = rows(&t);
    assert_eq!(before[0], "TOP");
    assert_eq!(before[1], "A");
    assert_eq!(before[4], "D");
    assert_eq!(before[5], "BOTTOM");

    // Put cursor at bottom margin (row 5) and emit LF -> region scrolls up:
    // A is lost, B/C/D move up, row 5 becomes blank. Rows 1 and 6 untouched.
    t.feed(b"\x1b[5;1H\n").unwrap();

    let after = rows(&t);
    assert_eq!(after[0], "TOP", "row above region must not move");
    assert_eq!(after[1], "B", "region row 2 should now hold B");
    assert_eq!(after[2], "C");
    assert_eq!(after[3], "D");
    assert_eq!(after[4], "", "exposed bottom region row must be BLANK, not stale A/D");
    assert_eq!(after[5], "BOTTOM", "row below region must not move");
}

#[test]
fn writing_short_line_over_long_line_without_el_leaves_tail() {
    // Sanity: overwriting a long line with a short one and NO erase keeps the
    // tail. This is expected VT behavior — Claude must send ESC[K to clear.
    // (Confirms the merge mechanism: short redraw over stale long row.)
    let mut t = Terminal::new(20, 2).unwrap();
    t.feed(b"\x1b[1;1HLONGLONGLONGLINE").unwrap();
    t.feed(b"\x1b[1;1HSHORT").unwrap();
    assert_eq!(rows(&t)[0], "SHORTONGLONGLINE");

    t.feed(b"\x1b[1;1HSHORT\x1b[K").unwrap();
    assert_eq!(rows(&t)[0], "SHORT", "ESC[K must clear the tail");
}

#[test]
fn su_sd_scroll_region() {
    let mut t = Terminal::new(10, 5).unwrap();
    t.feed(b"\x1b[2;4r").unwrap(); // region rows 2..4
    t.feed(b"\x1b[1;1H1\x1b[2;1H2\x1b[3;1H3\x1b[4;1H4\x1b[5;1H5")
        .unwrap();
    // SU 1 (scroll up within region): row2 lost, 3->2,4->3, row4 blank.
    t.feed(b"\x1b[5;1H").unwrap(); // cursor anywhere
    t.feed(b"\x1b[S").unwrap();
    let r = rows(&t);
    assert_eq!(r[0], "1", "outside region unchanged");
    assert_eq!(r[1], "3");
    assert_eq!(r[2], "4");
    assert_eq!(r[3], "", "exposed region row blank");
    assert_eq!(r[4], "5", "outside region unchanged");
}
