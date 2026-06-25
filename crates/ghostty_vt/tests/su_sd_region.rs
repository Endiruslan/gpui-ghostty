use ghostty_vt::Terminal;

fn rows(t: &Terminal) -> Vec<String> {
    let v = t.dump_viewport().unwrap_or_default();
    v.strip_suffix('\n').unwrap_or(&v).split('\n').map(|s| s.trim_end().to_string()).collect()
}

fn fill(t: &mut Terminal, n: u16) {
    for r in 1..=n {
        t.feed(format!("\x1b[{r};1H{r}").as_bytes()).unwrap();
    }
}

#[test]
fn su_full_screen() {
    // No region. SU 1 scrolls whole screen up: row1 lost, others up, last blank.
    // dump_viewport trims the trailing blank row, so it reports 3 rows.
    let mut t = Terminal::new(10, 4).unwrap();
    fill(&mut t, 4);
    t.feed(b"\x1b[S").unwrap();
    assert_eq!(rows(&t), vec!["2", "3", "4"], "SU on full screen");
}

#[test]
fn sd_full_screen() {
    let mut t = Terminal::new(10, 4).unwrap();
    fill(&mut t, 4);
    t.feed(b"\x1b[T").unwrap();
    assert_eq!(rows(&t), vec!["", "1", "2", "3"], "SD on full screen");
}

#[test]
fn sd_within_region_claude_pattern() {
    // Exactly Claude's fullscreen scroll: region 2..4, SD 1, reset region.
    let mut t = Terminal::new(10, 5).unwrap();
    fill(&mut t, 5);
    t.feed(b"\x1b[2;4r").unwrap(); // region rows 2..4
    t.feed(b"\x1b[T").unwrap(); // SD 1 within region
    t.feed(b"\x1b[r").unwrap(); // reset region
    // Expect: rows 1 and 5 fixed; region 2..4 scrolled down: row2 blank, 2->3, 3->4.
    assert_eq!(
        rows(&t),
        vec!["1".to_string(), "".to_string(), "2".to_string(), "3".to_string(), "5".to_string()],
        "SD within region must scroll rows 2..4 down and blank row 2"
    );
}

#[test]
fn su_within_region_claude_pattern() {
    let mut t = Terminal::new(10, 5).unwrap();
    fill(&mut t, 5);
    t.feed(b"\x1b[2;4r").unwrap();
    t.feed(b"\x1b[S").unwrap(); // SU 1 within region
    t.feed(b"\x1b[r").unwrap();
    assert_eq!(
        rows(&t),
        vec!["1".to_string(), "3".to_string(), "4".to_string(), "".to_string(), "5".to_string()],
        "SU within region must scroll rows 2..4 up and blank row 4"
    );
}
