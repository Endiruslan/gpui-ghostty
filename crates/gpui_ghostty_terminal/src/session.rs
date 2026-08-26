use ghostty_vt::{Error, Rgb, Terminal};

use crate::{ColorScheme, TerminalConfig};

pub struct TerminalSession {
    config: TerminalConfig,
    terminal: Terminal,
    bracketed_paste_enabled: bool,
    cursor_visible: bool,
    synchronized_output_active: bool,
    mouse_x10_enabled: bool,
    mouse_button_event_enabled: bool,
    mouse_any_event_enabled: bool,
    mouse_sgr_enabled: bool,
    /// Alternate screen buffer active (DEC modes 47 / 1047 / 1049). Set by
    /// fullscreen TUIs (vim, htop, Claude Code `/tui fullscreen`). The alt
    /// screen has no scrollback, so local scrollback scrolling must be
    /// suppressed while it's active.
    alternate_screen_active: bool,
    /// Cursor position (1-based `(col, row)`) captured the moment
    /// [`ghostty_vt::TerminalEvent::InputStart`] (OSC 133;B) is drained —
    /// the cursor sits right after the shell's prompt, at the start of
    /// whatever the user is about to type. Cleared on `CommandStart`,
    /// `CommandEnd` (both OSC 133), alt-screen entry, resize, and hard
    /// reset — all cases where the anchor no longer describes "where input
    /// begins" for the current line. Consumed by prefix-extraction for
    /// terminal history autosuggest.
    input_anchor: Option<(u16, u16)>,
    /// DECCKM (mode 1): cursor keys send SS3 (`ESC O A`) instead of CSI
    /// (`ESC [ A`). Needed to emit the right arrow-key encoding for
    /// alternate-scroll wheel translation.
    application_cursor_keys: bool,
    /// Alternate scroll mode (DEC mode 1007). When the alt screen is active
    /// and mouse reporting is off, the mouse wheel is translated to cursor
    /// keys. Default ON, matching xterm / Ghostty.
    mouse_alternate_scroll: bool,
    title: Option<String>,
    clipboard_write: Option<String>,
    /// DEC mode 2031: the program asked to be told, as `CSI ? 997 ; Ps n`,
    /// whenever the colour scheme changes (Claude Code parses that report;
    /// neovim and helix set the mode).
    report_color_scheme: bool,
    parse_tail: Vec<u8>,
    csi_query_state: CsiQueryScanState,
    osc_query_state: OscQueryScanState,
    transparent_default_bgs: Vec<Rgb>,
}

impl TerminalSession {
    pub fn new(config: TerminalConfig) -> Result<Self, Error> {
        let mut terminal = Terminal::new(config.cols, config.rows)?;
        terminal.set_default_colors(config.default_fg, config.default_bg);
        let initial_bg = config.default_bg;
        Ok(Self {
            config,
            terminal,
            bracketed_paste_enabled: false,
            cursor_visible: true,
            synchronized_output_active: false,
            mouse_x10_enabled: false,
            mouse_button_event_enabled: false,
            mouse_any_event_enabled: false,
            mouse_sgr_enabled: false,
            alternate_screen_active: false,
            input_anchor: None,
            application_cursor_keys: false,
            mouse_alternate_scroll: true,
            title: None,
            clipboard_write: None,
            report_color_scheme: false,
            parse_tail: Vec::new(),
            csi_query_state: CsiQueryScanState::default(),
            osc_query_state: OscQueryScanState::default(),
            transparent_default_bgs: vec![initial_bg],
        })
    }

    pub fn cols(&self) -> u16 {
        self.config.cols
    }

    pub fn rows(&self) -> u16 {
        self.config.rows
    }

    pub fn default_foreground(&self) -> Rgb {
        self.config.default_fg
    }

    pub fn default_background(&self) -> Rgb {
        self.config.default_bg
    }

    /// Alpha applied to the full default-background fill (see
    /// [`TerminalConfig::background_alpha`]).
    pub fn background_alpha(&self) -> f32 {
        self.config.background_alpha
    }

    /// Set the default-background fill alpha at runtime (e.g. when the host
    /// window toggles between opaque and translucent). Triggers no terminal
    /// reset; the next paint picks it up.
    pub fn set_background_alpha(&mut self, alpha: f32) {
        self.config.background_alpha = alpha.clamp(0.0, 1.0);
    }

