#[test]
fn viewport_dump_contains_text() {
    let mut t = ghostty_vt::Terminal::new(80, 24).unwrap();
    t.feed(b"hello\r\nworld\r\n").unwrap();
    t.feed(b"\x1b[31mred\x1b[0m\r\n").unwrap();

    let s = t.dump_viewport().unwrap();
    assert!(s.contains("hello"));
    assert!(s.contains("world"));
    assert!(s.contains("red"));
}

#[test]
fn viewport_row_dump_contains_text() {
    let mut t = ghostty_vt::Terminal::new(80, 24).unwrap();
    t.feed(b"hello\r\n").unwrap();

    let row0 = t.dump_viewport_row(0).unwrap();
    assert!(row0.contains("hello"));
}

#[test]
fn osc_133_prompt_and_input_start_events_drain() {
    use ghostty_vt::TerminalEvent;

    let mut t = ghostty_vt::Terminal::new(80, 24).unwrap();

    // OSC 133;A — new prompt about to be drawn.
    t.feed(b"\x1b]133;A\x07").unwrap();
    assert_eq!(t.drain_events(), vec![TerminalEvent::PromptStart]);

    // Prompt text, then OSC 133;B — prompt done, input starts here.
    t.feed(b"$ \x1b]133;B\x07").unwrap();
    assert_eq!(t.drain_events(), vec![TerminalEvent::InputStart]);

    // Draining again returns nothing until the next OSC 133 marker.
    assert!(t.drain_events().is_empty());
}

#[test]
fn osc_133_full_prompt_cycle_drains_in_order() {
    use ghostty_vt::TerminalEvent;

    let mut t = ghostty_vt::Terminal::new(80, 24).unwrap();

    // A (prompt start) -> prompt text -> B (input start) -> typed command
    // -> C (command start / end of input) -> output -> D (command end).
    t.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\x1b]133;C\x07")
        .unwrap();
    assert_eq!(
        t.drain_events(),
        vec![
            TerminalEvent::PromptStart,
            TerminalEvent::InputStart,
            TerminalEvent::CommandStart { cmd: None }
        ]
    );

    t.feed(b"hi\r\n\x1b]133;D;0\x07").unwrap();
    assert_eq!(
        t.drain_events(),
        vec![TerminalEvent::CommandEnd { exit_code: 0 }]
    );
}

#[test]
fn osc_9_4_progress_report_events_drain() {
    use ghostty_vt::TerminalEvent;

    let mut t = ghostty_vt::Terminal::new(80, 24).unwrap();

    // ConEmu OSC 9;4 — grok's "working" payload. `payload` must be the raw
    // text after "9;" verbatim: ghostty's own parsed state/percentage
    // (osc/parsers/osc9.zig) can't reproduce this exact string (it drops
    // the percentage for `remove`/`indeterminate` and defaults `set` to 0),
    // which is exactly what agent-detection manifests match against
    // (grok.toml's `osc_progress_working`/`osc_progress_idle` rules).
    t.feed(b"\x1b]9;4;1;-1\x07").unwrap();
    assert_eq!(
        t.drain_events(),
        vec![TerminalEvent::ProgressReport {
            payload: "4;1;-1".to_string(),
        }]
    );

    // grok's "idle" payload — note the trailing "0" survives verbatim here,
    // even though ghostty's own parser would have discarded it.
    t.feed(b"\x1b]9;4;0;0\x07").unwrap();
    assert_eq!(
        t.drain_events(),
        vec![TerminalEvent::ProgressReport {
            payload: "4;0;0".to_string(),
        }]
    );
}
