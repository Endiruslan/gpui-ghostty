use ghostty_vt::{Error, Rgb, Terminal};

use crate::TerminalConfig;

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
    parse_tail: Vec<u8>,
    /// The OSC 0/2 title not yet handed to the host as an event — see
    /// [`Self::drain_events`]. "Changed since last time", not "seen": the parse
    /// carry is re-scanned by design, so the same sequence is observed more
    /// than once whenever it lands near a read boundary.
    pending_title_event: Option<String>,
    dsr_state: DsrScanState,
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
            parse_tail: Vec::new(),
            pending_title_event: None,
            dsr_state: DsrScanState::default(),
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
        let buf = self.parse_tail.as_slice();

        let mut i = 0usize;
        while i + 2 < buf.len() {
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
            // Only a *change* is an event: the carry is re-scanned on the next
            // call (that is what makes every effect here idempotent), so the
            // same title is seen again whenever it lands near a read boundary,
            // and a host that relabels a tab must not be woken for it twice.
            if self.title.as_deref() != Some(title.as_str()) {
                self.pending_title_event = Some(title.clone());
            }
            self.title = Some(title);
        }
        if let Some(clipboard) = last_clipboard {
            self.clipboard_write = Some(clipboard);
        }

        // Now that the whole buffer has been scanned, keep only enough of its
        // tail to complete a sequence cut in half by the read boundary. The
        // carry is re-scanned on the next call, which is why every effect above
        // is idempotent (flag assignments and last-wins title/clipboard).
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
            let dsr = self.dsr_state.advance(b);
            let osc = self.osc_query_state.advance(b);
            if dsr.is_none() && osc.is_none() {
                continue;
            }

            self.terminal.feed(&bytes[seg_start..=i])?;
            seg_start = i + 1;

            if let Some(query) = dsr {
                match query {
                    TerminalQuery::DeviceStatus => send(b"\x1b[0n"),
                    TerminalQuery::CursorPosition => {
                        let (col, row) = self.cursor_position().unwrap_or((1, 1));
                        let resp = format!("\x1b[{};{}R", row, col);
                        send(resp.as_bytes());
                    }
                }
            }

            if let Some(query) = osc {
                let rgb = match query {
                    OscQuery::ForegroundColor => {
                        let fg = self.config.default_fg;
                        (fg.r, fg.g, fg.b)
                    }
                    OscQuery::BackgroundColor => {
                        let bg = self.config.default_bg;
                        (bg.r, bg.g, bg.b)
                    }
                };
                let resp = osc_color_query_response(query, rgb);
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
        self.input_anchor = None;
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
    /// Drop the half-scanned tail carried between PTY reads.
    ///
    /// For a host that swaps the pty under a live session (mxds re-attaches a
    /// pane to a new `zmx` client): the dead client's last bytes may sit
    /// mid-sequence in the carry, and fusing them with the new one's first
    /// bytes yields a title or a working directory that was never sent.
    /// Only the *carry* goes — everything already parsed out of it stands.
    pub fn reset_output_scan(&mut self) {
        self.parse_tail.clear();
    }

    pub fn drain_events(&mut self) -> Vec<ghostty_vt::TerminalEvent> {
        let mut events = self.terminal.drain_events();
        // Appended after the C drain, not merged into it: these two are framed
        // by our own OSC scan (`update_state_from_output`), which has already
        // run for every byte in this batch by the time anything drains.
        if let Some(title) = self.pending_title_event.take() {
            events.push(ghostty_vt::TerminalEvent::TitleChanged { title });
        }
        let events = events;
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

#[derive(Clone, Copy, Debug)]
enum TerminalQuery {
    DeviceStatus,
    CursorPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OscQuery {
    ForegroundColor,
    BackgroundColor,
}

fn osc_color_query_response(query: OscQuery, (r, g, b): (u8, u8, u8)) -> String {
    let ps = match query {
        OscQuery::ForegroundColor => 10,
        OscQuery::BackgroundColor => 11,
    };

    let r16 = u16::from(r) * 0x0101;
    let g16 = u16::from(g) * 0x0101;
    let b16 = u16::from(b) * 0x0101;

    format!("\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\", ps, r16, g16, b16)
}

#[derive(Clone, Copy, Debug, Default)]
enum DsrScanState {
    #[default]
    Idle,
    Esc,
    Csi,
    CsiQ,
    Csi5,
    CsiQ5,
    Csi6,
    CsiQ6,
}

impl DsrScanState {
    fn advance(&mut self, b: u8) -> Option<TerminalQuery> {
        use DsrScanState::*;

        let matched = match (*self, b) {
            (Csi5, b'n') | (CsiQ5, b'n') => Some(TerminalQuery::DeviceStatus),
            (Csi6, b'n') | (CsiQ6, b'n') => Some(TerminalQuery::CursorPosition),
            _ => None,
        };

        *self = match (*self, b) {
            (_, 0x1b) => Esc,
            (Esc, b'[') => Csi,
            (Csi, b'?') => CsiQ,
            (Csi, b'5') => Csi5,
            (CsiQ, b'5') => CsiQ5,
            (Csi, b'6') => Csi6,
            (CsiQ, b'6') => CsiQ6,
            (Csi5, b'n') => Idle,
            (CsiQ5, b'n') => Idle,
            (Csi6, b'n') => Idle,
            (CsiQ6, b'n') => Idle,
            _ => Idle,
        };

        matched
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
            (Query { ps }, 0x07) => match ps {
                10 => Some(OscQuery::ForegroundColor),
                11 => Some(OscQuery::BackgroundColor),
                _ => None,
            },
            (StEscape { ps }, b'\\') => match ps {
                10 => Some(OscQuery::ForegroundColor),
                11 => Some(OscQuery::BackgroundColor),
                _ => None,
            },
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
    match ps {
        10 | 11 => OscQueryScanState::AfterSemicolon { ps },
        _ => OscQueryScanState::Idle,
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

    /// Every title case mxds used to pin with a byte scanner of its own
    /// (`terminal_ui::osc_title::TitleParser`, deleted when this became the one
    /// place a title is framed): both terminators, OSC 1 ignored, a payload cut
    /// by a read boundary, a terminator cut by one, several titles in a batch,
    /// and a malformed sequence that must not poison the next.
    #[test]
    fn a_title_is_framed_the_way_the_host_scanner_used_to_frame_it() {
        for (name, chunks, want) in [
            (
                "OSC 0, BEL",
                vec![&b"foo\x1b]0;My Tab\x07bar"[..]],
                Some("My Tab"),
            ),
            (
                "OSC 2, ST",
                vec![&b"\x1b]2;Window\x1b\\done"[..]],
                Some("Window"),
            ),
            (
                "OSC 1 is the icon name, not the title",
                vec![&b"\x1b]1;icon\x07"[..]],
                None,
            ),
            (
                "payload split across reads",
                vec![&b"\x1b]0;hel"[..], &b"lo\x07"[..]],
                Some("hello"),
            ),
            (
                "ST split across reads",
                vec![&b"\x1b]2;x\x1b"[..], &b"\\"[..]],
                Some("x"),
            ),
            (
                "an unhandled OSC does not poison the next title",
                vec![&b"\x1b]9;nope\x07\x1b]2;good\x07"[..]],
                Some("good"),
            ),
            (
                "last title in the batch wins",
                vec![&b"\x1b]0;first\x07\x1b]0;second\x07"[..]],
                Some("second"),
            ),
        ] {
            let mut vt = session();
            for chunk in chunks {
                vt.feed(chunk).expect("feed");
            }
            assert_eq!(vt.title(), want, "{name}");
        }
    }

    /// And the host hears about it exactly once per change — the parse carry is
    /// re-scanned on the next read by design, so "seen again" must not become
    /// "changed again" and relabel a tab on every batch.
    #[test]
    fn a_title_change_is_one_event_and_a_repeat_is_none() {
        let mut vt = session();
        vt.feed(b"\x1b]0;One\x07").expect("feed");
        let first: Vec<_> = vt
            .drain_events()
            .into_iter()
            .filter(|e| matches!(e, ghostty_vt::TerminalEvent::TitleChanged { .. }))
            .collect();
        assert_eq!(
            first,
            vec![ghostty_vt::TerminalEvent::TitleChanged {
                title: "One".to_string()
            }]
        );

        // Same title again, then nothing at all: neither may produce an event.
        vt.feed(b"\x1b]0;One\x07").expect("feed");
        vt.feed(b"plain output\n").expect("feed");
        let repeats: Vec<_> = vt
            .drain_events()
            .into_iter()
            .filter(|e| matches!(e, ghostty_vt::TerminalEvent::TitleChanged { .. }))
            .collect();
        assert!(
            repeats.is_empty(),
            "re-announced an unchanged title: {repeats:?}"
        );

        vt.feed(b"\x1b]0;Two\x07").expect("feed");
        let changed: Vec<_> = vt
            .drain_events()
            .into_iter()
            .filter(|e| matches!(e, ghostty_vt::TerminalEvent::TitleChanged { .. }))
            .collect();
        assert_eq!(
            changed,
            vec![ghostty_vt::TerminalEvent::TitleChanged {
                title: "Two".to_string()
            }]
        );
    }

    /// A pty swap under a live pane (mxds re-attaching to a new `zmx` client)
    /// drops the half-scanned carry: the dead client's last bytes must not fuse
    /// with the new one's first into a title nobody sent.
    #[test]
    fn resetting_the_scan_drops_a_half_read_sequence() {
        let mut vt = session();
        vt.feed(b"\x1b]0;doomed").expect("feed");
        vt.reset_output_scan();
        vt.feed(b" title\x07").expect("feed");
        assert_eq!(
            vt.title(),
            None,
            "the carry survived the reset and completed a sequence from two clients"
        );
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
