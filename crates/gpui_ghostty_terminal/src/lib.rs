use ghostty_vt::{Error, Terminal};

#[derive(Clone, Copy, Debug)]
pub struct TerminalConfig {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

pub struct TerminalSession {
    terminal: Terminal,
}

impl TerminalSession {
    pub fn new(config: TerminalConfig) -> Result<Self, Error> {
        Ok(Self {
            terminal: Terminal::new(config.cols, config.rows)?,
        })
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.terminal.feed(bytes)
    }

    pub fn dump_viewport(&self) -> Result<String, Error> {
        self.terminal.dump_viewport()
    }

    pub fn scroll_viewport(&mut self, delta_lines: i32) -> Result<(), Error> {
        self.terminal.scroll_viewport(delta_lines)
    }
}

pub mod view {
    use super::TerminalSession;
    use gpui::{
        div, prelude::*, Context, FocusHandle, IntoElement, KeyDownEvent, Render, ScrollDelta,
        ScrollWheelEvent, Window,
    };

    pub struct TerminalView {
        session: TerminalSession,
        viewport: String,
        focus_handle: FocusHandle,
    }

    impl TerminalView {
        pub fn new(session: TerminalSession, focus_handle: FocusHandle) -> Self {
            let viewport = session.dump_viewport().unwrap_or_default();
            Self {
                session,
                viewport,
                focus_handle,
            }
        }

        fn on_key_down(
            &mut self,
            event: &KeyDownEvent,
            _window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let keystroke = event.keystroke.clone().with_simulated_ime();

            if keystroke.modifiers.platform
                || keystroke.modifiers.control
                || keystroke.modifiers.alt
                || keystroke.modifiers.function
            {
                return;
            }

            if let Some(text) = keystroke.key_char.as_deref() {
                let _ = self.session.feed(text.as_bytes());
                cx.notify();
                return;
            }

            if keystroke.key == "backspace" {
                let _ = self.session.feed(&[0x08]);
                cx.notify();
            }
        }

        fn on_scroll_wheel(
            &mut self,
            event: &ScrollWheelEvent,
            _window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let dy_lines: f32 = match event.delta {
                ScrollDelta::Lines(p) => p.y,
                ScrollDelta::Pixels(p) => f32::from(p.y) / 16.0,
            };

            let delta_lines = (-dy_lines).round() as i32;
            if delta_lines == 0 {
                return;
            }

            let _ = self.session.scroll_viewport(delta_lines);
            cx.notify();
        }
    }

    impl Render for TerminalView {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.viewport = self.session.dump_viewport().unwrap_or_default();

            div()
                .size_full()
                .flex()
                .track_focus(&self.focus_handle)
                .on_key_down(cx.listener(Self::on_key_down))
                .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
                .font_family("monospace")
                .whitespace_nowrap()
                .child(self.viewport.clone())
        }
    }
}
