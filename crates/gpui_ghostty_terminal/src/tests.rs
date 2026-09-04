use gpui::{KeyBinding, KeyContext, Keymap, Keystroke, actions};
use std::any::TypeId;

use crate::{ColorScheme, TerminalConfig, TerminalSession};

actions!(tab_shadow_test, [RootTab, TerminalTab]);

fn osc_color_response(ps: u8, (r, g, b): (u8, u8, u8)) -> String {
    let r16 = u16::from(r) * 0x0101;
    let g16 = u16::from(g) * 0x0101;
    let b16 = u16::from(b) * 0x0101;

    format!("\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\", ps, r16, g16, b16)
}

fn viewport_index_for_cell(viewport: &str, row: u16, col: u16) -> usize {
    let row = row.max(1) as usize;
    let col = col.max(1) as usize;

    use unicode_width::UnicodeWidthChar as _;

    let mut current_row = 1usize;
    let mut offset = 0usize;

    for segment in viewport.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);

        if current_row == row {
            if col == 1 {
                return offset;
            }

            let mut current_col = 1usize;
            for (byte_index, ch) in line.char_indices() {
                let width = ch.width().unwrap_or(0);
                if width == 0 {
                    continue;
                }

                if current_col == col {
                    return offset + byte_index;
                }

                let next_col = current_col.saturating_add(width);
                if col < next_col {
                    return offset + byte_index;
                }

                current_col = next_col;
            }

            return offset + line.len();
        }

        offset = offset.saturating_add(segment.len());
        current_row += 1;
    }

    viewport.len()
}

#[test]
fn terminal_tab_binding_shadows_root_tab_binding() {
    let mut keymap = Keymap::default();
    keymap.add_bindings([
        KeyBinding::new("tab", RootTab, Some("Root")),
        KeyBinding::new("tab", TerminalTab, Some("Terminal")),
    ]);

    let mut root = KeyContext::default();
    root.add("Root");
    let mut terminal = KeyContext::default();
    terminal.add("Terminal");

    let (bindings, pending) =
        keymap.bindings_for_input(&[Keystroke::parse("tab").unwrap()], &[root, terminal]);

    assert!(!pending);
    assert_eq!(
        bindings[0].action().as_any().type_id(),
        TypeId::of::<TerminalTab>()
    );
}

#[test]
fn tracks_bracketed_paste_mode_from_output() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert!(!session.bracketed_paste_enabled());

    session.feed(b"\x1b[?2004h").unwrap();
    assert!(session.bracketed_paste_enabled());

    session.feed(b"\x1b[?2004l").unwrap();
    assert!(!session.bracketed_paste_enabled());
}

#[test]
fn tracks_cursor_visibility_from_output() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert!(session.cursor_visible());

    session.feed(b"\x1b[?25l").unwrap();
    assert!(!session.cursor_visible());

    session.feed(b"\x1b[?25h").unwrap();
    assert!(session.cursor_visible());
}

#[test]
fn tracks_cursor_visibility_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b[?2").unwrap();
    assert!(session.cursor_visible());

    session.feed(b"5l").unwrap();
    assert!(!session.cursor_visible());

    session.feed(b"\x1b[?25").unwrap();
    assert!(!session.cursor_visible());

    session.feed(b"h").unwrap();
    assert!(session.cursor_visible());
}

#[test]
fn tracks_synchronized_output_mode_from_output() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert!(!session.synchronized_output_active());

    session.feed(b"\x1b[?2026h").unwrap();
    assert!(session.synchronized_output_active());

    session.feed(b"\x1b[?2026l").unwrap();
    assert!(!session.synchronized_output_active());
}

#[test]
fn tracks_synchronized_output_mode_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();

    session.feed(b"\x1b[?20").unwrap();
    assert!(!session.synchronized_output_active());

    session.feed(b"26h").unwrap();
    assert!(session.synchronized_output_active());

    session.feed(b"\x1b[?202").unwrap();
    assert!(session.synchronized_output_active());

    session.feed(b"6l").unwrap();
    assert!(!session.synchronized_output_active());
}

