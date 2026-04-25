use super::TerminalSession;
use ghostty_vt::{KeyModifiers, Rgb, StyleRun, encode_key_named};
use gpui::{
    App, Bounds, ClipboardItem, Context, Element, ElementId, ElementInputHandler,
    EntityInputHandler, FocusHandle, GlobalElementId, IntoElement, KeyBinding, KeyDownEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Render,
    ScrollDelta, ScrollWheelEvent, SharedString, Style, TextRun, TouchPhase, UTF16Selection,
    UnderlineStyle, Window, actions, div, fill, hsla, point, prelude::*, px, relative, rgba, size,
};
use std::ops::Range;
use std::sync::Once;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Lightweight cumulative counters dumped to stderr roughly every 500 ms
/// while the user is actively scrolling. Lets us compare CPU vs. event
/// volume vs. line-snaps vs. peek refetches at a glance — was added when
/// the pixel-smooth scroll landed and CPU jumped from ~10–20% to ~40%
/// during trackpad scrolls.
struct ScrollStats {
    events: AtomicU64,
    subline_paints: AtomicU64,
    line_snaps: AtomicU64,
    peek_fetches: AtomicU64,
    peek_reshapes: AtomicU64,
    boundary_clamps: AtomicU64,
    /// Number of times `TerminalTextElement::paint` was actually invoked.
    /// Compared with `events` to see if GPUI is re-painting more often than
    /// the trackpad event rate (display-link driven, PTY-driven, etc).
    paint_calls: AtomicU64,
    /// Microseconds spent inside `TerminalTextElement::paint` excluding
    /// `nextDrawable` (paint itself only — `prepaint` runs separately). Lets
    /// us see if per-paint CPU work is the bottleneck or if it's display
    /// sync waits.
    paint_us: AtomicU64,
    /// Number of times `TerminalTextElement::prepaint` was invoked, plus
    /// total microseconds spent there.
    prepaint_calls: AtomicU64,
    prepaint_us: AtomicU64,
    /// Mouse-wheel events forwarded to the running TUI as SGR mouse
    /// sequences (mouse-reporting mode). Each forwarded event causes the
    /// TUI to re-render and emit PTY output back, which in turn triggers
    /// our `feed_output_bytes` notify path.
    sgr_wheel_events: AtomicU64,
    /// Number of times `feed_output_bytes` (PTY → terminal) caused a notify.
    /// If this is high during scroll, the running TUI is re-rendering in
    /// response to our forwarded mouse events — that's external CPU we
    /// can't optimise on the terminal side.
    pty_input_notifies: AtomicU64,
    /// Bytes received from PTY while scrolling.
    pty_input_bytes: AtomicU64,
    last_emit_ms: AtomicU64,
    started_ms: AtomicU64,
}

impl ScrollStats {
    const fn new() -> Self {
        Self {
            events: AtomicU64::new(0),
            subline_paints: AtomicU64::new(0),
            line_snaps: AtomicU64::new(0),
            peek_fetches: AtomicU64::new(0),
            peek_reshapes: AtomicU64::new(0),
            boundary_clamps: AtomicU64::new(0),
            paint_calls: AtomicU64::new(0),
            paint_us: AtomicU64::new(0),
            prepaint_calls: AtomicU64::new(0),
            prepaint_us: AtomicU64::new(0),
            sgr_wheel_events: AtomicU64::new(0),
            pty_input_notifies: AtomicU64::new(0),
            pty_input_bytes: AtomicU64::new(0),
            last_emit_ms: AtomicU64::new(0),
            started_ms: AtomicU64::new(0),
        }
    }

    fn now_ms() -> u64 {
        // Monotonic millis since process start. We don't need wall time.
        static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        epoch.elapsed().as_millis() as u64
    }

    fn maybe_emit(&self) {
        let now = Self::now_ms();
        let last = self.last_emit_ms.load(Ordering::Relaxed);
        if last == 0 {
            self.started_ms.store(now, Ordering::Relaxed);
            self.last_emit_ms.store(now, Ordering::Relaxed);
            return;
        }
        if now.saturating_sub(last) < 500 {
            return;
        }
        // Reset the window — we want per-window deltas, not cumulative.
        let events = self.events.swap(0, Ordering::Relaxed);
        let subline = self.subline_paints.swap(0, Ordering::Relaxed);
        let snaps = self.line_snaps.swap(0, Ordering::Relaxed);
        let fetches = self.peek_fetches.swap(0, Ordering::Relaxed);
        let reshapes = self.peek_reshapes.swap(0, Ordering::Relaxed);
        let clamps = self.boundary_clamps.swap(0, Ordering::Relaxed);
        let paint_calls = self.paint_calls.swap(0, Ordering::Relaxed);
        let paint_us = self.paint_us.swap(0, Ordering::Relaxed);
        let prepaint_calls = self.prepaint_calls.swap(0, Ordering::Relaxed);
        let prepaint_us = self.prepaint_us.swap(0, Ordering::Relaxed);
        let sgr_evts = self.sgr_wheel_events.swap(0, Ordering::Relaxed);
        let pty_notifies = self.pty_input_notifies.swap(0, Ordering::Relaxed);
        let pty_bytes = self.pty_input_bytes.swap(0, Ordering::Relaxed);
        self.last_emit_ms.store(now, Ordering::Relaxed);
        if events == 0 && subline == 0 && snaps == 0 && fetches == 0 && paint_calls == 0 {
            return;
        }
        let dur_ms = (now - last).max(1);
        let per_sec = |x: u64| (x as f64 * 1000.0) / dur_ms as f64;
        let avg_us = |total: u64, n: u64| if n == 0 { 0.0 } else { total as f64 / n as f64 };
        eprintln!(
            "[scroll] {dur_ms}ms ev={events}({:.0}/s) sub={subline}({:.0}/s) snaps={snaps}({:.0}/s) clamps={clamps} sgr={sgr_evts} pty_notif={pty_notifies}({:.0}/s, {pty_bytes}B) | paint={paint_calls}({:.0}/s, {:.0}us) prep={prepaint_calls}({:.0}/s, {:.0}us)",
            per_sec(events),
            per_sec(subline),
            per_sec(snaps),
            per_sec(pty_notifies),
            per_sec(paint_calls),
            avg_us(paint_us, paint_calls),
            per_sec(prepaint_calls),
            avg_us(prepaint_us, prepaint_calls),
        );
    }
}

static SCROLL_STATS: ScrollStats = ScrollStats::new();

actions!(terminal_view, [Copy, Paste, SelectAll, Tab, TabPrev]);

const KEY_CONTEXT: &str = "Terminal";
static KEY_BINDINGS: Once = Once::new();

fn ensure_key_bindings(cx: &mut App) {
    KEY_BINDINGS.call_once(|| {
        cx.bind_keys([
            KeyBinding::new("tab", Tab, Some(KEY_CONTEXT)),
            KeyBinding::new("shift-tab", TabPrev, Some(KEY_CONTEXT)),
        ]);
    });
}

fn split_viewport_lines(viewport: &str) -> Vec<String> {
    let viewport = viewport.strip_suffix('\n').unwrap_or(viewport);
    if viewport.is_empty() {
        return Vec::new();
    }
    viewport.split('\n').map(|line| line.to_string()).collect()
}

pub(crate) fn should_skip_key_down_for_ime(has_input: bool, keystroke: &gpui::Keystroke) -> bool {
    if !has_input || !keystroke.is_ime_in_progress() {
        return false;
    }

    !matches!(
        keystroke.key.as_str(),
        "enter" | "return" | "kp_enter" | "numpad_enter"
    )
}

pub(crate) fn ctrl_byte_for_keystroke(keystroke: &gpui::Keystroke) -> Option<u8> {
    let candidate = keystroke
        .key_char
        .as_deref()
        .or_else(|| (!keystroke.key.is_empty()).then_some(keystroke.key.as_str()))?;

    if candidate == "space" {
        return Some(0x00);
    }

    let bytes = candidate.as_bytes();
    if bytes.len() != 1 {
        return None;
    }

    let b = bytes[0];
    if (b'@'..=b'_').contains(&b) {
        Some(b & 0x1f)
    } else if b.is_ascii_lowercase() {
        Some(b - b'a' + 1)
    } else if b.is_ascii_uppercase() {
        Some(b - b'A' + 1)
    } else {
        None
    }
}

pub(crate) fn sgr_mouse_button_value(
    base_button: u8,
    motion: bool,
    shift: bool,
    alt: bool,
    control: bool,
) -> u8 {
    let mut value = base_button;
    if motion {
        value = value.saturating_add(32);
    }
    if shift {
        value = value.saturating_add(4);
    }
    if alt {
        value = value.saturating_add(8);
    }
    if control {
        value = value.saturating_add(16);
    }
    value
}

fn window_position_to_local(
    last_bounds: Option<Bounds<Pixels>>,
    position: gpui::Point<gpui::Pixels>,
) -> gpui::Point<gpui::Pixels> {
    let origin = last_bounds
        .map(|bounds| bounds.origin)
        .unwrap_or_else(|| point(px(0.0), px(0.0)));
    point(position.x - origin.x, position.y - origin.y)
}

pub(crate) fn sgr_mouse_sequence(button_value: u8, col: u16, row: u16, pressed: bool) -> String {
    let suffix = if pressed { 'M' } else { 'm' };
    format!("\x1b[<{};{};{}{}", button_value, col, row, suffix)
}

fn is_url_byte(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9')
        || matches!(
            b,
            b'-' | b'.'
                | b'_'
                | b'~'
                | b':'
                | b'/'
                | b'?'
                | b'#'
                | b'['
                | b']'
                | b'@'
                | b'!'
                | b'$'
                | b'&'
                | b'\''
                | b'('
                | b')'
                | b'*'
                | b'+'
                | b','
                | b';'
                | b'='
                | b'%'
        )
}

