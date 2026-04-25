mod config;
mod font;
mod session;

pub mod view;

pub use config::{CursorStyle, TerminalConfig};
pub use font::{default_terminal_font, default_terminal_font_features};
pub use ghostty_vt::TerminalEvent;
pub use session::TerminalSession;

#[cfg(test)]
mod tests;