#[test]
fn tracks_mouse_reporting_mode_from_output() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert!(!session.mouse_reporting_enabled());
    assert!(!session.mouse_sgr_enabled());

    session.feed(b"\x1b[?1000;1006h").unwrap();
    assert!(session.mouse_reporting_enabled());
    assert!(session.mouse_sgr_enabled());

    session.feed(b"\x1b[?1000l").unwrap();
    assert!(!session.mouse_reporting_enabled());
    assert!(session.mouse_sgr_enabled());

    session.feed(b"\x1b[?1006l").unwrap();
    assert!(!session.mouse_sgr_enabled());
}

#[test]
fn viewport_index_maps_row_and_column_to_byte_index() {
    let viewport = "abc\ndef";
    assert_eq!(viewport_index_for_cell(viewport, 1, 1), 0);
    assert_eq!(viewport_index_for_cell(viewport, 1, 2), 1);
    assert_eq!(viewport_index_for_cell(viewport, 1, 4), 3);
    assert_eq!(viewport_index_for_cell(viewport, 2, 1), 4);
    assert_eq!(viewport_index_for_cell(viewport, 2, 3), 6);
}

#[test]
fn viewport_index_accounts_for_wide_characters() {
    let viewport = "Ｗa\n";
    assert_eq!(viewport_index_for_cell(viewport, 1, 1), 0);
    assert_eq!(viewport_index_for_cell(viewport, 1, 2), 0);
    assert_eq!(viewport_index_for_cell(viewport, 1, 3), "Ｗ".len());
    assert_eq!(viewport_index_for_cell(viewport, 1, 4), "Ｗ".len() + 1);
}

#[test]
fn tracks_modes_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b[?1000;").unwrap();
    assert!(!session.mouse_reporting_enabled());

    session.feed(b"1006h").unwrap();
    assert!(session.mouse_reporting_enabled());
    assert!(session.mouse_sgr_enabled());
}

#[test]
fn tracks_osc_title_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b]0;hi").unwrap();
    assert!(session.title().is_none());

    session.feed(b"\x07").unwrap();
    assert_eq!(session.title(), Some("hi"));
}

#[test]
fn tracks_osc_52_clipboard_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b]52;c;").unwrap();
    assert!(session.take_clipboard_write().is_none());

    session.feed(b"aGk=\x07").unwrap();
    assert_eq!(session.take_clipboard_write().as_deref(), Some("hi"));
}

#[test]
fn responds_to_csi_6n_cursor_position_request() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"hi\x1b[6n", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    assert_eq!(response, b"\x1b[1;3R");
}

#[test]
fn responds_to_csi_6n_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"hi\x1b[", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();
    assert!(response.is_empty());

    session
        .feed_with_pty_responses(b"6n", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    assert_eq!(response, b"\x1b[1;3R");
}

#[test]
fn responds_to_csi_5n_device_status_request() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b[5n", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    assert_eq!(response, b"\x1b[0n");
}

#[test]
fn responds_to_csi_5n_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b[", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();
    assert!(response.is_empty());

    session
        .feed_with_pty_responses(b"5n", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    assert_eq!(response, b"\x1b[0n");
}

#[test]
fn responds_to_osc_10_default_foreground_color_query() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]10;?\x1b\\", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    let expected = osc_color_response(10, (0xFF, 0xFF, 0xFF));
    assert_eq!(response, expected.as_bytes());
}

#[test]
fn responds_to_osc_11_default_background_color_query() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]11;?\x1b\\", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    let expected = osc_color_response(11, (0x00, 0x00, 0x00));
    assert_eq!(response, expected.as_bytes());
}

