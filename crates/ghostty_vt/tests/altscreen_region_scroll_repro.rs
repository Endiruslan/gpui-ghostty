use ghostty_vt::Terminal;

/// Mirror of `split_viewport_lines` in gpui_ghostty_terminal/src/view/mod.rs.
fn split(viewport: &str) -> Vec<String> {
    let v = viewport.strip_suffix('\n').unwrap_or(viewport);
    if v.is_empty() {
        return Vec::new();
    }
    v.split('\n').map(|s| s.to_string()).collect()
}

/// Ground truth: a full re-dump of the viewport (what `refresh_viewport` does,
/// and what happens when the user selects text).
fn full_dump(t: &Terminal) -> Vec<String> {
    split(&t.dump_viewport().unwrap_or_default())
}

/// Mirror of mxds's incremental reconcile after PTY output:
/// `apply_viewport_scroll_delta(delta)` then `apply_dirty_viewport_rows(dirty)`.
/// Operates on a cached `lines` Vec the way `TerminalView` keeps `viewport_lines`.
fn reconcile(t: &mut Terminal, rows: u16, lines: &mut Vec<String>) {
    let rows_us = rows as usize;

    // --- apply_viewport_scroll_delta ---
    let delta = t.take_viewport_scroll_delta();
    if delta != 0 {
        if lines.len() != rows_us {
            *lines = full_dump(t);
        } else {
            let d = delta.unsigned_abs() as usize;
            if d >= rows_us {
                *lines = full_dump(t);
            } else if delta > 0 {
                lines.rotate_left(d);
                for i in rows_us - d..rows_us {
                    lines[i].clear();
                }
            } else {
                lines.rotate_right(d);
                for line in lines.iter_mut().take(d) {
                    line.clear();
                }
            }
        }
    }

    // --- apply_dirty_viewport_rows ---
    let dirty = t.take_dirty_viewport_rows(rows).unwrap();
    if lines.len() != rows_us {
        *lines = full_dump(t);
        return;
    }
    for &row in &dirty {
        let r = row as usize;
        if r >= lines.len() {
            continue;
        }
        let line = t.dump_viewport_row(row).unwrap();
        let line = line.strip_suffix('\n').unwrap_or(line.as_str());
        lines[r].clear();
        lines[r].push_str(line);
    }
}

/// Fullscreen TUIs (Claude Code `/tui fullscreen`, vim, htop) enter the alt
/// screen and keep a fixed status/input row at the bottom by setting a scroll
/// region (DECSTBM) above it, then scroll content WITHIN that region.
///
/// This test asserts the incremental reconcile path produces the same viewport
/// as a full re-dump. If it diverges, the live terminal shows stale/garbled
/// rows until something forces a full refresh (e.g. selecting text).
#[test]
fn altscreen_region_scroll_matches_full_dump() {
    let rows: u16 = 6;
    let mut t = Terminal::new(20, rows).unwrap();

    // Enter alt screen + prime caches the way the view does on first paint.
    t.feed(b"\x1b[?1049h").unwrap();
    let mut lines = full_dump(&t);
    let _ = t.take_viewport_scroll_delta();
    let _ = t.take_dirty_viewport_rows(rows).unwrap();

    // Fixed bottom row (the "input box"): row 6. Scroll region = rows 1..5.
    t.feed(b"\x1b[6;1Hinput-box").unwrap();
    // Set scroll region to rows 1..5 (1-based, inclusive) -> top 5 rows scroll.
    t.feed(b"\x1b[1;5r").unwrap();
    // Home, fill the region with 5 lines.
    t.feed(b"\x1b[1;1H").unwrap();
    t.feed(b"line-A\r\nline-B\r\nline-C\r\nline-D\r\nline-E").unwrap();
    reconcile(&mut t, rows, &mut lines);
    assert_eq!(lines, full_dump(&t), "after initial region fill");

    // Now scroll the region: cursor at bottom of region, emit newlines so new
    // content scrolls the region up while the bottom input row stays put.
    for n in 0..4 {
        t.feed(format!("\x1b[5;1H\r\nnew-{n}").as_bytes()).unwrap();
        reconcile(&mut t, rows, &mut lines);
        assert_eq!(
            lines,
            full_dump(&t),
            "after region scroll #{n}: incremental reconcile diverged from full dump"
        );
    }
}