    /// Background RGBs that represented the terminal default background at some
    /// point during this session. When the host makes the default background
    /// transparent, existing cells may still snapshot an older default as a raw
    /// RGB style run; those should also be treated as transparent.
    pub fn transparent_default_backgrounds(&self) -> &[Rgb] {
        &self.transparent_default_bgs
    }

    /// Swap the terminal's default foreground and background colors at runtime.
    ///
    /// Affects every subsequently-rendered cell that uses the "default"
    /// palette entry — i.e. any cell not carrying an explicit SGR color.
    /// The config copy is also updated so callers can read the new values via
    /// [`Self::default_foreground`] / [`Self::default_background`].
    ///
    /// Use this to react to host-application theme changes without
    /// reconstructing the `TerminalSession` (which would lose scrollback and
    /// reset the cursor).
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        self.remember_transparent_default_bg(self.config.default_bg);
        self.remember_transparent_default_bg(bg);
        self.config.default_fg = fg;
        self.config.default_bg = bg;
        self.terminal.set_default_colors(fg, bg);
    }

    fn remember_transparent_default_bg(&mut self, bg: Rgb) {
        if self.transparent_default_bgs.contains(&bg) {
            return;
        }
        self.transparent_default_bgs.push(bg);
        if self.transparent_default_bgs.len() > 8 {
            self.transparent_default_bgs.remove(0);
        }
    }

    /// Override font size in pixels. `None` = inherit from `window.text_style()`.
    pub fn font_size(&self) -> Option<f32> {
        self.config.font_size
    }

    /// Update the font-size override at runtime. The next render frame
    /// will re-shape with the new size and the host should call
    /// `resize_terminal` afterwards because cell metrics changed.
    pub fn set_font_size(&mut self, size: Option<f32>) {
        self.config.font_size = size;
    }

    /// Override line height as multiplier of font size. `None` = inherit.
    pub fn line_height_ratio(&self) -> Option<f32> {
        self.config.line_height_ratio
    }

    /// Primary font family override. `None` = `default_terminal_font()`.
    pub fn font_family(&self) -> Option<&str> {
        self.config.font_family.as_deref()
    }

    /// Update the primary font family override at runtime. `None` =
    /// `default_terminal_font()`. The host should rebuild the base font and
    /// invalidate any shaped-line cache after calling this.
    pub fn set_font_family(&mut self, family: Option<String>) {
        self.config.font_family = family;
    }

    /// Primary font weight (CSS numeric scale). `None` = Normal.
    pub fn font_weight(&self) -> Option<f32> {
        self.config.font_weight
    }

    /// Update the primary font weight override at runtime. `None` = Normal.
    /// The host should rebuild the base font and invalidate any shaped-line
    /// cache after calling this.
    pub fn set_font_weight(&mut self, weight: Option<f32>) {
        self.config.font_weight = weight;
    }

    /// Cursor blink interval in ms. `None` = no blink.
    pub fn cursor_blink_ms(&self) -> Option<u64> {
        self.config.cursor_blink_ms
    }

    /// Override color for the block/bar cursor.  `None` = auto-contrast.
    pub fn cursor_color(&self) -> Option<Rgb> {
        self.config.cursor_color
    }

    /// Cursor shape (Block vs Bar).
    pub fn cursor_style(&self) -> crate::CursorStyle {
        self.config.cursor_style
    }

    /// Update the cursor color at runtime (Theme switches, accent changes).
    pub fn set_cursor_color(&mut self, color: Option<Rgb>) {
        self.config.cursor_color = color;
    }

    /// Update the cursor shape at runtime.
    pub fn set_cursor_style(&mut self, style: crate::CursorStyle) {
        self.config.cursor_style = style;
    }

    /// Which colour scheme the host theme is, as far as the terminal knows.
    pub fn color_scheme(&self) -> ColorScheme {
        self.config.color_scheme
    }

    /// Whether the program set DEC mode 2031 (colour-scheme change reports).
    pub fn color_scheme_reports_enabled(&self) -> bool {
        self.report_color_scheme
    }

    /// Tell the terminal which colour scheme the host theme is now.
    ///
    /// Returns the DEC mode 2031 report (`CSI ? 997 ; Ps n`) to write to the
    /// pty when the scheme actually changed *and* the program asked to be told;
    /// the caller owns the pty, so it does the write. `None` otherwise — a
    /// repeat of the current scheme is not a change, and a program that never
    /// set the mode must not receive unsolicited bytes.
    pub fn set_color_scheme(&mut self, scheme: ColorScheme) -> Option<&'static [u8]> {
        if self.config.color_scheme == scheme {
            return None;
        }
        self.config.color_scheme = scheme;
        self.report_color_scheme.then(|| scheme.report())
    }

    pub fn bracketed_paste_enabled(&self) -> bool {
        self.bracketed_paste_enabled
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    pub fn synchronized_output_active(&self) -> bool {
        self.synchronized_output_active
    }

    pub fn mouse_reporting_enabled(&self) -> bool {
        self.mouse_x10_enabled || self.mouse_button_event_enabled || self.mouse_any_event_enabled
    }

    pub fn mouse_sgr_enabled(&self) -> bool {
        self.mouse_sgr_enabled
    }

    /// Whether the alternate screen buffer is active (fullscreen TUI). While
    /// active there is no scrollback, so the view must not run its local
    /// pixel-smooth scrollback scroll.
    pub fn alternate_screen_active(&self) -> bool {
        self.alternate_screen_active
    }

    /// DECCKM state — `true` means cursor keys are encoded as SS3.
    pub fn application_cursor_keys(&self) -> bool {
        self.application_cursor_keys
    }

    /// Alternate scroll mode (DEC 1007). When set, wheel events on the alt
    /// screen translate to cursor keys. Defaults to `true`.
    pub fn mouse_alternate_scroll_enabled(&self) -> bool {
        self.mouse_alternate_scroll
    }

    pub fn mouse_button_event_enabled(&self) -> bool {
        self.mouse_button_event_enabled
    }

    pub fn mouse_any_event_enabled(&self) -> bool {
        self.mouse_any_event_enabled
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    pub(crate) fn window_title_updates_enabled(&self) -> bool {
        self.config.update_window_title
    }

    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<String> {
        self.terminal.hyperlink_at(col, row)
    }

    pub fn take_clipboard_write(&mut self) -> Option<String> {
        self.clipboard_write.take()
    }

    fn update_state_from_output(&mut self, bytes: &[u8]) {
        /// How much of the *scanned* stream is carried into the next call, so a
        /// sequence split across two PTY reads is still seen whole. Only a
        /// carry — never a limit on what gets scanned (see below).
        const CARRY_LIMIT: usize = 2048;

        // Everything new is scanned, however large the batch. Truncating the
        // buffer *before* the scan (as this did until 2026-07-28) silently
        // dropped the head of any batch over `CARRY_LIMIT`, and the modes that
        // matter arrive exactly there: a `zmx` re-attach replays the session in
        // one ~130 KB write whose first bytes are `?1000h ?1002h ?1006h
        // ?1049h`. The C terminal still got every byte, so the screen looked
        // right while `alternate_screen_active` / `mouse_*` stayed false — the
        // wheel then scrolled a scrollback the fullscreen app does not have,
        // and mxds's terminal appeared to ignore scrolling until the pane was
        // resized (which made the app redraw in batches small enough to fit
        // the window). OSC titles and OSC 52 clipboard writes were lost the
        // same way.
        self.parse_tail.extend_from_slice(bytes);
        // Taken, not borrowed: RIS below resets flags on `self` mid-scan.
        let tail = std::mem::take(&mut self.parse_tail);
        let buf = tail.as_slice();

        let mut i = 0usize;
        while i + 1 < buf.len() {
            // RIS. Handled in this same pass, in stream order: a mode set
            // *after* the reset must survive it and one set before must not,
            // which a separate pass over the buffer could not get right. ESC
            // restarts both CSI and OSC parsing, so `ESC c` never occurs
            // inside another sequence. Two bytes, so it is also applied when
            // it ends the chunk — a reset must not wait for the next read.
            if buf[i] == 0x1b && buf[i + 1] == b'c' {
                self.reset_modes();
                i += 2;
                continue;
            }
            if i + 2 >= buf.len() {
                break;
            }
            if buf[i] != 0x1b || buf[i + 1] != b'[' || buf[i + 2] != b'?' {
                i += 1;
                continue;
            }

            let mut k = i + 3;
            let mut nums: Vec<u32> = Vec::new();
            let mut num: u32 = 0;
            let mut saw_digit = false;
            let mut consumed = false;

            while k < buf.len() {
                let b = buf[k];
                if b.is_ascii_digit() {
                    saw_digit = true;
                    num = num.saturating_mul(10).saturating_add((b - b'0') as u32);
                    k += 1;
                    continue;
                }

                if b == b';' {
                    if saw_digit {
                        nums.push(num);
                        num = 0;
                        saw_digit = false;
                    }
                    k += 1;
                    continue;
                }

                if b == b'h' || b == b'l' {
                    if saw_digit {
                        nums.push(num);
                    }

                    let enabled = b == b'h';
                    for ps in nums {
                        match ps {
                            1 => self.application_cursor_keys = enabled,
                            25 => self.cursor_visible = enabled,
                            47 | 1047 | 1049 => {
                                self.alternate_screen_active = enabled;
                                if enabled {
                                    // Entering the alt screen (fullscreen
                                    // TUI) — the anchor no longer refers to
                                    // a shell prompt on the primary screen.
                                    self.input_anchor = None;
                                }
                            }
                            1007 => self.mouse_alternate_scroll = enabled,
                            2004 => self.bracketed_paste_enabled = enabled,
                            2026 => self.synchronized_output_active = enabled,
                            2031 => self.report_color_scheme = enabled,
                            1000 => self.mouse_x10_enabled = enabled,
                            1002 => self.mouse_button_event_enabled = enabled,
                            1003 => self.mouse_any_event_enabled = enabled,
                            1006 => self.mouse_sgr_enabled = enabled,
                            _ => {}
                        }
                    }

                    i = k + 1;
                    consumed = true;
                    break;
                }

                i += 1;
                consumed = true;
                break;
            }

            if k >= buf.len() && !consumed {
                break;
            }

            if consumed {
                continue;
            }

            i += 1;
        }

        let mut last_title: Option<String> = None;
        let mut last_clipboard: Option<String> = None;
        let mut j = 0usize;
        while j + 1 < buf.len() {
            if buf[j] != 0x1b || buf[j + 1] != b']' {
                j += 1;
                continue;
            }

            let mut k = j + 2;
            let mut ps: u32 = 0;
            let mut saw_digit = false;
            while k < buf.len() {
                let b = buf[k];
                if b.is_ascii_digit() {
                    saw_digit = true;
                    ps = ps.saturating_mul(10).saturating_add((b - b'0') as u32);
                    k += 1;
                    continue;
                }
                if b == b';' {
                    k += 1;
                    break;
                }
                break;
            }
            if !saw_digit || k >= buf.len() {
                j += 1;
                continue;
            }

            let title_start = k;
            while k < buf.len() {
                match buf[k] {
                    0x07 => {
                        if ps == 0 || ps == 2 {
                            last_title =
                                Some(String::from_utf8_lossy(&buf[title_start..k]).into_owned());
                        } else if ps == 52 {
                            last_clipboard = decode_osc_52(&buf[title_start..k]);
                        }
                        k += 1;
                        break;
                    }
                    0x1b if k + 1 < buf.len() && buf[k + 1] == b'\\' => {
                        if ps == 0 || ps == 2 {
                            last_title =
                                Some(String::from_utf8_lossy(&buf[title_start..k]).into_owned());
                        } else if ps == 52 {
                            last_clipboard = decode_osc_52(&buf[title_start..k]);
                        }
                        k += 2;
                        break;
                    }
                    _ => k += 1,
                }
            }

            j = k.max(j + 1);
        }

        if let Some(title) = last_title {
            self.title = Some(title);
        }
        if let Some(clipboard) = last_clipboard {
            self.clipboard_write = Some(clipboard);
        }

        // Now that the whole buffer has been scanned, keep only enough of its
        // tail to complete a sequence cut in half by the read boundary. The
        // carry is re-scanned on the next call, which is why every effect above
        // is idempotent (flag assignments and last-wins title/clipboard) and
        // applied in buffer order (RIS included).
        self.parse_tail = tail;
        let len = self.parse_tail.len();
        if len > CARRY_LIMIT {
            self.parse_tail.drain(0..len - CARRY_LIMIT);
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.update_state_from_output(bytes);
        self.terminal.feed(bytes)
    }

    pub fn feed_with_pty_responses(
        &mut self,
        bytes: &[u8],
        mut send: impl FnMut(&[u8]),
    ) -> Result<(), Error> {
        self.update_state_from_output(bytes);

        let mut seg_start = 0usize;
        for (i, &b) in bytes.iter().enumerate() {
            let csi = self.csi_query_state.advance(b);
            let osc = self.osc_query_state.advance(b);
            if csi.is_none() && osc.is_none() {
                continue;
            }

            self.terminal.feed(&bytes[seg_start..=i])?;
            seg_start = i + 1;

            if let Some(query) = csi {
                match query {
                    TerminalQuery::DeviceStatus => send(b"\x1b[0n"),
                    TerminalQuery::CursorPosition => {
                        let (col, row) = self.cursor_position().unwrap_or((1, 1));
                        let resp = format!("\x1b[{};{}R", row, col);
                        send(resp.as_bytes());
                    }
                    TerminalQuery::ColorScheme => send(self.config.color_scheme.report()),
                    TerminalQuery::PrimaryDeviceAttributes => send(DA1_RESPONSE),
                    TerminalQuery::SecondaryDeviceAttributes => send(DA2_RESPONSE),
                }
            }

            if let Some(query) = osc {
                let color = match query {
                    OscQuery::Foreground => self.config.default_fg,
                    OscQuery::Background => self.config.default_bg,
                    // No explicit cursor colour means the cursor is drawn in
                    // the foreground colour, so that is the honest answer
                    // (ghostty's stream handler reports the same).
                    OscQuery::Cursor => self.config.cursor_color.unwrap_or(self.config.default_fg),
                };
                let resp = osc_color_query_response(query, (color.r, color.g, color.b));
                send(resp.as_bytes());
            }
        }

        if seg_start < bytes.len() {
            self.terminal.feed(&bytes[seg_start..])?;
        }

        Ok(())
    }

    pub fn dump_viewport(&self) -> Result<String, Error> {
        self.terminal.dump_viewport()
    }

    pub fn dump_viewport_row(&self, row: u16) -> Result<String, Error> {
        self.terminal.dump_viewport_row(row)
    }

    pub fn dump_viewport_row_cell_styles(
        &self,
        row: u16,
    ) -> Result<Vec<ghostty_vt::CellStyle>, Error> {
        self.terminal.dump_viewport_row_cell_styles(row)
    }

    pub fn dump_viewport_row_style_runs(
        &self,
        row: u16,
    ) -> Result<Vec<ghostty_vt::StyleRun>, Error> {
        self.terminal.dump_viewport_row_style_runs(row)
    }

    /// Read a row from scrollback above the viewport. `rows_above = 0`
    /// returns the row directly above viewport top. Returns `Ok(None)` if
    /// scrollback start is reached.
    pub fn dump_screen_row(&self, rows_above: u32) -> Result<Option<String>, Error> {
        self.terminal.dump_screen_row(rows_above)
    }

    pub fn dump_screen_row_style_runs(
        &self,
        rows_above: u32,
    ) -> Result<Option<Vec<ghostty_vt::StyleRun>>, Error> {
        self.terminal.dump_screen_row_style_runs(rows_above)
    }

    /// Where the viewport currently sits in the full screen — used to
    /// drive a scrollbar UI. See [`ghostty_vt::ScrollPosition`].
    pub fn scroll_position(&self) -> Option<ghostty_vt::ScrollPosition> {
        self.terminal.scroll_position()
    }

    /// Hard reset of terminal state. See [`ghostty_vt::Terminal::full_reset`].
    pub fn full_reset(&mut self) {
        self.terminal.full_reset();
        self.reset_modes();
    }

    /// Put every mirrored DEC mode back to its power-on value — what RIS
    /// (`ESC c`) does to the real modes inside ghostty. Also run on a pty swap
    /// (see [`Self::reset_for_new_pty`]): a mode the *previous* shell's program
    /// set must not outlive it. Mode 2031 is the one that makes this matter —
    /// a stale flag there would have the host *write* `CSI ? 997 ; Ps n` into
    /// a shell that never asked, once per theme switch, for as long as the
    /// pane lives.
    pub fn reset_modes(&mut self) {
        self.bracketed_paste_enabled = false;
        self.cursor_visible = true;
        self.synchronized_output_active = false;
        self.mouse_x10_enabled = false;
        self.mouse_button_event_enabled = false;
        self.mouse_any_event_enabled = false;
        self.mouse_sgr_enabled = false;
        self.alternate_screen_active = false;
        self.input_anchor = None;
        self.application_cursor_keys = false;
        self.mouse_alternate_scroll = true;
        self.report_color_scheme = false;
    }

    /// The pty behind this session was swapped (a plain shell replacing a
    /// dead zmx client, a re-attach after restart). Forget everything that
    /// described the old stream: the mirrored modes, the half-parsed carry
    /// and both query scanners — the dead client's last bytes must not fuse
    /// with the new shell's first ones into a mode, a query or a reply. A
    /// zmx re-attach replays the session's modes (measured: `?2031h` comes
    /// back in the replay), so resetting first is right there too.
    pub fn reset_for_new_pty(&mut self) {
        self.reset_modes();
        self.parse_tail.clear();
        self.csi_query_state = CsiQueryScanState::default();
        self.osc_query_state = OscQueryScanState::default();
    }

    /// Drain queued OSC / control events (notifications, command boundaries,
    /// bell, shell-integration prompt/input markers). The internal queue is
    /// cleared. Also updates [`Self::input_anchor`] from `InputStart` /
    /// `CommandStart` / `CommandEnd` as they're observed here.
    ///
    /// Ordering contract: the anchor is cleared as part of *this* call, for
    /// every `CommandStart`/`CommandEnd` in the drained batch — a caller
    /// that needs the pre-boundary anchor (e.g. to capture the input line
    /// that was just submitted) must read [`Self::input_anchor`] (or
    /// anything derived from it) *before* calling `drain_events`, not after.
    pub fn drain_events(&mut self) -> Vec<ghostty_vt::TerminalEvent> {
        let events = self.terminal.drain_events();
        for event in &events {
            match event {
                ghostty_vt::TerminalEvent::InputStart => {
                    // Cursor sits right after the prompt — snapshot it as
                    // the start-of-input anchor.
                    self.input_anchor = self.cursor_position();
                }
                ghostty_vt::TerminalEvent::CommandStart { .. }
                | ghostty_vt::TerminalEvent::CommandEnd { .. } => {
                    // Input is over (command about to run) or already ran —
                    // the anchor no longer describes an in-progress input
                    // line. Any consumer that needs it must have already
                    // read it before this point.
                    self.input_anchor = None;
                }
                _ => {}
            }
        }
        events
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        self.terminal.cursor_position()
    }

    pub fn scroll_viewport(&mut self, delta_lines: i32) -> Result<(), Error> {
        self.terminal.scroll_viewport(delta_lines)
    }

    pub fn scroll_viewport_top(&mut self) -> Result<(), Error> {
        self.terminal.scroll_viewport_top()
    }

    pub fn scroll_viewport_bottom(&mut self) -> Result<(), Error> {
        self.terminal.scroll_viewport_bottom()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        self.config.cols = cols;
        self.config.rows = rows;
        self.input_anchor = None;
        self.terminal.resize(cols, rows)
    }

    /// Cursor position (1-based `(col, row)`) captured when `InputStart`
    /// (OSC 133;B) was last drained — i.e. where the user's current input
    /// line begins. `None` if no prompt has started input yet, or if it was
    /// cleared by a command boundary, alt-screen entry, resize, or reset.
    pub fn input_anchor(&self) -> Option<(u16, u16)> {
        self.input_anchor
    }

    pub(crate) fn take_dirty_viewport_rows(&mut self) -> Vec<u16> {
        self.terminal
            .take_dirty_viewport_rows(self.config.rows)
            .unwrap_or_default()
    }

    pub(crate) fn take_viewport_scroll_delta(&mut self) -> i32 {
        self.terminal.take_viewport_scroll_delta()
    }
}

/// A query in the pty output that the terminal — not the shell — must answer.
#[derive(Clone, Copy, Debug)]
enum TerminalQuery {
    /// `CSI 5 n`.
    DeviceStatus,
    /// `CSI 6 n`.
    CursorPosition,
    /// `CSI ? 996 n` — the query half of DEC mode 2031.
    ColorScheme,
    /// `CSI c` / `CSI 0 c` (DA1).
    PrimaryDeviceAttributes,
    /// `CSI > c` / `CSI > 0 c` (DA2).
    SecondaryDeviceAttributes,
}

/// DA1: VT220-level conformance (62) with colour text (22). The same identity
/// ghostty reports without clipboard access, and the one zmx answers on our
/// behalf while no client is attached — so a program sees one terminal
/// whichever side happens to answer.
const DA1_RESPONSE: &[u8] = b"\x1b[?62;22c";
/// DA2: ghostty's own secondary attributes, which zmx also mirrors.
const DA2_RESPONSE: &[u8] = b"\x1b[>1;10;0c";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscQuery {
    Foreground,
    Background,
    Cursor,
}

impl OscQuery {
    fn for_ps(ps: u32) -> Option<OscQuery> {
        match ps {
            10 => Some(OscQuery::Foreground),
            11 => Some(OscQuery::Background),
            12 => Some(OscQuery::Cursor),
            _ => None,
        }
    }
}

fn osc_color_query_response(query: OscQuery, (r, g, b): (u8, u8, u8)) -> String {
    let ps = match query {
        OscQuery::Foreground => 10,
        OscQuery::Background => 11,
        OscQuery::Cursor => 12,
    };

    let r16 = u16::from(r) * 0x0101;
    let g16 = u16::from(g) * 0x0101;
    let b16 = u16::from(b) * 0x0101;

    format!("\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\", ps, r16, g16, b16)
}

/// Byte-at-a-time recogniser for the CSI queries in [`TerminalQuery`]. Runs
/// beside ghostty's own parser (which applies the sequence to the screen but,
/// being read-only, never answers), so a query split across two pty reads is
/// still seen whole.
#[derive(Clone, Copy, Debug, Default)]
enum CsiQueryScanState {
    #[default]
    Idle,
    Esc,
    /// Inside `CSI`, before the final byte. `marker` is the private prefix
    /// (`?`, `>`, or 0 for none), `value` the one numeric parameter seen so
    /// far. `multi` records a `;`: none of the queries answered here takes a
    /// parameter list, and that is exactly what keeps a DA *response* echoed
    /// back (`CSI ? 62 ; 22 c`) from reading as a query.
    Csi {
        marker: u8,
        value: u32,
        saw_digit: bool,
        multi: bool,
    },
}

impl CsiQueryScanState {
    fn advance(&mut self, b: u8) -> Option<TerminalQuery> {
        use CsiQueryScanState::*;

        let (next, matched) = match (*self, b) {
            (_, 0x1b) => (Esc, None),
            (Esc, b'[') => (
                Csi {
                    marker: 0,
                    value: 0,
                    saw_digit: false,
                    multi: false,
                },
                None,
            ),
            (Esc, _) => (Idle, None),
            (
                Csi {
                    marker: 0,
                    saw_digit: false,
                    multi: false,
                    ..
                },
                b'?' | b'>',
            ) => (
                Csi {
                    marker: b,
                    value: 0,
                    saw_digit: false,
                    multi: false,
                },
                None,
            ),
            (
                Csi {
                    marker,
                    value,
                    multi,
                    ..
                },
                d,
            ) if d.is_ascii_digit() => (
                Csi {
                    marker,
                    value: value.saturating_mul(10).saturating_add(u32::from(d - b'0')),
                    saw_digit: true,
                    multi,
                },
                None,
            ),
            (Csi { marker, .. }, b';') => (
                Csi {
                    marker,
                    value: 0,
                    saw_digit: false,
                    multi: true,
                },
                None,
            ),
            (
                Csi {
                    marker,
                    value,
                    multi,
                    ..
                },
                final_byte @ 0x40..=0x7e,
            ) => (Idle, csi_query(marker, value, multi, final_byte)),
            _ => (Idle, None),
        };

        *self = next;
        matched
    }
}

fn csi_query(marker: u8, value: u32, multi: bool, final_byte: u8) -> Option<TerminalQuery> {
    if multi {
        return None;
    }
    match (final_byte, marker, value) {
        (b'n', 0 | b'?', 5) => Some(TerminalQuery::DeviceStatus),
        (b'n', 0 | b'?', 6) => Some(TerminalQuery::CursorPosition),
        (b'n', b'?', 996) => Some(TerminalQuery::ColorScheme),
        (b'c', 0, 0) => Some(TerminalQuery::PrimaryDeviceAttributes),
        (b'c', b'>', 0) => Some(TerminalQuery::SecondaryDeviceAttributes),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default)]
enum OscQueryScanState {
    #[default]
    Idle,
    Esc,
    Osc,
    Ps {
        value: u32,
    },
    AfterSemicolon {
        ps: u32,
    },
    Query {
        ps: u32,
    },
    StEscape {
        ps: u32,
    },
}

impl OscQueryScanState {
    fn advance(&mut self, b: u8) -> Option<OscQuery> {
        use OscQueryScanState::*;

        let matched = match (*self, b) {
            (Query { ps }, 0x07) | (StEscape { ps }, b'\\') => OscQuery::for_ps(ps),
            _ => None,
        };

        *self = match (*self, b) {
            (Query { ps }, 0x1b) => StEscape { ps },
            (_, 0x1b) => Esc,
            (Esc, b']') => Osc,
            (Esc, _) => Idle,
            (Osc, d) if d.is_ascii_digit() => Ps {
                value: (d - b'0') as u32,
            },
            (Ps { value }, d) if d.is_ascii_digit() => Ps {
                value: value.saturating_mul(10).saturating_add((d - b'0') as u32),
            },
            (Ps { value }, b';') => value_to_after_semicolon_state(value),
            (Osc, _) | (Ps { .. }, _) => Idle,
            (AfterSemicolon { ps }, b'?') => Query { ps },
            (AfterSemicolon { .. }, _) => Idle,
            (Query { .. }, 0x07) => Idle,
            (Query { .. }, _) => Idle,
            (StEscape { .. }, b'\\') => Idle,
            (StEscape { .. }, _) => Idle,
            _ => Idle,
        };

        matched
    }
}

fn value_to_after_semicolon_state(ps: u32) -> OscQueryScanState {
    if OscQuery::for_ps(ps).is_some() {
        OscQueryScanState::AfterSemicolon { ps }
    } else {
        OscQueryScanState::Idle
    }
}

fn decode_osc_52(payload: &[u8]) -> Option<String> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    let mut split = payload.splitn(2, |b| *b == b';');
    let selection = split.next()?;
    let data = split.next()?;

    if !selection.contains(&b'c') {
        return None;
    }
    if data.is_empty() {
        return None;
    }

    let decoded = STANDARD.decode(data).ok()?;
    Some(String::from_utf8_lossy(&decoded).into_owned())
}

#[cfg(test)]
mod state_scan_tests {
    use super::{TerminalConfig, TerminalSession};

    fn session() -> TerminalSession {
        TerminalSession::new(TerminalConfig::default()).expect("terminal session")
    }

    /// The reattach bug (2026-07-28): a `zmx` re-attach replays the whole
    /// session in one ~130 KB write whose *first* bytes set the modes. Scanning
    /// only the tail of that write left the pane believing it was on the primary
    /// screen with mouse reporting off, so the wheel scrolled a scrollback the
    /// fullscreen app does not have and the terminal looked frozen to scrolling
    /// until the pane was resized.
    #[test]
    fn modes_at_the_head_of_a_huge_batch_are_applied() {
        let mut vt = session();
        let mut replay =
            b"\x1b[2J\x1b[H\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?1049h".to_vec();
        // Well past the carry window — the live replay is ~64× this.
        replay.extend(std::iter::repeat_n(b'x', 128 * 1024));
        vt.feed(&replay).expect("feed");

        assert!(vt.alternate_screen_active(), "?1049h lost");
        assert!(vt.mouse_reporting_enabled(), "?1000h/?1002h/?1003h lost");
        assert!(vt.mouse_sgr_enabled(), "?1006h lost");
    }

    /// Same batch, same loss: the pane's title (and an OSC 52 clipboard write)
    /// rode in the head of the replay too.
    #[test]
    fn an_osc_title_at_the_head_of_a_huge_batch_is_applied() {
        let mut vt = session();
        let mut replay = b"\x1b]0;claude - analytics-ops\x07".to_vec();
        replay.extend(std::iter::repeat_n(b'y', 64 * 1024));
        vt.feed(&replay).expect("feed");

        assert_eq!(vt.title(), Some("claude - analytics-ops"));
    }

    /// What the carry buffer is actually for: a sequence cut in half by a read
    /// boundary must still be seen once its second half arrives. Deleting the
    /// carry entirely would keep the test above green.
    #[test]
    fn a_mode_split_across_two_batches_is_applied() {
        let mut vt = session();
        vt.feed(b"\x1b[?10").expect("feed head");
        assert!(
            !vt.mouse_sgr_enabled(),
            "half a sequence must decide nothing"
        );
        vt.feed(b"06h").expect("feed tail");
        assert!(
            vt.mouse_sgr_enabled(),
            "the split ?1006h was never completed"
        );
    }

    /// The carry stays bounded across a long stream, or a busy pane grows a
    /// buffer that is re-scanned on every batch forever.
    #[test]
    fn the_carry_stays_bounded() {
        let mut vt = session();
        for _ in 0..8 {
            vt.feed(&vec![b'z'; 64 * 1024]).expect("feed");
        }
        assert!(
            vt.parse_tail.len() <= 2048,
            "carry grew to {} bytes",
            vt.parse_tail.len()
        );
    }
}