#[test]
fn responds_to_osc_10_and_11_use_configured_defaults() {
    let config = TerminalConfig {
        default_fg: ghostty_vt::Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        },
        default_bg: ghostty_vt::Rgb {
            r: 0x44,
            g: 0x55,
            b: 0x66,
        },
        ..TerminalConfig::default()
    };
    let mut session = TerminalSession::new(config).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    let expected_fg = osc_color_response(10, (0x11, 0x22, 0x33));
    let expected_bg = osc_color_response(11, (0x44, 0x55, 0x66));
    let mut expected = Vec::new();
    expected.extend_from_slice(expected_fg.as_bytes());
    expected.extend_from_slice(expected_bg.as_bytes());
    assert_eq!(response, expected);
}

#[test]
fn responds_to_osc_11_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]11;?\x1b", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();
    assert!(response.is_empty());

    session
        .feed_with_pty_responses(b"\\", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    let expected = osc_color_response(11, (0x00, 0x00, 0x00));
    assert_eq!(response, expected.as_bytes());
}

#[test]
fn responds_to_osc_11_query_terminated_by_bel() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]11;?\x07", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    let expected = osc_color_response(11, (0x00, 0x00, 0x00));
    assert_eq!(response, expected.as_bytes());
}

#[test]
fn responds_to_osc_12_cursor_color_query_with_foreground_when_unset() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]12;?\x1b\\", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    // No explicit cursor colour → the cursor is drawn in the foreground
    // colour, so that is what a query learns (ghostty does the same).
    let expected = osc_color_response(12, (0xFF, 0xFF, 0xFF));
    assert_eq!(response, expected.as_bytes());
}

#[test]
fn responds_to_osc_12_cursor_color_query_with_configured_cursor_color() {
    let config = TerminalConfig {
        cursor_color: Some(ghostty_vt::Rgb {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        }),
        ..TerminalConfig::default()
    };
    let mut session = TerminalSession::new(config).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(b"\x1b]12;?\x07", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();

    let expected = osc_color_response(12, (0x11, 0x22, 0x33));
    assert_eq!(response, expected.as_bytes());
}

#[test]
fn responds_to_primary_device_attributes_query() {
    for query in [&b"\x1b[c"[..], &b"\x1b[0c"[..]] {
        let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
        let mut response = Vec::new();

        session
            .feed_with_pty_responses(query, |bytes| {
                response.extend_from_slice(bytes);
            })
            .unwrap();

        // Same identity zmx reports on our behalf while no client is
        // attached, so a program sees one terminal whichever side answers.
        assert_eq!(response, b"\x1b[?62;22c", "query {:?}", query);
    }
}

#[test]
fn responds_to_secondary_device_attributes_query() {
    for query in [&b"\x1b[>c"[..], &b"\x1b[>0c"[..]] {
        let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
        let mut response = Vec::new();

        session
            .feed_with_pty_responses(query, |bytes| {
                response.extend_from_slice(bytes);
            })
            .unwrap();

        assert_eq!(response, b"\x1b[>1;10;0c", "query {:?}", query);
    }
}

#[test]
fn device_attributes_responses_and_multi_param_csis_are_not_queries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    session
        .feed_with_pty_responses(
            // A DA1 response echoed back, a DA2 response, a CSI with a
            // non-zero parameter, a multi-parameter DSR-shaped sequence and
            // a CSI 996 without the private marker: none is a query we answer.
            b"\x1b[?62;22c\x1b[>1;10;0c\x1b[1c\x1b[?1;996n\x1b[996n",
            |bytes| {
                response.extend_from_slice(bytes);
            },
        )
        .unwrap();

    assert!(response.is_empty(), "unexpected response {:?}", response);
}

#[test]
fn responds_to_dsr_996_color_scheme_query() {
    let mut dark = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();
    dark.feed_with_pty_responses(b"\x1b[?996n", |bytes| {
        response.extend_from_slice(bytes);
    })
    .unwrap();
    assert_eq!(response, b"\x1b[?997;1n");

    let mut light = TerminalSession::new(TerminalConfig {
        color_scheme: ColorScheme::Light,
        ..TerminalConfig::default()
    })
    .unwrap();
    let mut response = Vec::new();
    light
        .feed_with_pty_responses(b"\x1b[?996n", |bytes| {
            response.extend_from_slice(bytes);
        })
        .unwrap();
    assert_eq!(response, b"\x1b[?997;2n");
}