fn url_at_byte_index(text: &str, index: usize) -> Option<String> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }

    let mut idx = index.min(bytes.len().saturating_sub(1));

    if !is_url_byte(bytes[idx]) && idx > 0 && is_url_byte(bytes[idx - 1]) {
        idx -= 1;
    }

    if !is_url_byte(bytes[idx]) {
        return None;
    }

    let mut start = idx;
    while start > 0 && is_url_byte(bytes[start - 1]) {
        start -= 1;
    }

    let mut end = idx + 1;
    while end < bytes.len() && is_url_byte(bytes[end]) {
        end += 1;
    }

    while end > start
        && matches!(
            bytes[end - 1],
            b'.' | b',' | b')' | b']' | b'}' | b';' | b':' | b'!' | b'?'
        )
    {
        end -= 1;
    }

    let candidate = std::str::from_utf8(&bytes[start..end]).ok()?;
    if candidate.starts_with("https://") || candidate.starts_with("http://") {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn url_at_column_in_line(line: &str, col: u16) -> Option<String> {
    if line.is_empty() {
        return None;
    }

    let local = byte_index_for_column_in_line(line, col).min(line.len().saturating_sub(1));
    url_at_byte_index(line, local)
}

type TerminalSendFn = dyn Fn(&[u8]) + Send + Sync + 'static;

pub struct TerminalInput {
    send: Box<TerminalSendFn>,
}

impl TerminalInput {
    pub fn new(send: impl Fn(&[u8]) + Send + Sync + 'static) -> Self {
        Self {
            send: Box::new(send),
        }
    }

    pub fn send(&self, bytes: &[u8]) {
        (self.send)(bytes);
    }
}

/// Build the `gpui::Font` to use for a terminal, applying any override
/// from its `TerminalConfig.font_family` while keeping the rich fallback
/// chain from `default_terminal_font()` (SF Mono, Cascadia, JetBrains,
/// Noto CJK mono, emoji …).
fn font_for_session(session: &TerminalSession) -> gpui::Font {
    let mut font = crate::default_terminal_font();
    if let Some(family) = session.font_family() {
        font.family = family.to_string().into();
    }
    font
}

pub struct TerminalView {
    session: TerminalSession,
    viewport_lines: Vec<String>,
    viewport_line_offsets: Vec<usize>,
    viewport_total_len: usize,
    viewport_style_runs: Vec<Vec<StyleRun>>,
    line_layouts: Vec<Option<gpui::ShapedLine>>,
    line_layout_key: Option<(Pixels, Pixels)>,
    last_bounds: Option<Bounds<Pixels>>,
    focus_handle: FocusHandle,
    last_window_title: Option<String>,
    input: Option<TerminalInput>,
    pending_output: Vec<u8>,
    pending_refresh: bool,
    selection: Option<ByteSelection>,
    marked_text: Option<SharedString>,
    marked_selected_range_utf16: Range<usize>,
    font: gpui::Font,
    /// Whether the cursor should currently be drawn. When blink is enabled,
    /// a background timer toggles this ~every `cursor_blink_ms`. When blink
    /// is disabled, this stays `true` forever (solid cursor).
    cursor_blink_on: bool,
    /// Runtime switch for the blink timer. Lets the host pause blinking on
    /// focus-out (unfocused pane shows a static cursor) and resume on
    /// focus-in without tearing down the timer task.
    cursor_blink_enabled: bool,
    /// True while the hosting window is active (frontmost, not occluded).
    /// Tracked via `observe_window_activation`. When false, the blink timer
    /// stops calling `cx.notify()` — no redraws happen while the window is
    /// in the background, saving several % idle CPU.
    window_active: bool,
    /// Held so the window activation subscription lives as long as the view.
    _window_activation_sub: Option<gpui::Subscription>,
    /// Sub-line vertical scroll position in **pixels**, in `[0, cell_h)`.
    /// Represents how much of the bleed-row above the viewport ("peek row")
    /// is currently revealed at the top edge. `0.0` means the viewport sits
    /// exactly on a line boundary, just like a classic line-aligned terminal.
    /// Values in between produce smooth pixel-by-pixel motion: when this
    /// crosses a line boundary, [`apply_scroll_pixels`] snaps it back into
    /// range and advances the VT viewport by the corresponding line count.
    pixel_offset: f32,
    /// Cached text of the row immediately above the viewport. `None` means
    /// scrollback is exhausted (no row above) — `pixel_offset` will be
    /// clamped to `0.0` in that case so we never reveal an empty strip.
    peek_text: Option<String>,
    /// Style runs for [`peek_text`] (same shape as `viewport_style_runs[i]`).
    peek_runs: Vec<StyleRun>,
    /// Cached shaping for [`peek_text`]. Invalidated on font/size/text change.
    peek_layout: Option<gpui::ShapedLine>,
    /// Set when the peek-row data may be stale: VT scrolled, viewport text
    /// changed, or font metrics changed. Re-fetched lazily in prepaint, but
    /// only when `pixel_offset > 0` (otherwise peek isn't rendered).
    peek_dirty: bool,
    /// Cached viewport position inside the full screen, used to size and
    /// place the scrollbar. Refreshed in prepaint when [`scroll_pos_dirty`]
    /// is set — i.e. after VT scroll, refresh, or new PTY content. The FFI
    /// behind this walks the page list (O(pages)) so we don't want to call
    /// it every frame; on idle frames the cached value is accurate.
    scroll_pos: Option<ghostty_vt::ScrollPosition>,
    scroll_pos_dirty: bool,
}

#[derive(Clone, Copy, Debug)]
struct ByteSelection {
    anchor: usize,
    active: usize,
}

impl ByteSelection {
    fn range(self) -> Range<usize> {
        if self.anchor <= self.active {
            self.anchor..self.active
        } else {
            self.active..self.anchor
        }
    }
}

impl TerminalView {
    fn has_synchronized_output_mode_change(bytes: &[u8]) -> bool {
        bytes.windows(8).any(|window| {
            matches!(
                window,
                [0x1b, b'[', b'?', b'2', b'0', b'2', b'6', b'h']
                    | [0x1b, b'[', b'?', b'2', b'0', b'2', b'6', b'l']
            )
        })
    }

    pub fn new(session: TerminalSession, focus_handle: FocusHandle) -> Self {
        let font = font_for_session(&session);
        Self {
            session,
            viewport_lines: Vec::new(),
            viewport_line_offsets: Vec::new(),
            viewport_total_len: 0,
            viewport_style_runs: Vec::new(),
            line_layouts: Vec::new(),
            line_layout_key: None,
            last_bounds: None,
            focus_handle,
            last_window_title: None,
            input: None,
            pending_output: Vec::new(),
            pending_refresh: false,
            selection: None,
            marked_text: None,
            marked_selected_range_utf16: 0..0,
            font,
            cursor_blink_on: true,
            cursor_blink_enabled: true,
            window_active: true,
            _window_activation_sub: None,
            pixel_offset: 0.0,
            peek_text: None,
            peek_runs: Vec::new(),
            peek_layout: None,
            peek_dirty: false,
            scroll_pos: None,
            scroll_pos_dirty: true,
        }
        .with_refreshed_viewport()
    }

    fn on_tab(&mut self, _: &Tab, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(false, cx);
    }

    fn on_tab_prev(&mut self, _: &TabPrev, _window: &mut Window, cx: &mut Context<Self>) {
        self.send_tab(true, cx);
    }

    fn send_tab(&mut self, reverse: bool, cx: &mut Context<Self>) {
        if reverse {
            self.send_input_parts(&[b"\x1b[Z"], cx);
        } else {
            self.send_input_parts(&[b"\t"], cx);
        }
    }

    pub fn new_with_input(
        session: TerminalSession,
        focus_handle: FocusHandle,
        input: TerminalInput,
    ) -> Self {
        let font = font_for_session(&session);
        Self {
            session,
            viewport_lines: Vec::new(),
            viewport_line_offsets: Vec::new(),
            viewport_total_len: 0,
            viewport_style_runs: Vec::new(),
            line_layouts: Vec::new(),
            line_layout_key: None,
            last_bounds: None,
            focus_handle,
            last_window_title: None,
            input: Some(input),
            pending_output: Vec::new(),
            pending_refresh: false,
            selection: None,
            marked_text: None,
            marked_selected_range_utf16: 0..0,
            font,
            cursor_blink_on: true,
            cursor_blink_enabled: true,
            window_active: true,
            _window_activation_sub: None,
            pixel_offset: 0.0,
            peek_text: None,
            peek_runs: Vec::new(),
            peek_layout: None,
            peek_dirty: false,
            scroll_pos: None,
            scroll_pos_dirty: true,
        }
        .with_refreshed_viewport()
    }

    /// Start the cursor-blink timer if `TerminalConfig.cursor_blink_ms` is set,
    /// and subscribe to window activation so the timer can pause when the
    /// window is backgrounded.
    ///
    /// Idempotent-ish: calling twice spawns two timers — call once after
    /// constructing the view in `cx.new`. Must pass `&mut Window` so the
    /// activation subscription can register.
    ///
    /// Implementation: single background task. Each tick toggles the blink
    /// bool; `cx.notify()` is only issued while `window_active` is true.
    /// While the window is inactive, the timer still runs (cheap) but
    /// produces no repaints.
    pub fn start_cursor_blink(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Track window active/inactive — render loop skips notify when
        // inactive to avoid wasted paints on a backgrounded window.
        self.window_active = window.is_window_active();
        let sub = cx.observe_window_activation(window, |view, window, cx| {
            let now_active = window.is_window_active();
            if view.window_active != now_active {
                view.window_active = now_active;
                if now_active {
                    // Snap cursor on when we regain focus for instant feedback.
                    view.cursor_blink_on = true;
                    cx.notify();
                }
            }
        });
        self._window_activation_sub = Some(sub);

        let Some(blink_ms) = self.session.cursor_blink_ms() else {
            return;
        };
        let dur = std::time::Duration::from_millis(blink_ms);
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(dur).await;
            if this
                .update(cx, |view, cx| {
                    if !view.cursor_blink_enabled {
                        // Blink paused by host (unfocused pane); keep cursor
                        // solid-on and issue no repaints.
                        return;
                    }
                    view.cursor_blink_on = !view.cursor_blink_on;
                    if view.window_active {
                        cx.notify();
                    }
                })
                .is_err()
            {
                break;
            }
        })
        .detach();
    }

    /// Runtime toggle for cursor blinking. Host panes flip this on
    /// focus-in / focus-out so the unfocused terminal shows a static cursor
    /// (Terminal.app / iTerm convention). Safe to call repeatedly.
    pub fn set_cursor_blink_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.cursor_blink_enabled == enabled {
            return;
        }
        self.cursor_blink_enabled = enabled;
        // Disabling → snap cursor to solid-on immediately. Enabling keeps
        // whatever phase the timer was in; next tick will toggle normally.
        if !enabled {
            self.cursor_blink_on = true;
        }
        cx.notify();
    }

    fn utf16_len(s: &str) -> usize {
        s.chars().map(|ch| ch.len_utf16()).sum()
    }

    fn utf16_range_to_utf8(s: &str, range_utf16: Range<usize>) -> Option<Range<usize>> {
        let mut utf16_count = 0usize;
        let mut start_utf8: Option<usize> = None;
        let mut end_utf8: Option<usize> = None;

        if range_utf16.start == 0 {
            start_utf8 = Some(0);
        }
        if range_utf16.end == 0 {
            end_utf8 = Some(0);
        }

        for (utf8_index, ch) in s.char_indices() {
            if start_utf8.is_none() && utf16_count >= range_utf16.start {
                start_utf8 = Some(utf8_index);
            }
            if end_utf8.is_none() && utf16_count >= range_utf16.end {
                end_utf8 = Some(utf8_index);
            }

            utf16_count = utf16_count.saturating_add(ch.len_utf16());
        }

        if start_utf8.is_none() && utf16_count >= range_utf16.start {
            start_utf8 = Some(s.len());
        }
        if end_utf8.is_none() && utf16_count >= range_utf16.end {
            end_utf8 = Some(s.len());
        }

        Some(start_utf8?..end_utf8?)
    }

    fn cell_offset_for_utf16(text: &str, utf16_offset: usize) -> usize {
        use unicode_width::UnicodeWidthChar as _;

        let mut cells = 0usize;
        let mut utf16_count = 0usize;
        for ch in text.chars() {
            if utf16_count >= utf16_offset {
                break;
            }

            let len_utf16 = ch.len_utf16();
            if utf16_count.saturating_add(len_utf16) > utf16_offset {
                break;
            }
            utf16_count = utf16_count.saturating_add(len_utf16);

            let width = ch.width().unwrap_or(0);
            if width > 0 {
                cells = cells.saturating_add(width);
            }
        }
        cells
    }

    fn clear_marked_text(&mut self, cx: &mut Context<Self>) {
        self.marked_text = None;
        self.marked_selected_range_utf16 = 0..0;
        cx.notify();
    }

    fn set_marked_text(
        &mut self,
        text: String,
        selected_range_utf16: Option<Range<usize>>,
        cx: &mut Context<Self>,
    ) {
        if text.is_empty() {
            self.clear_marked_text(cx);
            return;
        }

        let total_utf16 = Self::utf16_len(&text);
        let selected = selected_range_utf16.unwrap_or(total_utf16..total_utf16);
        let selected = selected.start.min(total_utf16)..selected.end.min(total_utf16);

        self.marked_text = Some(SharedString::from(text));
        self.marked_selected_range_utf16 = selected;
        cx.notify();
    }

    fn commit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }

        self.send_input_parts(&[text.as_bytes()], cx);
    }

    fn send_input_parts(&mut self, parts: &[&[u8]], cx: &mut Context<Self>) {
        if parts.is_empty() {
            return;
        }

        if let Some(input) = self.input.as_ref() {
            for bytes in parts {
                input.send(bytes);
            }
            return;
        }

        for bytes in parts {
            let _ = self.session.feed(bytes);
        }
        self.apply_side_effects(cx);
        self.schedule_viewport_refresh(cx);
    }

    fn feed_output_bytes_to_session(&mut self, bytes: &[u8]) {
        if let Some(input) = self.input.as_ref() {
            let _ = self
                .session
                .feed_with_pty_responses(bytes, |resp| input.send(resp));
        } else {
            let _ = self.session.feed(bytes);
        }
    }

    fn sync_viewport_scroll_tracking(&mut self) {
        let _ = self.session.take_viewport_scroll_delta();
        // Any path that calls sync (wheel snap, page/home/end, resize) has
        // moved the viewport — invalidate the cached scrollbar position.
        self.scroll_pos_dirty = true;
        // Anything that calls this helper has just moved the VT to a fresh
        // line-aligned position (mouse wheel snap, page/home/end keys,
        // resize). The previous sub-line `pixel_offset` is no longer
        // meaningful — drop it so the next paint shows the new viewport
        // exactly on a line boundary. The mouse-wheel path explicitly
        // re-assigns the new offset after this call.
        if self.pixel_offset != 0.0 {
            self.pixel_offset = 0.0;
            self.peek_layout = None;
            self.peek_dirty = true;
        }
    }

    fn apply_viewport_scroll_delta(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        // Either the wheel handler or new PTY output moved the viewport —
        // the cached scrollbar position is now stale.
        self.scroll_pos_dirty = true;

        let rows = self.session.rows() as usize;
        if rows == 0 {
            return;
        }

        if self.viewport_lines.len() != rows || self.viewport_style_runs.len() != rows {
            self.refresh_viewport();
            return;
        }

        let delta_abs: usize = delta.unsigned_abs() as usize;
        if delta_abs == 0 {
            return;
        }
        if delta_abs >= rows {
            self.refresh_viewport();
            return;
        }

        let has_layouts = self.line_layouts.len() == rows;

        if delta > 0 {
            self.viewport_lines.rotate_left(delta_abs);
            self.viewport_style_runs.rotate_left(delta_abs);
            if has_layouts {
                self.line_layouts.rotate_left(delta_abs);
            }

            for idx in rows - delta_abs..rows {
                self.viewport_lines[idx].clear();
                self.viewport_style_runs[idx].clear();
                if has_layouts {
                    self.line_layouts[idx] = None;
                }
            }

            let dirty_rows: Vec<u16> = (rows - delta_abs..rows).map(|row| row as u16).collect();
            let _ = self.apply_dirty_viewport_rows(&dirty_rows);
            return;
        }

        self.viewport_lines.rotate_right(delta_abs);
        self.viewport_style_runs.rotate_right(delta_abs);
        if has_layouts {
            self.line_layouts.rotate_right(delta_abs);
        }

        for idx in 0..delta_abs {
            self.viewport_lines[idx].clear();
            self.viewport_style_runs[idx].clear();
            if has_layouts {
                self.line_layouts[idx] = None;
            }
        }

        let dirty_rows: Vec<u16> = (0..delta_abs).map(|row| row as u16).collect();
        let _ = self.apply_dirty_viewport_rows(&dirty_rows);
    }

    fn reconcile_dirty_viewport_after_output(&mut self) {
        let delta = self.session.take_viewport_scroll_delta();
        self.apply_viewport_scroll_delta(delta);

        let dirty = self.session.take_dirty_viewport_rows();
        if !dirty.is_empty() && !self.apply_dirty_viewport_rows(&dirty) {
            self.pending_refresh = true;
        }
    }

    fn with_refreshed_viewport(mut self) -> Self {
        self.refresh_viewport();
        self
    }

    fn refresh_viewport(&mut self) {
        let viewport = self.session.dump_viewport().unwrap_or_default();
        self.viewport_lines = split_viewport_lines(&viewport);
        self.viewport_line_offsets = Self::compute_viewport_line_offsets(&self.viewport_lines);
        self.viewport_total_len = Self::compute_viewport_total_len(&self.viewport_lines);
        self.viewport_style_runs = (0..self.session.rows())
            .map(|row| {
                self.session
                    .dump_viewport_row_style_runs(row)
                    .unwrap_or_default()
            })
            .collect();
        self.line_layouts.clear();
        self.line_layout_key = None;
        self.selection = None;
        // Viewport text changed — peek row above may also have shifted.
        // It'll be re-fetched lazily in prepaint when needed.
        self.peek_dirty = true;
        self.peek_layout = None;
        self.scroll_pos_dirty = true;
    }

    fn compute_viewport_line_offsets(lines: &[String]) -> Vec<usize> {
        let mut offsets = Vec::with_capacity(lines.len());
        let mut offset = 0usize;
        for line in lines {
            offsets.push(offset);
            offset = offset.saturating_add(line.len() + 1);
        }
        offsets
    }

    fn compute_viewport_total_len(lines: &[String]) -> usize {
        lines
            .iter()
            .fold(0usize, |acc, line| acc.saturating_add(line.len() + 1))
    }

    fn viewport_slice(&self, range: Range<usize>) -> String {
        if range.is_empty() || self.viewport_lines.is_empty() {
            return String::new();
        }

        let start = range.start.min(self.viewport_total_len);
        let end = range.end.min(self.viewport_total_len);
        if start >= end {
            return String::new();
        }

        let mut out = String::new();
        let mut i = 0usize;
        while i < self.viewport_lines.len() {
            let line_start = *self.viewport_line_offsets.get(i).unwrap_or(&0);
            let line = &self.viewport_lines[i];
            let line_end = line_start.saturating_add(line.len());
            let newline_pos = line_end;

            let seg_start = start.max(line_start);
            let seg_end = end.min(newline_pos.saturating_add(1));
            if seg_start < seg_end {
                let local_start = seg_start.saturating_sub(line_start);
                let local_end = seg_end.saturating_sub(line_start);
                let local_end = local_end.min(line.len().saturating_add(1));

                if local_start < line.len() {
                    let text_end = local_end.min(line.len());
                    if let Some(seg) = line.get(local_start..text_end) {
                        out.push_str(seg);
                    }
                }
                if local_end > line.len() {
                    out.push('\n');
                }
            }

            i += 1;
        }

        out
    }

    fn url_at_viewport_index(&self, index: usize) -> Option<String> {
        if self.viewport_lines.is_empty() {
            return None;
        }

        let idx = index.min(self.viewport_total_len.saturating_sub(1));
        let row = self
            .viewport_line_offsets
            .iter()
            .enumerate()
            .rfind(|(_, offset)| **offset <= idx)
            .map(|(i, _)| i)?;

        let line = self.viewport_lines.get(row)?.as_str();
        let line_start = *self.viewport_line_offsets.get(row).unwrap_or(&0);
        let local = idx
            .saturating_sub(line_start)
            .min(line.len().saturating_sub(1));
        url_at_byte_index(line, local)
    }

    fn apply_dirty_viewport_rows(&mut self, dirty_rows: &[u16]) -> bool {
        if dirty_rows.is_empty() {
            return false;
        }

        let expected_rows = self.session.rows() as usize;
        if self.viewport_lines.len() != expected_rows {
            self.refresh_viewport();
            return true;
        }
        if self.viewport_style_runs.len() != expected_rows {
            self.refresh_viewport();
            return true;
        }

        for &row in dirty_rows {
            let row = row as usize;
            if row >= self.viewport_lines.len() {
                continue;
            }

            let line = match self.session.dump_viewport_row(row as u16) {
                Ok(s) => s,
                Err(_) => {
                    self.refresh_viewport();
                    return true;
                }
            };

            let line = line.strip_suffix('\n').unwrap_or(line.as_str());
            self.viewport_lines[row].clear();
            self.viewport_lines[row].push_str(line);
            self.viewport_style_runs[row] = self
                .session
                .dump_viewport_row_style_runs(row as u16)
                .unwrap_or_default();
            if row < self.line_layouts.len() {
                self.line_layouts[row] = None;
            }
        }

        self.viewport_line_offsets = Self::compute_viewport_line_offsets(&self.viewport_lines);
        self.viewport_total_len = Self::compute_viewport_total_len(&self.viewport_lines);
        self.selection = None;
        true
    }

    fn schedule_viewport_refresh(&mut self, cx: &mut Context<Self>) {
        self.pending_refresh = true;
        cx.notify();
    }

    fn apply_side_effects(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.session.take_clipboard_write() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn feed_output_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.feed_output_bytes_to_session(bytes);
        self.apply_side_effects(cx);
        if self.session.synchronized_output_active() {
            self.pending_refresh = true;
        } else {
            self.refresh_viewport();
        }
        cx.notify();
    }

    pub fn queue_output_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        const MAX_PENDING_OUTPUT_BYTES: usize = 256 * 1024;

        // Reset blink so that cursor is visible on fresh output (e.g. while
        // user types and the shell echoes). Blink timer will resume toggling.
        self.cursor_blink_on = true;

        SCROLL_STATS.pty_input_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if self.pending_output.len().saturating_add(bytes.len()) <= MAX_PENDING_OUTPUT_BYTES {
            self.pending_output.extend_from_slice(bytes);
            SCROLL_STATS.pty_input_notifies.fetch_add(1, Ordering::Relaxed);
            cx.notify();
            return;
        }

        if !self.pending_output.is_empty() {
            let pending = std::mem::take(&mut self.pending_output);
            self.feed_output_bytes_to_session(&pending);
            self.apply_side_effects(cx);
            // Large output burst overflowed the pending buffer; dirty-row
            // reconciliation may not cover every affected row, so request a
            // full viewport refresh to avoid stale content.
            self.pending_refresh = true;
        }

        if bytes.len() > MAX_PENDING_OUTPUT_BYTES {
            let mut offset = 0usize;
            while offset < bytes.len() {
                let end = (offset + MAX_PENDING_OUTPUT_BYTES).min(bytes.len());
                self.feed_output_bytes_to_session(&bytes[offset..end]);
                offset = end;
            }
            self.apply_side_effects(cx);
            // Multiple large chunks were fed sequentially; the VT's dirty set
            // may only reflect the last chunk. Force a full refresh.
            self.pending_refresh = true;
            cx.notify();
            return;
        }

        self.pending_output.extend_from_slice(bytes);
        cx.notify();
    }

    pub fn resize_terminal(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let _ = self.session.resize(cols, rows);
        self.sync_viewport_scroll_tracking();
        self.pending_refresh = true;
        cx.notify();
    }

    /// Swap the terminal's default foreground and background colors at
    /// runtime.  Use to follow host-application theme changes without
    /// reconstructing the view (scrollback and cursor are preserved).
    ///
    /// Forces a full repaint so already-drawn cells that still use the
    /// default palette entry pick up the new colors immediately.
    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb, cx: &mut Context<Self>) {
        self.session.set_default_colors(fg, bg);
        self.pending_refresh = true;
        cx.notify();
    }

    /// Override the cursor color (`Some`) or restore auto-contrast (`None`).
    pub fn set_cursor_color(&mut self, color: Option<Rgb>, cx: &mut Context<Self>) {
        self.session.set_cursor_color(color);
        cx.notify();
    }

    /// Change the cursor shape (Block vs Bar).
    pub fn set_cursor_style(&mut self, style: crate::CursorStyle, cx: &mut Context<Self>) {
        self.session.set_cursor_style(style);
        cx.notify();
    }

    fn on_paste(&mut self, _: &Paste, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };

        if self.session.bracketed_paste_enabled() {
            self.send_input_parts(&[b"\x1b[200~", text.as_bytes(), b"\x1b[201~"], cx);
        } else {
            self.send_input_parts(&[text.as_bytes()], cx);
        }
    }

    fn on_copy(&mut self, _: &Copy, _window: &mut Window, cx: &mut Context<Self>) {
        let selection = self
            .selection
            .map(|s| s.range())
            .filter(|range| !range.is_empty())
            .map(|range| self.viewport_slice(range))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.viewport_slice(0..self.viewport_total_len));

        let item = ClipboardItem::new_string(selection.to_string());
        cx.write_to_clipboard(item.clone());
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        cx.write_to_primary(item);
    }

    fn on_select_all(&mut self, _: &SelectAll, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = Some(ByteSelection {
            anchor: 0,
            active: self.viewport_total_len,
        });
        self.on_copy(&Copy, window, cx);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);

        if event.first_mouse {
            return;
        }

        if event.button == MouseButton::Left && event.modifiers.platform {
            if let Some((col, row)) = self.mouse_position_to_cell(event.position, window) {
                if let Some(link) = self.session.hyperlink_at(col, row) {
                    let item = ClipboardItem::new_string(link);
                    cx.write_to_clipboard(item.clone());
                    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                    cx.write_to_primary(item);
                    return;
                }

                if let Some(line) = self.viewport_lines.get(row.saturating_sub(1) as usize)
                    && let Some(url) = url_at_column_in_line(line, col)
                {
                    let item = ClipboardItem::new_string(url);
                    cx.write_to_clipboard(item.clone());
                    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                    cx.write_to_primary(item);
                    return;
                }
            }

            if let Some(index) = self.mouse_position_to_viewport_index(event.position, window)
                && let Some(url) = self.url_at_viewport_index(index)
            {
                let item = ClipboardItem::new_string(url);
                cx.write_to_clipboard(item.clone());
                #[cfg(any(target_os = "linux", target_os = "freebsd"))]
                cx.write_to_primary(item);
                return;
            }
        }

        if event.modifiers.shift
            || self.input.is_none()
            || !self.session.mouse_reporting_enabled()
            || !self.session.mouse_sgr_enabled()
        {
            if event.button == MouseButton::Left
                && let Some(index) = self.mouse_position_to_viewport_index(event.position, window)
            {
                self.selection = Some(ByteSelection {
                    anchor: index,
                    active: index,
                });
                cx.notify();
            }
            return;
        }

        let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
            return;
        };

        if let Some(input) = self.input.as_ref() {
            let base_button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };

            let button_value = sgr_mouse_button_value(
                base_button,
                false,
                false,
                event.modifiers.alt,
                event.modifiers.control,
            );
            let seq = sgr_mouse_sequence(button_value, col, row, true);
            input.send(seq.as_bytes());
        }
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.shift
            || self.input.is_none()
            || !self.session.mouse_reporting_enabled()
            || !self.session.mouse_sgr_enabled()
        {
            if let Some(selection) = self.selection {
                if selection.range().is_empty() {
                    self.selection = None;
                }
                cx.notify();
            }
            return;
        }

        let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
            return;
        };

        if let Some(input) = self.input.as_ref() {
            let base_button = match event.button {
                MouseButton::Left => 0,
                MouseButton::Middle => 1,
                MouseButton::Right => 2,
                _ => return,
            };

            let button_value = sgr_mouse_button_value(
                base_button,
                false,
                false,
                event.modifiers.alt,
                event.modifiers.control,
            );
            let seq = sgr_mouse_sequence(button_value, col, row, false);
            input.send(seq.as_bytes());
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.shift
            && self.input.is_some()
            && self.session.mouse_reporting_enabled()
            && self.session.mouse_sgr_enabled()
        {
            let send_motion = if self.session.mouse_any_event_enabled() {
                true
            } else if self.session.mouse_button_event_enabled() {
                event.pressed_button.is_some()
            } else {
                false
            };

            if send_motion {
                let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
                    return;
                };

                let base_button = match event.pressed_button {
                    Some(MouseButton::Left) => 0,
                    Some(MouseButton::Middle) => 1,
                    Some(MouseButton::Right) => 2,
                    Some(_) => 3,
                    None => 3,
                };

                let button_value = sgr_mouse_button_value(
                    base_button,
                    true,
                    false,
                    event.modifiers.alt,
                    event.modifiers.control,
                );
                if let Some(input) = self.input.as_ref() {
                    let seq = sgr_mouse_sequence(button_value, col, row, true);
                    input.send(seq.as_bytes());
                }
                return;
            }
        }

        if !event.dragging() {
            return;
        }

        if self.selection.is_none() {
            return;
        }

        let Some(index) = self.mouse_position_to_viewport_index(event.position, window) else {
            return;
        };

        if let Some(selection) = self.selection.as_mut()
            && selection.active != index
        {
            selection.active = index;
            cx.notify();
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let raw_keystroke = event.keystroke.clone();
        if should_skip_key_down_for_ime(self.input.is_some(), &raw_keystroke) {
            return;
        }
        let keystroke = raw_keystroke.with_simulated_ime();

        if keystroke.modifiers.platform || keystroke.modifiers.function {
            return;
        }

        let scroll_step = (self.session.rows() as i32 / 2).max(1);

        if let Some(input) = self.input.as_ref() {
            if keystroke.modifiers.shift {
                match keystroke.key.as_str() {
                    "home" => {
                        let _ = self.session.scroll_viewport_top();
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "end" => {
                        let _ = self.session.scroll_viewport_bottom();
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "pageup" | "page_up" | "page-up" => {
                        let _ = self.session.scroll_viewport(-scroll_step);
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    "pagedown" | "page_down" | "page-down" => {
                        let _ = self.session.scroll_viewport(scroll_step);
                        self.sync_viewport_scroll_tracking();
                        self.apply_side_effects(cx);
                        self.schedule_viewport_refresh(cx);
                        return;
                    }
                    _ => {}
                }
            }

            if keystroke.modifiers.control
                && let Some(b) = ctrl_byte_for_keystroke(&keystroke)
            {
                input.send(&[b]);
                return;
            }

            if keystroke.modifiers.alt
                && let Some(text) = keystroke.key_char.as_deref()
            {
                input.send(&[0x1b]);
                input.send(text.as_bytes());
                return;
            }

            let modifiers = KeyModifiers {
                shift: keystroke.modifiers.shift,
                control: keystroke.modifiers.control,
                alt: keystroke.modifiers.alt,
                super_key: false,
            };
            if let Some(encoded) = encode_key_named(&keystroke.key, modifiers) {
                input.send(&encoded);
                return;
            }
            return;
        }

        match keystroke.key.as_str() {
            "home" => {
                let _ = self.session.scroll_viewport_top();
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "end" => {
                let _ = self.session.scroll_viewport_bottom();
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "pageup" | "page_up" | "page-up" => {
                let _ = self.session.scroll_viewport(-scroll_step);
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            "pagedown" | "page_down" | "page-down" => {
                let _ = self.session.scroll_viewport(scroll_step);
                self.sync_viewport_scroll_tracking();
                self.apply_side_effects(cx);
                self.schedule_viewport_refresh(cx);
                return;
            }
            _ => {}
        }

        let modifiers = KeyModifiers {
            shift: keystroke.modifiers.shift,
            control: keystroke.modifiers.control,
            alt: keystroke.modifiers.alt,
            super_key: false,
        };
        if let Some(encoded) = encode_key_named(&keystroke.key, modifiers) {
            let _ = self.session.feed(&encoded);
            self.apply_side_effects(cx);
            self.schedule_viewport_refresh(cx);
            return;
        }

        if keystroke.key == "backspace" {
            if let Some(input) = self.input.as_ref() {
                input.send(&[0x7f]);
                return;
            }
            let _ = self.session.feed(&[0x08]);
            self.apply_side_effects(cx);
            self.schedule_viewport_refresh(cx);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cell_h = cell_metrics_with_overrides(
            window,
            &self.font,
            self.session.font_size(),
            self.session.line_height_ratio(),
        )
        .map(|(_, h)| h)
        .unwrap_or(16.0);

        // Convert delta to pixels. ScrollDelta::Lines means an upstream OS
        // already mapped it to whole lines (rare on macOS trackpads); we
        // convert to the equivalent pixel count using the real cell height.
        let dy_px_in: f32 = match event.delta {
            ScrollDelta::Lines(p) => p.y * cell_h,
            ScrollDelta::Pixels(p) => f32::from(p.y),
        };

        // Sign convention used internally: positive = scroll INTO history
        // (peek revealed at top). Trackpad delta.y is positive when fingers
        // move up = page should reveal content above = into history.
        let dy_history = dy_px_in;

        // Mouse-reporting branch — remote app handles the scroll itself; we
        // pass discrete integer steps. No local pixel accumulation.
        if let Some(input) = self.input.as_ref()
            && !event.modifiers.shift
            && self.session.mouse_reporting_enabled()
            && self.session.mouse_sgr_enabled()
        {
            let delta_lines = (-dy_history / cell_h).round() as i32;
            if delta_lines == 0 {
                return;
            }
            let Some((col, row)) = self.mouse_position_to_cell(event.position, window) else {
                return;
            };
            let button = if delta_lines < 0 { 64 } else { 65 };
            let button_value = sgr_mouse_button_value(
                button,
                false,
                false,
                event.modifiers.alt,
                event.modifiers.control,
            );
            let steps = delta_lines.unsigned_abs().min(10);
            for _ in 0..steps {
                let seq = sgr_mouse_sequence(button_value, col, row, true);
                input.send(seq.as_bytes());
            }
            SCROLL_STATS.sgr_wheel_events.fetch_add(steps as u64, Ordering::Relaxed);
            SCROLL_STATS.events.fetch_add(1, Ordering::Relaxed);
            SCROLL_STATS.maybe_emit();
            return;
        }

        // Phase reset: a brand-new gesture starts at whatever sub-line offset
        // is currently visible — we don't snap to a line boundary, that's
        // exactly the "stickiness" we're trying to eliminate. We only drop
        // micro-noise (touch_phase::Started often arrives with delta = 0).
        if matches!(event.touch_phase, TouchPhase::Started) && dy_history.abs() < 0.5 {
            return;
        }

        self.apply_scroll_pixels(dy_history, cell_h, cx);
    }

    /// Update sub-line `pixel_offset` and snap the VT viewport to lines as
    /// needed. The bulk of trackpad events fall *within* a single line and
    /// only do `pixel_offset += dy + cx.notify()` — no VT calls, no text
    /// refetch. Only when crossing a line boundary do we touch the VT.
    fn apply_scroll_pixels(&mut self, dy_history_px: f32, cell_h: f32, cx: &mut Context<Self>) {
        if cell_h <= 0.0 {
            return;
        }

        // Velocity gate: at high scroll speeds (>1.5 lines per event), the
        // visual is dominated by line snaps — sub-pixel motion isn't
        // perceptible. Skip the peek-row render path entirely in that case
        // and snap straight to a line boundary. This keeps the cost of
        // pixel-smooth scroll bounded to slow/precise gestures (where the
        // user actually sees the smoothness) and matches OS-level fast-swipe
        // behaviour. Cuts ~10–13% CPU during fast trackpad swipes by
        // dropping the extra glyph row + quad submissions per frame.
        let fast = dy_history_px.abs() > cell_h * 1.5;
        if fast {
            // Drop any leftover sub-line offset and snap.
            let combined_px = self.pixel_offset + dy_history_px;
            let lines = (combined_px / cell_h).round() as i32;

            if lines != 0 {
                let _ = self.session.take_viewport_scroll_delta();
                let _ = self.session.scroll_viewport(-lines);
                let actual_delta = self.session.take_viewport_scroll_delta();
                self.apply_viewport_scroll_delta(actual_delta);
                let dirty_rows = self.session.take_dirty_viewport_rows();
                if !dirty_rows.is_empty() && !self.apply_dirty_viewport_rows(&dirty_rows) {
                    self.pending_refresh = true;
                }
                self.apply_side_effects(cx);
                cx.notify();
                if -actual_delta != lines {
                    SCROLL_STATS.boundary_clamps.fetch_add(1, Ordering::Relaxed);
                }
                SCROLL_STATS
                    .line_snaps
                    .fetch_add(lines.unsigned_abs() as u64, Ordering::Relaxed);
            }

            // Always reset offset (no peek to render, no sub-line state to keep).
            if self.pixel_offset != 0.0 {
                self.pixel_offset = 0.0;
                self.peek_layout = None;
                self.peek_dirty = true;
            }
            SCROLL_STATS.events.fetch_add(1, Ordering::Relaxed);
            SCROLL_STATS.maybe_emit();
            return;
        }

        let raw = self.pixel_offset + dy_history_px;
        let whole = (raw / cell_h).floor() as i32;
        let mut new_offset = raw - (whole as f32) * cell_h;

        let was_zero = self.pixel_offset == 0.0;
        let mut text_refresh_scheduled = false;
        let mut clamped_at_boundary = false;

        if whole != 0 {
            // `whole > 0` means `raw` advanced into history → scroll VT one
            // line *back* in scrollback per unit, i.e. scroll_viewport(-whole).
            // Wrap with delta clear/take so we can detect when the VT clamped
            // at a boundary (top of scrollback, or live screen with nothing
            // below). This replaces an earlier per-event `dump_screen_row(0)`
            // probe that allocated and freed a UTF-8 string on every event.
            let _ = self.session.take_viewport_scroll_delta();
            let _ = self.session.scroll_viewport(-whole);
            let actual_delta = self.session.take_viewport_scroll_delta();
            let actual_history_whole = -actual_delta;

            // Smart refresh: rotate the cached `viewport_lines` /
            // `line_layouts` by the actual delta and only re-fetch + re-shape
            // the rows that scrolled in (|delta| rows at top or bottom).
            // The previous path (`schedule_viewport_refresh`) cleared the
            // entire layout cache and forced a re-shape of every viewport
            // row on every line-snap — at fast trackpad speeds with
            // |whole|=7 and N=40 that's 33 wasted shapes per event. The
            // PTY-output path already uses this delta-rotate; we just
            // mirror it here.
            self.apply_viewport_scroll_delta(actual_delta);
            let dirty_rows = self.session.take_dirty_viewport_rows();
            if !dirty_rows.is_empty() && !self.apply_dirty_viewport_rows(&dirty_rows) {
                // Only fall back to full refresh if delta-aware patching
                // couldn't keep up (e.g. wide-char re-flow). Rare.
                self.pending_refresh = true;
                cx.notify();
            } else {
                cx.notify();
            }
            self.apply_side_effects(cx);
            self.peek_dirty = true;
            text_refresh_scheduled = true;

            if actual_history_whole != whole {
                clamped_at_boundary = true;
                SCROLL_STATS.boundary_clamps.fetch_add(1, Ordering::Relaxed);
            }
        }

        if clamped_at_boundary {
            // Hit a boundary (top of scrollback OR live mode bottom). The
            // visual position can't advance further in the requested
            // direction — snap to the nearest line boundary so we don't
            // rubber-band into peek territory (would show a "bounce" at the
            // bottom or empty strip at the top).
            new_offset = 0.0;
        }

        let offset_changed = (self.pixel_offset - new_offset).abs() > 0.001;
        if offset_changed {
            self.pixel_offset = new_offset;
            // Peek text only changes when the VT viewport moves *or* when we
            // newly enter peek territory (offset transitioned 0 → >0). Pure
            // sub-line offset changes within `(0, cell_h)` re-use the same
            // peek text, so don't dirty it then — that was a major source of
            // CPU during continuous trackpad scroll (every event re-fetched
            // and re-shaped the peek line).
            if was_zero && new_offset > 0.0 {
                self.peek_dirty = true;
            }
        }

        SCROLL_STATS.events.fetch_add(1, Ordering::Relaxed);
        if whole != 0 {
            SCROLL_STATS.line_snaps.fetch_add(whole.unsigned_abs() as u64, Ordering::Relaxed);
        } else if offset_changed {
            SCROLL_STATS.subline_paints.fetch_add(1, Ordering::Relaxed);
        }
        SCROLL_STATS.maybe_emit();

        if !text_refresh_scheduled && offset_changed {
            cx.notify();
        }
    }

    fn mouse_position_to_viewport_index(
        &self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
    ) -> Option<usize> {
        let rows = self.session.rows() as usize;
        if rows == 0 {
            return None;
        }

        let local_position = self.mouse_position_to_local(position);
        let (cell_width, cell_height) = cell_metrics_with_overrides(
            window,
            &self.font,
            self.session.font_size(),
            self.session.line_height_ratio(),
        )?;
        let x = f32::from(local_position.x);
        // Subtract pixel_offset so the row math operates in *content* space,
        // not *viewport* space. With the peek row visible at top, viewport
        // y=0 maps to content y=-pixel_offset (somewhere inside the peek).
        let y = f32::from(local_position.y) - self.pixel_offset;
        let mut row_index = (y / cell_height).floor() as i32;
        if row_index < 0 {
            row_index = 0;
        }
        if row_index >= rows as i32 {
            row_index = rows as i32 - 1;
        }
        let row_index = row_index as usize;

        if let Some(Some(line)) = self.line_layouts.get(row_index) {
            let byte_index = line
                .closest_index_for_x(px(x))
                .min(line.text.len());
            let offset = *self.viewport_line_offsets.get(row_index).unwrap_or(&0);
            return Some(offset.saturating_add(byte_index));
        }

        let cols = self.session.cols();
        let mut col = (x / cell_width).floor() as i32 + 1;
        let row = row_index as i32 + 1;
        if col < 1 { col = 1; }
        if col > cols as i32 { col = cols as i32; }
        let col = col as u16;
        let row_index = (row as u16).saturating_sub(1) as usize;
        let line = self.viewport_lines.get(row_index)?.as_str();
        let byte_index = byte_index_for_column_in_line(line, col).min(line.len());
        let offset = *self.viewport_line_offsets.get(row_index).unwrap_or(&0);
        Some(offset.saturating_add(byte_index))
    }

    fn mouse_position_to_cell(
        &self,
        position: gpui::Point<gpui::Pixels>,
        window: &mut Window,
    ) -> Option<(u16, u16)> {
        let cols = self.session.cols();
        let rows = self.session.rows();

        let position = self.mouse_position_to_local(position);
        let (cell_width, cell_height) = cell_metrics_with_overrides(
            window,
            &self.font,
            self.session.font_size(),
            self.session.line_height_ratio(),
        )?;
        let x = f32::from(position.x);
        let y = f32::from(position.y) - self.pixel_offset;

        let mut col = (x / cell_width).floor() as i32 + 1;
        let mut row = (y / cell_height).floor() as i32 + 1;

        if col < 1 {
            col = 1;
        }
        if row < 1 {
            row = 1;
        }
        if col > cols as i32 {
            col = cols as i32;
        }
        if row > rows as i32 {
            row = rows as i32;
        }

        Some((col as u16, row as u16))
    }

    fn mouse_position_to_local(
        &self,
        position: gpui::Point<gpui::Pixels>,
    ) -> gpui::Point<gpui::Pixels> {
        window_position_to_local(self.last_bounds, position)
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let text = self.marked_text.as_ref()?.as_str();
        let total_utf16 = Self::utf16_len(text);
        let start = range_utf16.start.min(total_utf16);
        let end = range_utf16.end.min(total_utf16);
        let range_utf16 = start..end;
        *adjusted_range = Some(range_utf16.clone());

        let range_utf8 = Self::utf16_range_to_utf8(text, range_utf16)?;
        Some(text.get(range_utf8)?.to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked_selected_range_utf16.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let text = self.marked_text.as_ref()?.as_str();
        let len = Self::utf16_len(text);
        (len > 0).then_some(0..len)
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.clear_marked_text(cx);
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clear_marked_text(cx);
        self.commit_text(text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_marked_text(new_text.to_string(), new_selected_range, cx);
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let (col, row) = self.session.cursor_position()?;
        let (cell_width, cell_height) = cell_metrics_with_overrides(
            window,
            &self.font,
            self.session.font_size(),
            self.session.line_height_ratio(),
        )?;

        let base_x = element_bounds.left() + px(cell_width * (col.saturating_sub(1)) as f32);
        let base_y = element_bounds.top()
            + px(cell_height * (row.saturating_sub(1)) as f32)
            + px(self.pixel_offset);

        let offset_cells = self
            .marked_text
            .as_ref()
            .map(|text| Self::cell_offset_for_utf16(text.as_str(), range_utf16.start))
            .unwrap_or(range_utf16.start);
        let x = base_x + px(cell_width * offset_cells as f32);
        Some(Bounds::new(
            point(x, base_y),
            size(px(cell_width), px(cell_height)),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

struct TerminalPrepaintState {
    line_height: Pixels,
    /// Y-shift applied to *all* viewport rows + cursor + selection during
    /// paint. Encodes the sub-line scroll offset so the screen slides
    /// pixel-by-pixel between line boundaries. Always in `[0, line_height)`.
    pixel_offset: Pixels,
    shaped_lines: Vec<gpui::ShapedLine>,
    background_quads: Vec<PaintQuad>,
    selection_quads: Vec<PaintQuad>,
    box_drawing_quads: Vec<PaintQuad>,
    marked_text: Option<(gpui::ShapedLine, gpui::Point<Pixels>)>,
    marked_text_background: Option<PaintQuad>,
    cursor: Option<PaintQuad>,
    /// Optional bleed-row shown above viewport row 0 when `pixel_offset > 0`.
    /// All three vectors are in *content* coords (no pixel_offset applied);
    /// paint translates them by `(0, pixel_offset - line_height)`.
    peek_line: Option<gpui::ShapedLine>,
    peek_background_quads: Vec<PaintQuad>,
    peek_box_drawing_quads: Vec<PaintQuad>,
    /// Scrollbar track + thumb quads, drawn on the right edge when there is
    /// scrollback above the visible area. `None` when the screen fits
    /// entirely in the viewport (no scrolling possible). Painted *after*
    /// content + cursor, on top, without `pixel_offset` translation since
    /// the scrollbar lives in viewport-relative space.
    scrollbar: Option<ScrollbarQuads>,
}

struct ScrollbarQuads {
    track: PaintQuad,
    thumb: PaintQuad,
}

const CELL_STYLE_FLAG_BOLD: u8 = 0x02;
const CELL_STYLE_FLAG_ITALIC: u8 = 0x04;
const CELL_STYLE_FLAG_UNDERLINE: u8 = 0x08;
const CELL_STYLE_FLAG_FAINT: u8 = 0x10;
const CELL_STYLE_FLAG_STRIKETHROUGH: u8 = 0x40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextRunKey {
    fg: Rgb,
    flags: u8,
}

fn hsla_from_rgb(rgb: Rgb) -> gpui::Hsla {
    let rgba = gpui::Rgba {
        r: rgb.r as f32 / 255.0,
        g: rgb.g as f32 / 255.0,
        b: rgb.b as f32 / 255.0,
        a: 1.0,
    };
    rgba.into()
}

fn cursor_color_for_background(background: Rgb) -> gpui::Hsla {
    let bg = hsla_from_rgb(background);
    let mut cursor = if bg.l > 0.6 {
        gpui::black()
    } else {
        gpui::white()
    };
    cursor.a = 0.72;
    cursor
}

fn font_for_flags(base: &gpui::Font, flags: u8) -> gpui::Font {
    let mut font = base.clone();
    if flags & CELL_STYLE_FLAG_BOLD != 0 {
        font = font.bold();
    }
    if flags & CELL_STYLE_FLAG_ITALIC != 0 {
        font = font.italic();
    }
    font
}

fn color_for_key(key: TextRunKey) -> gpui::Hsla {
    let mut color = hsla_from_rgb(key.fg);
    if key.flags & CELL_STYLE_FLAG_FAINT != 0 {
        color = color.alpha(0.65);
    }
    color
}

pub(crate) const BOX_DIR_LEFT: u8 = 0x01;
pub(crate) const BOX_DIR_RIGHT: u8 = 0x02;
pub(crate) const BOX_DIR_UP: u8 = 0x04;
pub(crate) const BOX_DIR_DOWN: u8 = 0x08;

pub(crate) fn box_drawing_mask(ch: char) -> Option<(u8, f32)> {
    let light = 1.0;
    let heavy = 1.35;
    let double = 1.15;

    let mask = match ch {
        '─' | '━' | '═' => BOX_DIR_LEFT | BOX_DIR_RIGHT,
        '│' | '┃' | '║' => BOX_DIR_UP | BOX_DIR_DOWN,
        '┌' | '┏' | '╔' | '╭' => BOX_DIR_RIGHT | BOX_DIR_DOWN,
        '┐' | '┓' | '╗' | '╮' => BOX_DIR_LEFT | BOX_DIR_DOWN,
        '└' | '┗' | '╚' | '╰' => BOX_DIR_RIGHT | BOX_DIR_UP,
        '┘' | '┛' | '╝' | '╯' => BOX_DIR_LEFT | BOX_DIR_UP,
        '├' | '┣' | '╠' => BOX_DIR_RIGHT | BOX_DIR_UP | BOX_DIR_DOWN,
        '┤' | '┫' | '╣' => BOX_DIR_LEFT | BOX_DIR_UP | BOX_DIR_DOWN,
        '┬' | '┳' | '╦' => BOX_DIR_LEFT | BOX_DIR_RIGHT | BOX_DIR_DOWN,
        '┴' | '┻' | '╩' => BOX_DIR_LEFT | BOX_DIR_RIGHT | BOX_DIR_UP,
        '┼' | '╋' | '╬' => BOX_DIR_LEFT | BOX_DIR_RIGHT | BOX_DIR_UP | BOX_DIR_DOWN,
        _ => return None,
    };

    let scale = match ch {
        '━' | '┃' | '┏' | '┓' | '┗' | '┛' | '┣' | '┫' | '┳' | '┻' | '╋' => {
            heavy
        }
        '═' | '║' | '╔' | '╗' | '╚' | '╝' | '╠' | '╣' | '╦' | '╩' | '╬' => {
            double
        }
        _ => light,
    };

    Some((mask, scale))
}

fn box_drawing_quads_for_char(
    bounds: Bounds<Pixels>,
    line_height: Pixels,
    cell_width: f32,
    color: gpui::Hsla,
    ch: char,
) -> Vec<PaintQuad> {
    let Some((mask, scale)) = box_drawing_mask(ch) else {
        return Vec::new();
    };

    let x0 = bounds.left();
    let x1 = x0 + px(cell_width);
    let y0 = bounds.top();
    let y1 = y0 + line_height;

    let mid_x = x0 + px(cell_width * 0.5);
    let mid_y = y0 + line_height * 0.5;

    let thickness = px(((f32::from(line_height) / 12.0).max(1.0) * scale).max(1.0));
    let half_t = thickness * 0.5;

    let has_left = mask & BOX_DIR_LEFT != 0;
    let has_right = mask & BOX_DIR_RIGHT != 0;
    let has_up = mask & BOX_DIR_UP != 0;
    let has_down = mask & BOX_DIR_DOWN != 0;

    let mut quads = Vec::new();

    if has_left || has_right {
        let (start_x, end_x) = if has_left && has_right {
            (x0, x1)
        } else if has_left {
            (x0, mid_x)
        } else {
            (mid_x, x1)
        };
        quads.push(fill(
            Bounds::from_corners(point(start_x, mid_y - half_t), point(end_x, mid_y + half_t)),
            color,
        ));
    }

    if has_up || has_down {
        let (start_y, end_y) = if has_up && has_down {
            (y0, y1)
        } else if has_up {
            (y0, mid_y)
        } else {
            (mid_y, y1)
        };

        quads.push(fill(
            Bounds::from_corners(point(mid_x - half_t, start_y), point(mid_x + half_t, end_y)),
            color,
        ));
    }

    quads
}

/// Returns `true` for Unicode block / shade / eighth-block characters.
///
/// These characters are conventionally rendered by terminal emulators as
/// filled quads of specific fractions of the cell. Menlo (and many fonts)
/// size their block glyphs to the em-box, which is smaller than the
/// line height — this leaves horizontal gaps between rows where the glyph
/// doesn't reach. Rendering them as quads that fill exactly the computed
/// cell bounds eliminates those stripes.
pub(crate) fn is_block_char(ch: char) -> bool {
    matches!(
        ch,
        '█' | '▀' | '▄' | '▌' | '▐'
            | '▁' | '▂' | '▃' | '▅' | '▆' | '▇'
            | '▏' | '▎' | '▍' | '▋' | '▊' | '▉'
            | '▔' | '▕'
            | '▖' | '▗' | '▘' | '▝' | '▙' | '▟' | '▛' | '▜' | '▚' | '▞'
    )
}

/// Render block / shade / eighth-block characters as filled quads.
///
/// Returns `None` for chars that aren't block drawing → caller falls back
/// to normal glyph rendering.
fn block_char_quads_for_char(
    cell: Bounds<Pixels>,
    line_height: Pixels,
    cell_width: f32,
    color: gpui::Hsla,
    ch: char,
) -> Option<Vec<PaintQuad>> {
    let x0 = cell.left();
    let y0 = cell.top();
    let w = px(cell_width);
    let h = line_height;

    // Helper: fill a fractional sub-rectangle of the cell.
    // frac_x, frac_y, frac_w, frac_h are all in [0, 1] of the cell.
    let rect = |fx: f32, fy: f32, fw: f32, fh: f32| -> PaintQuad {
        let ox = x0 + w * fx;
        let oy = y0 + h * fy;
        fill(
            Bounds::new(point(ox, oy), size(w * fw, h * fh)),
            color,
        )
    };

    let quads: Vec<PaintQuad> = match ch {
        // Full block
        '█' => vec![rect(0., 0., 1., 1.)],

        // Half blocks
        '▀' => vec![rect(0., 0., 1., 0.5)],            // upper half
        '▄' => vec![rect(0., 0.5, 1., 0.5)],           // lower half
        '▌' => vec![rect(0., 0., 0.5, 1.)],            // left half
        '▐' => vec![rect(0.5, 0., 0.5, 1.)],           // right half

        // Lower eighths (▁ = 1/8, ▂ = 2/8, ..., ▇ = 7/8, ▄ already above = 4/8)
        '▁' => vec![rect(0., 7. / 8., 1., 1. / 8.)],
        '▂' => vec![rect(0., 6. / 8., 1., 2. / 8.)],
        '▃' => vec![rect(0., 5. / 8., 1., 3. / 8.)],
        '▅' => vec![rect(0., 3. / 8., 1., 5. / 8.)],
        '▆' => vec![rect(0., 2. / 8., 1., 6. / 8.)],
        '▇' => vec![rect(0., 1. / 8., 1., 7. / 8.)],

        // Left eighths (▏ = 1/8, ▎ = 2/8, ..., ▉ = 7/8, ▌ already = 4/8)
        '▏' => vec![rect(0., 0., 1. / 8., 1.)],
        '▎' => vec![rect(0., 0., 2. / 8., 1.)],
        '▍' => vec![rect(0., 0., 3. / 8., 1.)],
        '▋' => vec![rect(0., 0., 5. / 8., 1.)],
        '▊' => vec![rect(0., 0., 6. / 8., 1.)],
        '▉' => vec![rect(0., 0., 7. / 8., 1.)],

        // Upper block variants
        '▔' => vec![rect(0., 0., 1., 1. / 8.)],         // upper one eighth block
        '▕' => vec![rect(7. / 8., 0., 1. / 8., 1.)],    // right one eighth block

        // Shaded blocks (░▒▓) — skipped: translucent quads would blend
        // with the underlying glyph. Font renderer handles them.

        // Quadrants
        '▖' => vec![rect(0., 0.5, 0.5, 0.5)],          // lower-left
        '▗' => vec![rect(0.5, 0.5, 0.5, 0.5)],         // lower-right
        '▘' => vec![rect(0., 0., 0.5, 0.5)],           // upper-left
        '▝' => vec![rect(0.5, 0., 0.5, 0.5)],          // upper-right
        '▙' => vec![rect(0., 0., 0.5, 1.), rect(0.5, 0.5, 0.5, 0.5)],
        '▟' => vec![rect(0.5, 0., 0.5, 1.), rect(0., 0.5, 0.5, 0.5)],
        '▛' => vec![rect(0., 0., 1., 0.5), rect(0., 0.5, 0.5, 0.5)],
        '▜' => vec![rect(0., 0., 1., 0.5), rect(0.5, 0.5, 0.5, 0.5)],
        '▚' => vec![rect(0., 0., 0.5, 0.5), rect(0.5, 0.5, 0.5, 0.5)],
        '▞' => vec![rect(0.5, 0., 0.5, 0.5), rect(0., 0.5, 0.5, 0.5)],

        _ => return None,
    };

    Some(quads)
}

fn text_run_for_key(base_font: &gpui::Font, key: TextRunKey, len: usize) -> TextRun {
    let font = font_for_flags(base_font, key.flags);
    let color = color_for_key(key);

    let underline = (key.flags & CELL_STYLE_FLAG_UNDERLINE != 0).then_some(UnderlineStyle {
        color: Some(color),
        thickness: px(1.0),
        wavy: false,
    });

    let strikethrough =
        (key.flags & CELL_STYLE_FLAG_STRIKETHROUGH != 0).then_some(gpui::StrikethroughStyle {
            color: Some(color),
            thickness: px(1.0),
        });

    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline,
        strikethrough,
    }
}

pub(crate) fn byte_index_for_column_in_line(line: &str, col: u16) -> usize {
    use unicode_width::UnicodeWidthChar as _;

    let col = col.max(1) as usize;
    if col == 1 {
        return 0;
    }

    let mut current_col = 1usize;
    for (byte_index, ch) in line.char_indices() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            continue;
        }

        if current_col == col {
            return byte_index;
        }

        let next_col = current_col.saturating_add(width);
        if col < next_col {
            return byte_index;
        }

        current_col = next_col;
    }

    line.len()
}

/// Build the GPUI [`TextRun`] sequence for a single terminal row from its
/// raw text and per-cell style runs. Extracted so it can be shared by the
/// main viewport shaping loop and the peek-row shaper.
fn build_runs_for_line(
    text: &str,
    style_runs: &[StyleRun],
    run_font: &gpui::Font,
    run_color: gpui::Hsla,
) -> Vec<TextRun> {
    let mut runs: Vec<TextRun> = Vec::new();
    if !style_runs.is_empty() {
        let mut byte_pos = 0usize;
        for style in style_runs.iter() {
            let key = TextRunKey {
                fg: style.fg,
                flags: style.flags
                    & (CELL_STYLE_FLAG_BOLD
                        | CELL_STYLE_FLAG_ITALIC
                        | CELL_STYLE_FLAG_UNDERLINE
                        | CELL_STYLE_FLAG_FAINT
                        | CELL_STYLE_FLAG_STRIKETHROUGH),
            };

            let start = byte_index_for_column_in_line(text, style.start_col).min(text.len());
            let end = byte_index_for_column_in_line(text, style.end_col.saturating_add(1))
                .min(text.len());

            if start > byte_pos {
                runs.push(TextRun {
                    len: start.saturating_sub(byte_pos),
                    font: run_font.clone(),
                    color: run_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                });
                byte_pos = start;
            }

            if end > start {
                runs.push(text_run_for_key(run_font, key, end.saturating_sub(start)));
                byte_pos = end;
            }
        }

        if byte_pos < text.len() {
            runs.push(TextRun {
                len: text.len().saturating_sub(byte_pos),
                font: run_font.clone(),
                color: run_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            });
        }
    }

    if runs.is_empty() {
        runs.push(TextRun {
            len: text.len(),
            font: run_font.clone(),
            color: run_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }

    runs
}

/// Background-color quads for a single row at vertical offset `y`.
/// Skips runs whose bg matches `default_bg` (those are painted by the
/// element's outer fill).
fn build_bg_quads_for_row(
    style_runs: &[StyleRun],
    default_bg: Rgb,
    origin_x: Pixels,
    y: Pixels,
    cell_width: f32,
    line_height: Pixels,
    out: &mut Vec<PaintQuad>,
) {
    if style_runs.is_empty() {
        return;
    }
    for run in style_runs.iter() {
        if run.bg == default_bg {
            continue;
        }
        let x = origin_x + px(cell_width * (run.start_col.saturating_sub(1)) as f32);
        let w = px(cell_width
            * (run.end_col.saturating_sub(run.start_col).saturating_add(1)) as f32);
        let color = rgba(
            (u32::from(run.bg.r) << 24)
                | (u32::from(run.bg.g) << 16)
                | (u32::from(run.bg.b) << 8)
                | 0xFF,
        );
        out.push(fill(Bounds::new(point(x, y), size(w, line_height)), color));
    }
}

/// Box-drawing / block-character quads for a single row at vertical offset
/// `y`. These are foreground glyphs that we draw as quads instead of
/// shaping through text (sub-pixel alignment hassle, charset coverage gaps).
fn build_box_quads_for_row(
    line: &str,
    style_runs: Option<&[StyleRun]>,
    default_fg: gpui::Hsla,
    origin_x: Pixels,
    y: Pixels,
    cell_width: f32,
    line_height: Pixels,
    out: &mut Vec<PaintQuad>,
) {
    use unicode_width::UnicodeWidthChar as _;
    let mut run_idx: usize = 0;
    let mut col = 1usize;
    for ch in line.chars() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            continue;
        }

        let is_box = box_drawing_mask(ch).is_some();
        let is_block = is_block_char(ch);

        if is_box || is_block {
            let fg = style_runs
                .and_then(|runs| {
                    while let Some(run) = runs.get(run_idx) {
                        if (col as u16) <= run.end_col {
                            break;
                        }
                        run_idx = run_idx.saturating_add(1);
                    }
                    runs.get(run_idx).and_then(|run| {
                        (col as u16 >= run.start_col && (col as u16) <= run.end_col).then_some(run)
                    })
                })
                .map(|run| {
                    let key = TextRunKey {
                        fg: run.fg,
                        flags: run.flags
                            & (CELL_STYLE_FLAG_FAINT
                                | CELL_STYLE_FLAG_BOLD
                                | CELL_STYLE_FLAG_ITALIC
                                | CELL_STYLE_FLAG_UNDERLINE
                                | CELL_STYLE_FLAG_STRIKETHROUGH),
                    };
                    color_for_key(key)
                })
                .unwrap_or(default_fg);

            let x = origin_x + px(cell_width * (col.saturating_sub(1)) as f32);
            let cell_bounds = Bounds::new(point(x, y), size(px(cell_width), line_height));

            if is_box {
                out.extend(box_drawing_quads_for_char(
                    cell_bounds,
                    line_height,
                    cell_width,
                    fg,
                    ch,
                ));
            } else if let Some(block_quads) =
                block_char_quads_for_char(cell_bounds, line_height, cell_width, fg, ch)
            {
                out.extend(block_quads);
            }
        }

        col = col.saturating_add(width);
    }
}

/// Build a track + proportional thumb for the right-side scrollbar.
///
/// Layout: a 6 px track flush to the right edge with a 1 px gap, semi-
/// transparent so it doesn't compete with terminal content. The thumb's
/// height is proportional to `viewport_rows / total_rows` (with a 24 px
/// minimum so it stays grabbable on tall scrollbacks) and its top is
/// proportional to `viewport_top / scrollable_range`.
fn build_scrollbar_quads(
    bounds: Bounds<Pixels>,
    pos: ghostty_vt::ScrollPosition,
) -> ScrollbarQuads {
    const TRACK_WIDTH: f32 = 6.0;
    const TRACK_RIGHT_GAP: f32 = 1.0;
    const MIN_THUMB_HEIGHT: f32 = 24.0;

    let track_x = bounds.right() - px(TRACK_WIDTH + TRACK_RIGHT_GAP);
    let track_w = px(TRACK_WIDTH);
    let track_y = bounds.top();
    let track_h = bounds.size.height;

    let total = pos.total_rows.max(1) as f32;
    let viewport = pos.viewport_rows.max(1) as f32;
    let scrollable = (total - viewport).max(1.0);

    let h_px = f32::from(track_h);
    let raw_thumb_h = (viewport / total) * h_px;
    let thumb_h = raw_thumb_h.max(MIN_THUMB_HEIGHT).min(h_px);

    let progress = (pos.viewport_top as f32 / scrollable).clamp(0.0, 1.0);
    let thumb_y_offset = progress * (h_px - thumb_h);

    // Subtle achromatic bar that adapts to the terminal's default
    // background — blended over whatever's underneath, so it works on
    // dark and light themes without theming. The thumb is the same hue
    // family but considerably more opaque so the contrast against the
    // track is what reads, not the absolute brightness.
    let track_color = hsla(0.0, 0.0, 0.5, 0.18);
    let thumb_color = hsla(0.0, 0.0, 0.5, 0.55);

    let track = fill(
        Bounds::new(point(track_x, track_y), size(track_w, track_h)),
        track_color,
    )
    .corner_radii(px(TRACK_WIDTH * 0.5));

    let thumb = fill(
        Bounds::new(
            point(track_x, track_y + px(thumb_y_offset)),
            size(track_w, px(thumb_h)),
        ),
        thumb_color,
    )
    .corner_radii(px(TRACK_WIDTH * 0.5));

    ScrollbarQuads { track, thumb }
}

struct TerminalTextElement {
    view: gpui::Entity<TerminalView>,
}

impl IntoElement for TerminalTextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalTextElement {
    type RequestLayoutState = ();
    type PrepaintState = TerminalPrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let _prepaint_start = Instant::now();
        // Update last_bounds early (during prepaint) so that mouse event
        // handlers on the current frame have accurate bounds for coordinate
        // conversion. Without this, selection coordinates are offset by the
        // element's position after layout changes (e.g. pane switches).
        self.view.update(cx, |view, _cx| {
            view.last_bounds = Some(bounds);
        });

        let (font, font_size_override, line_height_ratio_override, default_fg) = {
            let view = self.view.read(cx);
            (
                view.font.clone(),
                view.session.font_size(),
                view.session.line_height_ratio(),
                view.session.default_foreground(),
            )
        };
        let mut style = window.text_style();
        style.font_family = font.family.clone();
        style.font_features = crate::default_terminal_font_features();
        style.font_fallbacks = font.fallbacks.clone();
        if let Some(fs) = font_size_override {
            style.font_size = px(fs).into();
        }
        if let Some(ratio) = line_height_ratio_override {
            style.line_height = relative(ratio);
        }
        style.color = hsla_from_rgb(default_fg);
        let rem_size = window.rem_size();
        let font_size = style.font_size.to_pixels(rem_size);
        let line_height = style.line_height.to_pixels(style.font_size, rem_size);

        let run_font = style.font();
        let run_color = style.color;

        let cell_width = cell_metrics_with_overrides(
            window,
            &font,
            font_size_override,
            line_height_ratio_override,
        )
        .map(|(w, _)| px(w));

        self.view.update(cx, |view, _cx| {
            // Sub-line scroll: refresh the peek-row text/style if it's stale
            // and currently visible. We do this here (not in render) so that
            // the same prepaint pass shapes the peek line below.
            if view.pixel_offset > 0.0 && view.peek_dirty {
                view.peek_text = view
                    .session
                    .dump_screen_row(0)
                    .ok()
                    .flatten();
                view.peek_runs = view
                    .session
                    .dump_screen_row_style_runs(0)
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                view.peek_layout = None;
                view.peek_dirty = false;
                SCROLL_STATS.peek_fetches.fetch_add(1, Ordering::Relaxed);
                if view.peek_text.is_none() {
                    // Scrollback exhausted while we were waiting to render —
                    // drop the offset so we don't show an empty strip.
                    view.pixel_offset = 0.0;
                }
            }

            if view.viewport_lines.is_empty() {
                view.line_layouts.clear();
                view.line_layout_key = None;
                view.peek_layout = None;
                return;
            }

            // Font/size change → invalidate every cached layout (viewport + peek).
            let key_changed = view.line_layout_key != Some((font_size, line_height));
            if key_changed
                || view.line_layouts.len() != view.viewport_lines.len()
            {
                view.line_layout_key = Some((font_size, line_height));
                view.line_layouts = vec![None; view.viewport_lines.len()];
                view.peek_layout = None;
            }

            for (idx, line) in view.viewport_lines.iter().enumerate() {
                let Some(slot) = view.line_layouts.get_mut(idx) else {
                    continue;
                };

                if let Some(existing) = slot.as_ref()
                    && existing.text.as_str() == line.as_str()
                {
                    continue;
                }

                let text = SharedString::from(line.clone());
                let runs = build_runs_for_line(
                    text.as_str(),
                    view.viewport_style_runs.get(idx).map(|v| v.as_slice()).unwrap_or(&[]),
                    &run_font,
                    run_color,
                );

                let force_width = cell_width.and_then(|cell_width| {
                    use unicode_width::UnicodeWidthChar as _;
                    let has_wide = text.as_str().chars().any(|ch| ch.width().unwrap_or(0) > 1);
                    (!has_wide).then_some(cell_width)
                });
                let shaped = window
                    .text_system()
                    .shape_line(text, font_size, &runs, force_width);
                *slot = Some(shaped);
            }

            // Shape the peek row (above viewport top) when it's both visible
            // (`pixel_offset > 0`) and either un-shaped or stale.
            if view.pixel_offset > 0.0
                && let Some(peek_text) = view.peek_text.clone()
            {
                let needs_shape = view
                    .peek_layout
                    .as_ref()
                    .map(|l| l.text.as_str() != peek_text.as_str())
                    .unwrap_or(true);
                if needs_shape {
                    let text = SharedString::from(peek_text);
                    let runs = build_runs_for_line(
                        text.as_str(),
                        &view.peek_runs,
                        &run_font,
                        run_color,
                    );
                    let force_width = cell_width.and_then(|cell_width| {
                        use unicode_width::UnicodeWidthChar as _;
                        let has_wide =
                            text.as_str().chars().any(|ch| ch.width().unwrap_or(0) > 1);
                        (!has_wide).then_some(cell_width)
                    });
                    view.peek_layout = Some(window.text_system().shape_line(
                        text,
                        font_size,
                        &runs,
                        force_width,
                    ));
                    SCROLL_STATS.peek_reshapes.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                view.peek_layout = None;
            }
        });

        let default_bg = { self.view.read(cx).session.default_background() };
        let cell_w_metric = cell_metrics_with_overrides(
            window,
            &font,
            font_size_override,
            line_height_ratio_override,
        )
        .map(|(w, _)| w);
        let background_quads = cell_w_metric
            .map(|cell_width| {
                let origin = bounds.origin;
                let mut quads: Vec<PaintQuad> = Vec::new();

                let view = self.view.read(cx);
                for (row, runs) in view.viewport_style_runs.iter().enumerate() {
                    let y = origin.y + line_height * row as f32;
                    build_bg_quads_for_row(
                        runs,
                        default_bg,
                        origin.x,
                        y,
                        cell_width,
                        line_height,
                        &mut quads,
                    );
                }

                quads
            })
            .unwrap_or_default();

        // Peek-row background quads. Painted at content row "−1" (above
        // viewport row 0). The same `pixel_offset` translation that's
        // applied to viewport quads in paint also applies here.
        let peek_background_quads = cell_w_metric
            .map(|cell_width| {
                let mut quads: Vec<PaintQuad> = Vec::new();
                let view = self.view.read(cx);
                if view.pixel_offset > 0.0 && !view.peek_runs.is_empty() {
                    let y = bounds.origin.y - line_height;
                    build_bg_quads_for_row(
                        &view.peek_runs,
                        default_bg,
                        bounds.origin.x,
                        y,
                        cell_width,
                        line_height,
                        &mut quads,
                    );
                }
                quads
            })
            .unwrap_or_default();

        let (shaped_lines, selection, line_offsets) = {
            let view = self.view.read(cx);
            (
                view.line_layouts
                    .iter()
                    .map(|line| line.clone().unwrap_or_default())
                    .collect::<Vec<_>>(),
                view.selection,
                view.viewport_line_offsets.clone(),
            )
        };

        let (marked_text, cursor_position, font) = {
            let view = self.view.read(cx);
            (
                view.marked_text.clone(),
                view.session.cursor_position(),
                view.font.clone(),
            )
        };

        let (marked_text, marked_text_background) = marked_text
            .and_then(|text| {
                if text.is_empty() {
                    return None;
                }
                let (col, row) = cursor_position?;
                let (cell_width, _) = cell_metrics_with_overrides(window, &font, font_size_override, line_height_ratio_override)?;

                let origin_x = bounds.left() + px(cell_width * (col.saturating_sub(1)) as f32);
                let origin_y = bounds.top() + line_height * (row.saturating_sub(1)) as f32;
                let origin = point(origin_x, origin_y);

                let run = TextRun {
                    len: text.len(),
                    font: run_font.clone(),
                    color: run_color,
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        color: Some(run_color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                let force_width = {
                    use unicode_width::UnicodeWidthChar as _;
                    let has_wide = text.as_str().chars().any(|ch| ch.width().unwrap_or(0) > 1);
                    (!has_wide).then_some(px(cell_width))
                };
                let shaped =
                    window
                        .text_system()
                        .shape_line(text.clone(), font_size, &[run], force_width);

                let bg = {
                    let view = self.view.read(cx);
                    let row_index = row.saturating_sub(1) as usize;
                    view.viewport_style_runs
                        .get(row_index)
                        .and_then(|runs| {
                            runs.iter().find_map(|run| {
                                (col >= run.start_col && col <= run.end_col).then_some(run.bg)
                            })
                        })
                        .unwrap_or(default_bg)
                };

                let cell_len = {
                    use unicode_width::UnicodeWidthChar as _;
                    let mut cells = 0usize;
                    for ch in text.as_str().chars() {
                        let w = ch.width().unwrap_or(0);
                        if w > 0 {
                            cells = cells.saturating_add(w);
                        }
                    }
                    cells.max(1)
                };

                let marked_text_background = fill(
                    Bounds::new(origin, size(px(cell_width * cell_len as f32), line_height)),
                    rgba(
                        (u32::from(bg.r) << 24)
                            | (u32::from(bg.g) << 16)
                            | (u32::from(bg.b) << 8)
                            | 0xFF,
                    ),
                );

                Some(((shaped, origin), marked_text_background))
            })
            .map(|(text, bg)| (Some(text), Some(bg)))
            .unwrap_or((None, None));

        let selection_quads = selection
            .map(|sel| sel.range())
            .filter(|range| !range.is_empty())
            .map(|range| {
                let highlight = hsla(0.58, 0.9, 0.55, 0.35);
                let mut quads = Vec::new();

                for (row, line) in shaped_lines.iter().enumerate() {
                    let Some(&line_offset) = line_offsets.get(row) else {
                        continue;
                    };

                    let line_start = line_offset;
                    let line_end = line_offset.saturating_add(line.text.len());

                    let seg_start = range.start.max(line_start).min(line_end);
                    let seg_end = range.end.max(line_start).min(line_end);
                    if seg_start >= seg_end {
                        continue;
                    }

                    let local_start = seg_start.saturating_sub(line_start);
                    let local_end = seg_end.saturating_sub(line_start);

                    let x1 = line.x_for_index(local_start);
                    let x2 = line.x_for_index(local_end);

                    let y1 = bounds.top() + line_height * row as f32;
                    let y2 = y1 + line_height;

                    quads.push(fill(
                        Bounds::from_corners(
                            point(bounds.left() + x1, y1),
                            point(bounds.left() + x2, y2),
                        ),
                        highlight,
                    ));
                }

                quads
            })
            .unwrap_or_default();

        let box_drawing_quads = cell_w_metric
            .map(|cell_width| {
                let default_fg = run_color;
                let mut quads = Vec::new();

                let view = self.view.read(cx);
                for (row, line) in view.viewport_lines.iter().enumerate() {
                    let y = bounds.top() + line_height * row as f32;
                    let runs = view.viewport_style_runs.get(row).map(|v| v.as_slice());
                    build_box_quads_for_row(
                        line,
                        runs,
                        default_fg,
                        bounds.left(),
                        y,
                        cell_width,
                        line_height,
                        &mut quads,
                    );
                }

                quads
            })
            .unwrap_or_default();

        let peek_box_drawing_quads = cell_w_metric
            .map(|cell_width| {
                let mut quads = Vec::new();
                let view = self.view.read(cx);
                if view.pixel_offset > 0.0
                    && let Some(peek_text) = view.peek_text.as_deref()
                {
                    let y = bounds.top() - line_height;
                    build_box_quads_for_row(
                        peek_text,
                        Some(view.peek_runs.as_slice()),
                        run_color,
                        bounds.left(),
                        y,
                        cell_width,
                        line_height,
                        &mut quads,
                    );
                }
                quads
            })
            .unwrap_or_default();

        let cursor = {
            let view = self.view.read(cx);
            if view.focus_handle.is_focused(window)
                && view.session.cursor_visible()
                && view.cursor_blink_on
            {
                view.session.cursor_position()
            } else {
                None
            }
        }
        .and_then(|(col, row)| {
            let (cursor_color, cursor_style) = {
                let view = self.view.read(cx);
                let bg = view.session.default_background();
                let color = view
                    .session
                    .cursor_color()
                    .map(hsla_from_rgb)
                    .unwrap_or_else(|| cursor_color_for_background(bg));
                (color, view.session.cursor_style())
            };
            let y = bounds.top() + line_height * (row.saturating_sub(1)) as f32;
            let row_index = row.saturating_sub(1) as usize;
            let line = shaped_lines.get(row_index)?;
            let byte_index = byte_index_for_column_in_line(line.text.as_str(), col);
            let x = bounds.left() + line.x_for_index(byte_index.min(line.text.len()));

            let width = match cursor_style {
                crate::CursorStyle::Block => cell_width.unwrap_or(px(8.0)),
                crate::CursorStyle::Bar => px(2.0),
            };
            Some(fill(
                Bounds::new(point(x, y), size(width, line_height)),
                cursor_color,
            ))
        });

        let (pixel_offset, peek_line) = {
            let view = self.view.read(cx);
            (px(view.pixel_offset), view.peek_layout.clone())
        };

        // Refresh the cached scroll position if it's stale, then build
        // scrollbar quads. Skip when total rows fit entirely in the viewport
        // (nothing to scroll).
        let scrollbar = self.view.update(cx, |view, _cx| {
            if view.scroll_pos_dirty {
                view.scroll_pos = view.session.scroll_position();
                view.scroll_pos_dirty = false;
            }
            view.scroll_pos.and_then(|pos| {
                if pos.total_rows <= pos.viewport_rows || pos.viewport_rows == 0 {
                    return None;
                }
                Some(build_scrollbar_quads(bounds, pos))
            })
        });

        let prepaint_us = _prepaint_start.elapsed().as_micros() as u64;
        SCROLL_STATS.prepaint_calls.fetch_add(1, Ordering::Relaxed);
        SCROLL_STATS.prepaint_us.fetch_add(prepaint_us, Ordering::Relaxed);

        TerminalPrepaintState {
            line_height,
            pixel_offset,
            shaped_lines,
            background_quads,
            selection_quads,
            box_drawing_quads,
            marked_text,
            marked_text_background,
            cursor,
            peek_line,
            peek_background_quads,
            peek_box_drawing_quads,
            scrollbar,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let paint_start = Instant::now();
        self.view.update(cx, |view, _cx| {
            view.last_bounds = Some(bounds);
        });

        let focus_handle = { self.view.read(cx).focus_handle.clone() };
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );

        window.paint_layer(bounds, |window| {
            let default_bg = { self.view.read(cx).session.default_background() };
            window.paint_quad(fill(bounds, hsla_from_rgb(default_bg)));

            // Sub-line scroll: every viewport-content y is shifted by
            // `pixel_offset`. The peek row sits at content row "−1", i.e.
            // its quads/text were prepared at `bounds.top() - line_height`
            // and get the same shift, so it lands at
            // `bounds.top() - line_height + pixel_offset` — partially
            // revealed from the top edge. `paint_layer(bounds)` clips
            // anything that overflows top/bottom.
            let dy = prepaint.pixel_offset;
            let translate = |q: &mut PaintQuad| {
                q.bounds.origin.y += dy;
            };

            for quad in prepaint.peek_background_quads.drain(..) {
                let mut q = quad;
                translate(&mut q);
                window.paint_quad(q);
            }
            for quad in prepaint.background_quads.drain(..) {
                let mut q = quad;
                translate(&mut q);
                window.paint_quad(q);
            }

            for quad in prepaint.selection_quads.drain(..) {
                let mut q = quad;
                translate(&mut q);
                window.paint_quad(q);
            }

            let origin = bounds.origin;
            if let Some(peek_line) = prepaint.peek_line.as_ref() {
                let y = origin.y - prepaint.line_height + dy;
                let _ = peek_line.paint(
                    point(origin.x, y),
                    prepaint.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
            for (row, line) in prepaint.shaped_lines.iter().enumerate() {
                let y = origin.y + prepaint.line_height * row as f32 + dy;
                let _ = line.paint(
                    point(origin.x, y),
                    prepaint.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }

            for quad in prepaint.peek_box_drawing_quads.drain(..) {
                let mut q = quad;
                translate(&mut q);
                window.paint_quad(q);
            }
            for quad in prepaint.box_drawing_quads.drain(..) {
                let mut q = quad;
                translate(&mut q);
                window.paint_quad(q);
            }

            if let Some(mut bg) = prepaint.marked_text_background.take() {
                translate(&mut bg);
                window.paint_quad(bg);
            }

            if let Some((line, origin)) = prepaint.marked_text.as_ref() {
                let _ = line.paint(
                    point(origin.x, origin.y + dy),
                    prepaint.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }

            if let Some(mut cursor) = prepaint.cursor.take() {
                translate(&mut cursor);
                window.paint_quad(cursor);
            }

            // Scrollbar paints on top of everything else, *not* shifted by
            // pixel_offset — the bar lives in viewport-relative coords so a
            // sub-line scroll can advance the thumb without the track sliding
            // along. Track first, thumb on top.
            if let Some(scrollbar) = prepaint.scrollbar.take() {
                window.paint_quad(scrollbar.track);
                window.paint_quad(scrollbar.thumb);
            }
        });

        let paint_us = paint_start.elapsed().as_micros() as u64;
        SCROLL_STATS.paint_calls.fetch_add(1, Ordering::Relaxed);
        SCROLL_STATS.paint_us.fetch_add(paint_us, Ordering::Relaxed);
        SCROLL_STATS.maybe_emit();
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ensure_key_bindings(cx);

        let had_pending_output = !self.pending_output.is_empty();
        if had_pending_output {
            let bytes = std::mem::take(&mut self.pending_output);
            let sync_before = self.session.synchronized_output_active();
            let touched_sync_mode = Self::has_synchronized_output_mode_change(&bytes);
            self.feed_output_bytes_to_session(&bytes);
            self.apply_side_effects(cx);
            let sync_after = self.session.synchronized_output_active();

            if sync_after {
                self.pending_refresh = true;
            } else if sync_before || touched_sync_mode {
                self.pending_refresh = true;
            } else {
                self.reconcile_dirty_viewport_after_output();
            }
        }

        if self.pending_refresh {
            // Only defer refresh while synchronized output is active AND
            // the refresh was triggered by PTY output.  User-driven
            // refreshes (resize, scrollback) must never be blocked.
            if !self.session.synchronized_output_active() || !had_pending_output {
                self.refresh_viewport();
                self.pending_refresh = false;
            }
        }

        if self.session.window_title_updates_enabled() {
            let title = self
                .session
                .title()
                .unwrap_or("GPUI Embedded Terminal (Ghostty VT)");

            if self.last_window_title.as_deref() != Some(title) {
                window.set_window_title(title);
                self.last_window_title = Some(title.to_string());
            }
        }

        div()
            .size_full()
            .flex()
            .track_focus(&self.focus_handle)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::on_copy))
            .on_action(cx.listener(Self::on_select_all))
            .on_action(cx.listener(Self::on_paste))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_tab_prev))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .bg(gpui::black())
            .text_color(gpui::white())
            .font(self.font.clone())
            .whitespace_nowrap()
            .child(TerminalTextElement { view: cx.entity() })
    }
}

pub(crate) fn cell_metrics(window: &mut gpui::Window, font: &gpui::Font) -> Option<(f32, f32)> {
    cell_metrics_with_overrides(window, font, None, None)
}

/// Compute cell (width, height) in px. Overrides let callers force a
/// specific font size / line-height ratio instead of inheriting from
/// `window.text_style()`.
pub(crate) fn cell_metrics_with_overrides(
    window: &mut gpui::Window,
    font: &gpui::Font,
    font_size_override: Option<f32>,
    line_height_ratio_override: Option<f32>,
) -> Option<(f32, f32)> {
    let mut style = window.text_style();
    style.font_family = font.family.clone();
    style.font_features = crate::default_terminal_font_features();
    style.font_fallbacks = font.fallbacks.clone();
    if let Some(fs) = font_size_override {
        style.font_size = px(fs).into();
    }
    if let Some(ratio) = line_height_ratio_override {
        style.line_height = gpui::relative(ratio);
    }

    let rem_size = window.rem_size();
    let font_size = style.font_size.to_pixels(rem_size);
    let line_height = style.line_height.to_pixels(style.font_size, rem_size);

    let run = style.to_run(1);
    let lines = window
        .text_system()
        .shape_text(
            gpui::SharedString::from("M"),
            font_size,
            &[run],
            None,
            Some(1),
        )
        .ok()?;
    let line = lines.first()?;

    let cell_width = f32::from(line.width()).max(1.0);
    let cell_height = f32::from(line_height).max(1.0);
    Some((cell_width, cell_height))
}

#[cfg(test)]
mod tests {
    use ghostty_vt::Rgb;

    use super::{url_at_byte_index, url_at_column_in_line, window_position_to_local};

    #[test]
    fn url_detection_finds_https_links() {
        let text = "Visit https://google.com for search";
        let idx = text.find("google").unwrap();
        assert_eq!(
            url_at_byte_index(text, idx).as_deref(),
            Some("https://google.com")
        );
    }

    #[test]
    fn url_detection_finds_https_links_by_cell_column() {
        let line = "https://google.com";
        assert_eq!(
            url_at_column_in_line(line, 1).as_deref(),
            Some("https://google.com")
        );
        assert_eq!(
            url_at_column_in_line(line, 10).as_deref(),
            Some("https://google.com")
        );
    }

    #[test]
    fn mouse_position_to_local_accounts_for_bounds_origin() {
        let bounds = Some(gpui::Bounds::new(
            gpui::point(gpui::px(100.0), gpui::px(20.0)),
            gpui::size(gpui::px(200.0), gpui::px(80.0)),
        ));

        let local = window_position_to_local(bounds, gpui::point(gpui::px(110.0), gpui::px(30.0)));
        assert_eq!(local, gpui::point(gpui::px(10.0), gpui::px(10.0)));
    }

    #[test]
    fn cursor_color_contrasts_with_background() {
        let cursor = super::cursor_color_for_background(Rgb {
            r: 0xFF,
            g: 0xFF,
            b: 0xFF,
        });
        assert!(cursor.l < 0.2);
        assert!((cursor.a - 0.72).abs() < f32::EPSILON);

        let cursor = super::cursor_color_for_background(Rgb {
            r: 0x00,
            g: 0x00,
            b: 0x00,
        });
        assert!(cursor.l > 0.8);
        assert!((cursor.a - 0.72).abs() < f32::EPSILON);
    }
}
