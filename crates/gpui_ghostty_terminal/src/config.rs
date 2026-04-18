use ghostty_vt::Rgb;

#[derive(Clone, Debug)]
pub struct TerminalConfig {
    pub cols: u16,
    pub rows: u16,
    pub default_fg: Rgb,
    pub default_bg: Rgb,
    pub update_window_title: bool,
    /// Font size in pixels. `None` = inherit from `window.text_style()`.
    pub font_size: Option<f32>,
    /// Line height as a multiplier of font size. `None` = inherit from
    /// `window.text_style()` (GPUI default is phi ≈ 1.618, which is usually
    /// too tall for terminals — typical terminal ratio is ~1.2).
    pub line_height_ratio: Option<f32>,
    /// Primary font family. `None` = `default_terminal_font()` (Menlo on macOS).
    /// Fallbacks (SF Mono, Cascadia Mono, JetBrains Mono, Noto mono CJK, emoji…)
    /// are always appended regardless of this choice.
    pub font_family: Option<String>,
    /// Cursor blink interval in milliseconds. `None` = no blink (solid cursor).
    /// Typical terminal blink rate is 530 ms. The blink is implemented with a
    /// single background timer that toggles a boolean and schedules a repaint —
    /// ~2 Hz notify rate, negligible CPU cost.
    pub cursor_blink_ms: Option<u64>,
    /// Optional override for the cursor color.  `None` = auto-contrast
    /// against the current background.  Use to theme the cursor with the
    /// host application's accent color.
    pub cursor_color: Option<Rgb>,
    /// Cursor rendering shape.  Defaults to [`CursorStyle::Block`].
    pub cursor_style: CursorStyle,
}

/// Cursor shape for [`TerminalConfig::cursor_style`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorStyle {
    /// Full cell-width fill — classic block cursor.
    #[default]
    Block,
    /// Thin 2 px vertical bar at the left edge of the cursor cell.
    Bar,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            cols: 80,
            rows: 24,
            default_fg: Rgb {
                r: 0xFF,
                g: 0xFF,
                b: 0xFF,
            },
            default_bg: Rgb {
                r: 0x00,
                g: 0x00,
                b: 0x00,
            },
            update_window_title: true,
            font_size: None,
            line_height_ratio: None,
            font_family: None,
            cursor_blink_ms: None,
            cursor_color: None,
            cursor_style: CursorStyle::Block,
        }
    }
}