#[test]
fn responds_to_dsr_996_across_chunk_boundaries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut response = Vec::new();

    for chunk in [&b"\x1b["[..], &b"?9"[..], &b"96"[..], &b"n"[..]] {
        session
            .feed_with_pty_responses(chunk, |bytes| {
                response.extend_from_slice(bytes);
            })
            .unwrap();
    }

    assert_eq!(response, b"\x1b[?997;1n");
}

#[test]
fn color_scheme_change_is_reported_only_while_mode_2031_is_set() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert_eq!(session.color_scheme(), ColorScheme::Dark);

    // Mode off: a change is remembered but nothing is sent.
    assert_eq!(session.set_color_scheme(ColorScheme::Light), None);
    assert_eq!(session.color_scheme(), ColorScheme::Light);

    session.feed(b"\x1b[?2031h").unwrap();
    assert!(session.color_scheme_reports_enabled());

    // Same scheme again: no change, no report.
    assert_eq!(session.set_color_scheme(ColorScheme::Light), None);
    assert_eq!(
        session.set_color_scheme(ColorScheme::Dark),
        Some(&b"\x1b[?997;1n"[..])
    );
    assert_eq!(
        session.set_color_scheme(ColorScheme::Light),
        Some(&b"\x1b[?997;2n"[..])
    );

    session.feed(b"\x1b[?2031l").unwrap();
    assert!(!session.color_scheme_reports_enabled());
    assert_eq!(session.set_color_scheme(ColorScheme::Dark), None);
}

#[test]
fn mode_2031_is_tracked_inside_a_multi_mode_decset() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b[?1049;2031h").unwrap();
    assert!(session.color_scheme_reports_enabled());
    assert!(session.alternate_screen_active());
}

fn replies(session: &mut TerminalSession, bytes: &[u8]) -> Vec<u8> {
    let mut response = Vec::new();
    session
        .feed_with_pty_responses(bytes, |b| response.extend_from_slice(b))
        .unwrap();
    response
}

#[test]
fn esc_restarts_a_half_parsed_csi_query() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert_eq!(
        replies(&mut session, b"\x1b[?99\x1b[?996n"),
        b"\x1b[?997;1n"
    );
}

#[test]
fn intermediate_or_unknown_bytes_abort_a_csi_query() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    // Space and `$` are intermediates, `<` is not a marker we know, a `?`
    // after a digit is out of place, `?c` / `?0c` and `>1c` are not DA queries.
    let out = replies(
        &mut session,
        b"\x1b[?996 n\x1b[5$n\x1b[<0c\x1b[5?6n\x1b[?c\x1b[?0c\x1b[>1c",
    );
    assert!(out.is_empty(), "unexpected response {:?}", out);
}

#[test]
fn adjacent_osc_and_csi_queries_are_answered_in_order() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    let mut expected = osc_color_response(11, (0x00, 0x00, 0x00)).into_bytes();
    expected.extend_from_slice(b"\x1b[?62;22c");
    assert_eq!(replies(&mut session, b"\x1b]11;?\x07\x1b[c"), expected);
}

#[test]
fn osc_13_query_is_not_answered() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    assert!(replies(&mut session, b"\x1b]13;?\x07").is_empty());
}

