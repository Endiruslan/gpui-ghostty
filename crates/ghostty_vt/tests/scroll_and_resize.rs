#[test]
fn scrollback_jump_top_and_bottom() {
    let mut t = ghostty_vt::Terminal::new(20, 5).unwrap();

    for i in 0..50 {
        t.feed(format!("line-{i:02}\r\n").as_bytes()).unwrap();
    }

    let bottom = t.dump_viewport().unwrap();
    assert!(bottom.contains("line-49"));
    assert!(!bottom.contains("line-00"));

    t.scroll_viewport_top().unwrap();
    let top = t.dump_viewport().unwrap();
    assert!(top.contains("line-00"));

    t.scroll_viewport_bottom().unwrap();
    let bottom_again = t.dump_viewport().unwrap();
    assert!(bottom_again.contains("line-49"));
}

#[test]
fn resize_does_not_break_dump_or_feed() {
    let mut t = ghostty_vt::Terminal::new(10, 3).unwrap();
    t.feed(b"hello\r\nworld\r\n").unwrap();
    t.resize(30, 10).unwrap();
    t.feed(b"after-resize\r\n").unwrap();

    let s = t.dump_viewport().unwrap();
    assert!(s.contains("after-resize"));
}

/// A pane keeps Ghostty's own default history (10 MB), not libghostty's
/// 10 000-byte `Terminal.Options` default, which held ~800 rows: after
/// `seq 1 20000` everything but the last ~800 lines was gone. 10 MB is
/// ~8 900 rows: history is allocated in standard pages sized for 215
/// columns whatever the pane's width, so the ceiling is a row count, the
/// same one Ghostty itself has with its default `scrollback-limit`.
#[test]
fn scrollback_keeps_ghostty_default_history() {
    let mut t = ghostty_vt::Terminal::new(80, 24).unwrap();

    for i in 0..8_000 {
        t.feed(format!("line-{i:05}\r\n").as_bytes()).unwrap();
    }

    t.scroll_viewport_top().unwrap();
    let top = t.dump_viewport().unwrap();
    assert!(
        top.starts_with("line-00000"),
        "oldest retained row is not the first line: {:?}",
        top.lines().next()
    );
}
