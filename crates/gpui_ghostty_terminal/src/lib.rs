mod config;
mod font;
mod paste;
mod session;

pub mod view;

pub use config::{ColorScheme, CursorStyle, TerminalConfig};
pub use font::{
    default_terminal_font, default_terminal_font_features, device_pixel_line_height,
    snap_to_device_pixel,
};
pub use ghostty_vt::TerminalEvent;
pub use session::TerminalSession;

#[cfg(test)]
mod tests;