#[test]
fn ris_clears_mode_2031_and_the_other_mirrored_modes() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b[?2031h\x1b[?2004h\x1b[?1049h").unwrap();
    assert!(session.color_scheme_reports_enabled());
    assert!(session.bracketed_paste_enabled());
    assert!(session.alternate_screen_active());

    session.feed(b"\x1bc").unwrap();
    assert!(!session.color_scheme_reports_enabled());
    assert!(!session.bracketed_paste_enabled());
    assert!(!session.alternate_screen_active());
    // No program asked any more: a theme switch writes nothing to the pty.
    assert_eq!(session.set_color_scheme(ColorScheme::Light), None);
}

#[test]
fn a_mode_set_after_ris_in_the_same_chunk_survives_it() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b[?2031h\x1bc\x1b[?2031h").unwrap();
    assert!(session.color_scheme_reports_enabled());
    // …and re-scanning the carried tail on the next feed keeps that order.
    session.feed(b"x").unwrap();
    assert!(session.color_scheme_reports_enabled());
}

#[test]
fn full_reset_clears_mode_2031() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    session.feed(b"\x1b[?2031h").unwrap();
    session.full_reset();
    assert!(!session.color_scheme_reports_enabled());
    assert_eq!(session.set_color_scheme(ColorScheme::Light), None);
}

#[test]
fn reset_for_new_pty_forgets_modes_and_half_parsed_queries() {
    let mut session = TerminalSession::new(TerminalConfig::default()).unwrap();
    // The dead client's last bytes: a mode and the head of a DSR.
    assert!(replies(&mut session, b"\x1b[?2031h\x1b[?99").is_empty());
    session.reset_for_new_pty();
    assert!(!session.color_scheme_reports_enabled());
    // The new shell's first bytes must not complete the old query.
    assert!(replies(&mut session, b"6n").is_empty());
    // A fresh, complete query still works.
    assert_eq!(replies(&mut session, b"\x1b[?996n"), b"\x1b[?997;1n");
}

#[test]
fn sgr_mouse_encoding_helpers_match_expected_format() {
    assert_eq!(
        crate::view::sgr_mouse_button_value(0, false, false, false, false),
        0
    );
    assert_eq!(
        crate::view::sgr_mouse_button_value(2, true, false, true, true),
        2 + 32 + 8 + 16
    );
    assert_eq!(
        crate::view::sgr_mouse_sequence(0, 1, 1, true),
        "\u{1b}[<0;1;1M"
    );
    assert_eq!(
        crate::view::sgr_mouse_sequence(0, 1, 1, false),
        "\u{1b}[<0;1;1m"
    );
}

#[test]
fn ctrl_c_encodes_to_etx_even_without_key_char() {
    let ctrl_c = Keystroke::parse("ctrl-c").unwrap();
    assert_eq!(crate::view::ctrl_byte_for_keystroke(&ctrl_c), Some(0x03));
}

#[test]
fn cmd_arrows_and_backspace_map_to_emacs_line_editing_bytes() {
    // Cmd+Left / Cmd+Right jump to the ends of the line, as everywhere else
    // on macOS; the shell only understands the readline control bytes.
    assert_eq!(crate::view::cmd_line_editing_byte("left"), Some(0x01));
    assert_eq!(crate::view::cmd_line_editing_byte("right"), Some(0x05));
    assert_eq!(crate::view::cmd_line_editing_byte("backspace"), Some(0x15));

    // Everything else keeps travelling as a Cmd shortcut, not as input.
    assert_eq!(crate::view::cmd_line_editing_byte("up"), None);
    assert_eq!(crate::view::cmd_line_editing_byte("down"), None);
    assert_eq!(crate::view::cmd_line_editing_byte("a"), None);
    assert_eq!(crate::view::cmd_line_editing_byte("delete"), None);
}

#[test]
fn does_not_skip_enter_key_when_ime_in_progress() {
    let enter = Keystroke::parse("enter").unwrap();
    assert!(enter.is_ime_in_progress());
    assert!(!crate::view::should_skip_key_down_for_ime(true, &enter));

    let letter = Keystroke::parse("a").unwrap();
    assert!(letter.is_ime_in_progress());
    assert!(crate::view::should_skip_key_down_for_ime(true, &letter));

    let committed = Keystroke::parse("a->a").unwrap();
    assert!(!committed.is_ime_in_progress());
    assert!(!crate::view::should_skip_key_down_for_ime(true, &committed));
}

#[test]
fn byte_index_for_column_in_line_handles_wide_characters() {
    assert_eq!(crate::view::byte_index_for_column_in_line("Ｗa", 1), 0);
    assert_eq!(crate::view::byte_index_for_column_in_line("Ｗa", 2), 0);
    assert_eq!(
        crate::view::byte_index_for_column_in_line("Ｗa", 3),
        "Ｗ".len()
    );
    assert_eq!(
        crate::view::byte_index_for_column_in_line("Ｗa", 4),
        "Ｗ".len() + 1
    );
}

#[test]
fn maps_common_box_drawing_glyphs() {
    for ch in ['─', '│', '┌', '┐', '└', '┘', '├', '┤', '┬', '┴', '┼'] {
        assert!(
            crate::view::box_drawing_mask(ch).is_some(),
            "expected mask for {ch}"
        );
    }
    assert!(crate::view::box_drawing_mask('X').is_none());
}

#[test]
fn scrolling_bottom_margin_preserves_footer_rows() {
    let config = TerminalConfig {
        cols: 40,
        rows: 30,
        ..TerminalConfig::default()
    };
    let mut session = TerminalSession::new(config).unwrap();

    session.feed(b"\x1b[24;1HROW24").unwrap();
    session.feed(b"\x1b[25;1HROW25-FOOTER").unwrap();
    session.feed(b"\x1b[26;1HROW26").unwrap();

    session.feed(b"\x1b[1;23r").unwrap();
    session.feed(b"\x1b[23;1H").unwrap();
    session.feed(b"TOP-LINE\r\n").unwrap();

    let row24 = session.dump_viewport_row(23).unwrap();
    let row25 = session.dump_viewport_row(24).unwrap();
    let row26 = session.dump_viewport_row(25).unwrap();

    assert!(row24.starts_with("ROW24"), "row24={row24:?}");
    assert!(row25.starts_with("ROW25-FOOTER"), "row25={row25:?}");
    assert!(row26.starts_with("ROW26"), "row26={row26:?}");
}

#[test]
fn multi_line_write_stays_inside_scroll_region() {
    let config = TerminalConfig {
        cols: 40,
        rows: 30,
        ..TerminalConfig::default()
    };
    let mut session = TerminalSession::new(config).unwrap();

    session.feed(b"\x1b[24;1HFOOT24").unwrap();
    session.feed(b"\x1b[25;1HFOOT25").unwrap();
    session.feed(b"\x1b[26;1HFOOT26").unwrap();

    session.feed(b"\x1b[1;23r\x1b[23;1H\r\n\x1b[K\r\n").unwrap();
    session.feed(b"LINE1\r\nLINE2\r\nLINE3").unwrap();

    let row23 = session.dump_viewport_row(22).unwrap();
    let row24 = session.dump_viewport_row(23).unwrap();
    let row25 = session.dump_viewport_row(24).unwrap();
    let row26 = session.dump_viewport_row(25).unwrap();

    assert!(row23.starts_with("LINE3"), "row23={row23:?}");
    assert!(row24.starts_with("FOOT24"), "row24={row24:?}");
    assert!(row25.starts_with("FOOT25"), "row25={row25:?}");
    assert!(row26.starts_with("FOOT26"), "row26={row26:?}");
}

#[test]
fn codex_scroll_region_reverse_index_keeps_footer_rows_intact() {
    let config = TerminalConfig {
        cols: 40,
        rows: 12,
        ..TerminalConfig::default()
    };
    let mut session = TerminalSession::new(config).unwrap();

    session.feed(b"\x1b[5;1HBOX5").unwrap();
    session.feed(b"\x1b[6;1HBOX6").unwrap();
    session.feed(b"\x1b[7;1HBOX7").unwrap();
    session.feed(b"\x1b[8;1HBOX8").unwrap();
    session.feed(b"\x1b[9;1HFOOT9").unwrap();
    session.feed(b"\x1b[10;1HFOOT10").unwrap();

    session
        .feed(b"\x1b[?2026h\x1b[5;8r\x1b[5;1H\x1bM\x1bM\x1b[r\x1b[?2026l")
        .unwrap();

    let row5 = session.dump_viewport_row(4).unwrap();
    let row6 = session.dump_viewport_row(5).unwrap();
    let row7 = session.dump_viewport_row(6).unwrap();
    let row8 = session.dump_viewport_row(7).unwrap();
    let row9 = session.dump_viewport_row(8).unwrap();
    let row10 = session.dump_viewport_row(9).unwrap();

    assert_eq!(row5.trim_end(), "", "row5={row5:?}");
    assert_eq!(row6.trim_end(), "", "row6={row6:?}");
    assert!(row7.starts_with("BOX5"), "row7={row7:?}");
    assert!(row8.starts_with("BOX6"), "row8={row8:?}");
    assert!(row9.starts_with("FOOT9"), "row9={row9:?}");
    assert!(row10.starts_with("FOOT10"), "row10={row10:?}");
}

#[test]
fn insert_blanks_shifts_content_without_touching_footer_rows() {
    let config = TerminalConfig {
        cols: 20,
        rows: 8,
        ..TerminalConfig::default()
    };
    let mut session = TerminalSession::new(config).unwrap();

    session.feed(b"\x1b[3;1HABCDE").unwrap();
    session.feed(b"\x1b[7;1HFOOT7").unwrap();
    session.feed(b"\x1b[8;1HFOOT8").unwrap();

    session.feed(b"\x1b[3;2H\x1b[2@").unwrap();

    let row3 = session.dump_viewport_row(2).unwrap();
    let row7 = session.dump_viewport_row(6).unwrap();
    let row8 = session.dump_viewport_row(7).unwrap();

    assert!(row3.starts_with("A  BC"), "row3={row3:?}");
    assert!(row7.starts_with("FOOT7"), "row7={row7:?}");
    assert!(row8.starts_with("FOOT8"), "row8={row8:?}");
}

#[test]
fn url_at_cell_single_line() {
    use crate::view::url_at_cell_in_wrapped_lines;

    let lines = vec!["see https://example.com/docs for info".to_string()];
    // Click inside the URL (col is 1-based).
    assert_eq!(
        url_at_cell_in_wrapped_lines(&lines, 80, 0, 10),
        Some("https://example.com/docs".to_string())
    );
    // Click outside the URL.
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, 80, 0, 2), None);
}

