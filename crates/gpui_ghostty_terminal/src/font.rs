pub fn default_terminal_font() -> gpui::Font {
    let family = if cfg!(target_os = "macos") {
        "Menlo"
    } else if cfg!(target_os = "windows") {
        "Consolas"
    } else {
        "DejaVu Sans Mono"
    };

    let fallbacks = gpui::FontFallbacks::from_fonts(vec![
        "SF Mono".to_string(),
        "Menlo".to_string(),
        "Monaco".to_string(),
        "Consolas".to_string(),
        "Cascadia Mono".to_string(),
        "DejaVu Sans Mono".to_string(),
        "Noto Sans Mono".to_string(),
        "JetBrains Mono".to_string(),
        "Fira Mono".to_string(),
        "Sarasa Mono SC".to_string(),
        "Sarasa Term SC".to_string(),
        "Sarasa Mono J".to_string(),
        "Noto Sans Mono CJK SC".to_string(),
        "Noto Sans Mono CJK JP".to_string(),
        "Source Han Mono SC".to_string(),
        "WenQuanYi Zen Hei Mono".to_string(),
        "Apple Color Emoji".to_string(),
        "Noto Color Emoji".to_string(),
        "Segoe UI Emoji".to_string(),
    ]);

    let mut font = gpui::font(family);
    font.fallbacks = Some(fallbacks);
    font
}

pub fn default_terminal_font_features() -> gpui::FontFeatures {
    use std::sync::Arc;
    gpui::FontFeatures(Arc::new(vec![
        ("calt".to_string(), 0),
        ("liga".to_string(), 0),
        ("kern".to_string(), 0),
    ]))
}

/// A terminal row's height rounded to whole device pixels, as Ghostty keeps
/// its cell height. Rows are tiled edge to edge by separate quads, and gpui
/// rounds each quad's top and bottom to device pixels on its own: with a
/// fractional step (13 px × 1.35 = 17.55 px = 35.1 device px at 2×) every
/// tenth boundary lands on x.5, f32 error rounds the two sides of it apart,
/// and a 1-device-pixel line of whatever is behind shows through. A whole
/// device-pixel step keeps every row edge exact in f32.
pub fn device_pixel_line_height(line_height: gpui::Pixels, scale_factor: f32) -> gpui::Pixels {
    snap_to_device_pixel(line_height, scale_factor).max(gpui::px(1.0 / scale_factor))
}

/// `value` rounded to the nearest whole device pixel.
pub fn snap_to_device_pixel(value: gpui::Pixels, scale_factor: f32) -> gpui::Pixels {
    gpui::px((f32::from(value) * scale_factor).round() / scale_factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_pixel_line_height_tiles_rows_without_seams() {
        // The reported case: 13 px font, 1.35 ratio, Retina, a pane at y = 60.
        let raw = gpui::px(13.0 * 1.35);
        let lh = f32::from(device_pixel_line_height(raw, 2.0));
        assert_eq!(lh, 17.5);
        let origin = 60.0_f32;
        for row in 0..400 {
            let top = origin + lh * row as f32;
            let next_top = origin + lh * (row + 1) as f32;
            assert_eq!(top + lh, next_top, "row {row}: bottom and next top differ");
        }
    }

    #[test]
    fn device_pixel_line_height_never_collapses() {
        assert_eq!(f32::from(device_pixel_line_height(gpui::px(0.1), 2.0)), 0.5);
        assert_eq!(
            f32::from(device_pixel_line_height(gpui::px(17.55), 1.0)),
            18.0
        );
    }
}
