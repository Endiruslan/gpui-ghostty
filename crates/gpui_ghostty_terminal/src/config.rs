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
        }
    }
}