#[test]
fn url_at_cell_joins_wrapped_rows() {
    use crate::view::url_at_cell_in_wrapped_lines;

    // 20-col terminal; URL wraps across three rows. Rows 0 and 1 are
    // exactly full width → treated as soft-wrapped continuations.
    let cols = 20;
    let lines = vec![
        "see https://example.".to_string(),
        "com/a/very/long/path".to_string(),
        "?q=1 and more text".to_string(),
    ];
    let expected = Some("https://example.com/a/very/long/path?q=1".to_string());

    // Click on first row inside URL.
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, cols, 0, 8), expected);
    // Click on middle row — must walk back to the URL start.
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, cols, 1, 5), expected);
    // Click on last row inside the URL tail.
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, cols, 2, 2), expected);
    // Click past the URL on the last row.
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, cols, 2, 10), None);
}

#[test]
fn url_at_cell_does_not_join_short_rows() {
    use crate::view::url_at_cell_in_wrapped_lines;

    // Previous row is NOT full width → no continuation, rows independent.
    let lines = vec!["https://example.com".to_string(), "unrelated".to_string()];
    assert_eq!(
        url_at_cell_in_wrapped_lines(&lines, 40, 0, 3),
        Some("https://example.com".to_string())
    );
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, 40, 1, 3), None);
}

#[test]
fn url_at_cell_strips_trailing_punctuation() {
    use crate::view::url_at_cell_in_wrapped_lines;

    let lines = vec!["(https://example.com/x).".to_string()];
    assert_eq!(
        url_at_cell_in_wrapped_lines(&lines, 80, 0, 5),
        Some("https://example.com/x".to_string())
    );
}

#[test]
fn url_spans_cover_each_wrapped_row() {
    use crate::view::url_spans_at_cell_in_wrapped_lines;

    let cols = 20;
    let lines = vec![
        "see https://example.".to_string(),
        "com/a/very/long/path".to_string(),
        "?q=1 and more text".to_string(),
    ];
    let (url, spans) = url_spans_at_cell_in_wrapped_lines(&lines, cols, 1, 5).unwrap();
    assert_eq!(url, "https://example.com/a/very/long/path?q=1");
    assert_eq!(
        spans,
        vec![(0usize, 4..20), (1usize, 0..20), (2usize, 0..4)]
    );
}

#[test]
fn osc8_link_spans_expand_over_link_cells() {
    use crate::view::osc8_link_spans;

    // 20-col, 3-row screen. "Docs Page" link occupies row 2, cols 6..=14.
    let lines = vec![
        "header text".to_string(),
        "see  Docs Page  end".to_string(),
        "footer".to_string(),
    ];
    let link = |c: u16, r: u16| {
        (r == 2 && (6..=14).contains(&c)).then(|| "https://example.com/docs".to_string())
    };

    let (url, spans) = osc8_link_spans(link, &lines, 20, 3, 9, 2).unwrap();
    assert_eq!(url, "https://example.com/docs");
    assert_eq!(spans, vec![(1usize, 5..14)]);

    // Hover outside the link region → no spans.
    assert!(osc8_link_spans(link, &lines, 20, 3, 2, 2).is_none());
}

#[test]
fn osc8_link_spans_join_wrapped_rows() {
    use crate::view::osc8_link_spans;

    // Link wraps: row 1 cols 15..=20, row 2 cols 1..=8.
    let lines = vec![
        "intro and link starts".to_string(),
        "and ends here padded".to_string(),
    ];
    let link = |c: u16, r: u16| {
        let hit = (r == 1 && (15..=20).contains(&c)) || (r == 2 && (1..=8).contains(&c));
        hit.then(|| "https://example.com/wrapped".to_string())
    };

    // Hover on the second row's segment.
    let (url, spans) = osc8_link_spans(link, &lines, 20, 2, 3, 2).unwrap();
    assert_eq!(url, "https://example.com/wrapped");
    assert_eq!(spans, vec![(0usize, 14..21), (1usize, 0..8)]);
}

#[test]
fn url_detected_inside_tool_call_prefix() {
    use crate::view::url_at_cell_in_wrapped_lines;

    let lines = vec!["Fetch(https://amplitude.com/docs/data/persisted-properties)".to_string()];
    // Click inside the URL (col 10 = byte 9, the "s" of https).
    assert_eq!(
        url_at_cell_in_wrapped_lines(&lines, 120, 0, 10),
        Some("https://amplitude.com/docs/data/persisted-properties".to_string())
    );
    // Click on the "Fetch(" prefix → not a link.
    assert_eq!(url_at_cell_in_wrapped_lines(&lines, 120, 0, 2), None);

    // Same shape but wrapped across two rows at 30 cols.
    let cols = 30;
    let wrapped = vec![
        "Fetch(https://amplitude.com/do".to_string(),
        "cs/data/persisted-properties)".to_string(),
    ];
    let expected = Some("https://amplitude.com/docs/data/persisted-properties".to_string());
    assert_eq!(
        url_at_cell_in_wrapped_lines(&wrapped, cols, 0, 12),
        expected
    );
    assert_eq!(url_at_cell_in_wrapped_lines(&wrapped, cols, 1, 5), expected);
}
