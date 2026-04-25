use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

#[derive(Debug)]
pub enum Error {
    CreateFailed,
    FeedFailed(i32),
    ScrollFailed(i32),
    DumpFailed,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::CreateFailed => write!(f, "terminal create failed"),
            Error::FeedFailed(code) => write!(f, "terminal feed failed: {code}"),
            Error::ScrollFailed(code) => write!(f, "terminal scroll failed: {code}"),
            Error::DumpFailed => write!(f, "terminal dump failed"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Terminal {
    ptr: NonNull<c_void>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StyleRun {
    pub start_col: u16,
    pub end_col: u16,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: u8,
}

/// Snapshot of the viewport's position inside the full screen.
/// See [`Terminal::scroll_position`] for details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollPosition {
    /// 0-based row index of the topmost visible row, counting from the
    /// oldest scrollback row. Equals `total_rows - viewport_rows` when the
    /// viewport is scrolled all the way to the live screen bottom.
    pub viewport_top: u32,
    /// Total row count in the screen (scrollback + on-screen).
    pub total_rows: u32,
    /// Height of the viewport in rows.
    pub viewport_rows: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct KeyModifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
}

impl KeyModifiers {
    fn bits(self) -> u16 {
        let mut bits = 0u16;
        if self.shift {
            bits |= 0x0001;
        }
        if self.control {
            bits |= 0x0002;
        }
        if self.alt {
            bits |= 0x0004;
        }
        if self.super_key {
            bits |= 0x0008;
        }
        bits
    }
}

pub fn encode_key_named(name: &str, modifiers: KeyModifiers) -> Option<Vec<u8>> {
    if name.is_empty() {
        return None;
    }

    let bytes = unsafe {
        ghostty_vt_sys::ghostty_vt_encode_key_named(name.as_ptr(), name.len(), modifiers.bits())
    };
    if bytes.ptr.is_null() || bytes.len == 0 {
        return None;
    }

    let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
    let out = slice.to_vec();
    unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
    Some(out)
}

impl Terminal {
    pub fn new(cols: u16, rows: u16) -> Result<Self, Error> {
        let ptr = unsafe { ghostty_vt_sys::ghostty_vt_terminal_new(cols, rows) };
        let ptr = NonNull::new(ptr).ok_or(Error::CreateFailed)?;
        Ok(Self { ptr })
    }

    pub fn set_default_colors(&mut self, fg: Rgb, bg: Rgb) {
        unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_set_default_colors(
                self.ptr.as_ptr(),
                fg.r,
                fg.g,
                fg.b,
                bg.r,
                bg.g,
                bg.b,
            )
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let rc = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_feed(self.ptr.as_ptr(), bytes.as_ptr(), bytes.len())
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::FeedFailed(rc))
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), Error> {
        let rc =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_resize(self.ptr.as_ptr(), cols, rows) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }

    pub fn dump_viewport(&self) -> Result<String, Error> {
        let bytes = unsafe { ghostty_vt_sys::ghostty_vt_terminal_dump_viewport(self.ptr.as_ptr()) };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(s)
    }

    pub fn dump_viewport_row(&self, row: u16) -> Result<String, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_viewport_row(self.ptr.as_ptr(), row)
        };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(s)
    }

    pub fn dump_viewport_row_cell_styles(&self, row: u16) -> Result<Vec<CellStyle>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_viewport_row_cell_styles(
                self.ptr.as_ptr(),
                row,
            )
        };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }
        if bytes.len == 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Ok(Vec::new());
        }
        if bytes.len % 8 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 8);
        for chunk in slice.chunks_exact(8) {
            out.push(CellStyle {
                fg: Rgb {
                    r: chunk[0],
                    g: chunk[1],
                    b: chunk[2],
                },
                bg: Rgb {
                    r: chunk[3],
                    g: chunk[4],
                    b: chunk[5],
                },
                flags: chunk[6],
            });
        }

        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(out)
    }

    pub fn dump_viewport_row_style_runs(&self, row: u16) -> Result<Vec<StyleRun>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_viewport_row_style_runs(self.ptr.as_ptr(), row)
        };
        if bytes.ptr.is_null() {
            return Err(Error::DumpFailed);
        }
        if bytes.len == 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Ok(Vec::new());
        }
        if bytes.len % 12 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 12);
        for chunk in slice.chunks_exact(12) {
            out.push(StyleRun {
                start_col: u16::from_ne_bytes([chunk[0], chunk[1]]),
                end_col: u16::from_ne_bytes([chunk[2], chunk[3]]),
                fg: Rgb {
                    r: chunk[4],
                    g: chunk[5],
                    b: chunk[6],
                },
                bg: Rgb {
                    r: chunk[7],
                    g: chunk[8],
                    b: chunk[9],
                },
                flags: chunk[10],
            });
        }

        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(out)
    }

    /// Read a single row from scrollback **above** the visible viewport.
    /// Used by pixel-smooth scroll to fetch the bleed row that visually
    /// peeks in at the top while the viewport slides between line
    /// boundaries.
    ///
    /// `rows_above_viewport_top = 0` returns the row immediately above the
    /// viewport's top row. `1` returns the row above that, etc.
    ///
    /// Returns `Ok(None)` if there is no row that far back (start of
    /// scrollback hit). Returns `Ok(Some(""))` if the row exists but is
    /// blank.
    pub fn dump_screen_row(&self, rows_above_viewport_top: u32) -> Result<Option<String>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_screen_row(
                self.ptr.as_ptr(),
                rows_above_viewport_top,
            )
        };
        if bytes.ptr.is_null() {
            return Ok(None);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(Some(s))
    }

    /// Same as [`dump_screen_row`] but returns the per-cell style runs.
    /// Returns `Ok(None)` if no row at that offset.
    pub fn dump_screen_row_style_runs(
        &self,
        rows_above_viewport_top: u32,
    ) -> Result<Option<Vec<StyleRun>>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_dump_screen_row_style_runs(
                self.ptr.as_ptr(),
                rows_above_viewport_top,
            )
        };
        if bytes.ptr.is_null() {
            return Ok(None);
        }
        if bytes.len == 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Ok(Some(Vec::new()));
        }
        if bytes.len % 12 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 12);
        for chunk in slice.chunks_exact(12) {
            out.push(StyleRun {
                start_col: u16::from_ne_bytes([chunk[0], chunk[1]]),
                end_col: u16::from_ne_bytes([chunk[2], chunk[3]]),
                fg: Rgb {
                    r: chunk[4],
                    g: chunk[5],
                    b: chunk[6],
                },
                bg: Rgb {
                    r: chunk[7],
                    g: chunk[8],
                    b: chunk[9],
                },
                flags: chunk[10],
            });
        }

        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(Some(out))
    }

    pub fn take_dirty_viewport_rows(&mut self, rows: u16) -> Result<Vec<u16>, Error> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_take_dirty_viewport_rows(self.ptr.as_ptr(), rows)
        };
        if bytes.ptr.is_null() || bytes.len == 0 {
            return Ok(Vec::new());
        }
        if bytes.len % 2 != 0 {
            unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
            return Err(Error::DumpFailed);
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let mut out = Vec::with_capacity(bytes.len / 2);
        for chunk in slice.chunks_exact(2) {
            out.push(u16::from_le_bytes([chunk[0], chunk[1]]));
        }
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Ok(out)
    }

    /// Snapshot of where the viewport sits inside the full screen
    /// (scrollback + active rows). Used to drive a scrollbar UI.
    ///
    /// * `viewport_top` — 0-based row index of the topmost visible row,
    ///   counting from the oldest scrollback row.
    /// * `total_rows` — current total row count in the screen (scrollback +
    ///   on-screen). Grows as new lines scroll into history; capped at the
    ///   terminal's scrollback size.
    /// * `viewport_rows` — height of the viewport in rows, configured at
    ///   construction time.
    ///
    /// Returns `None` only if the FFI handle is null. O(pages) walk —
    /// cheap, but not free; cache between scroll events.
    pub fn scroll_position(&self) -> Option<ScrollPosition> {
        let mut viewport_top: u32 = 0;
        let mut total_rows: u32 = 0;
        let mut viewport_rows: u32 = 0;
        let ok = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_scroll_position(
                self.ptr.as_ptr(),
                &mut viewport_top,
                &mut total_rows,
                &mut viewport_rows,
            )
        };
        if !ok {
            return None;
        }
        Some(ScrollPosition {
            viewport_top,
            total_rows,
            viewport_rows,
        })
    }

    /// Hard reset of the terminal state — equivalent to `tput reset` or
    /// Terminal.app's "Reset" menu item. Clears scrollback, resets modes
    /// and tab stops, forces a full redraw. The shell process is left
    /// running; the next prompt will print as normal.
    pub fn full_reset(&mut self) {
        unsafe { ghostty_vt_sys::ghostty_vt_terminal_full_reset(self.ptr.as_ptr()) }
    }

    pub fn take_viewport_scroll_delta(&mut self) -> i32 {
        unsafe { ghostty_vt_sys::ghostty_vt_terminal_take_viewport_scroll_delta(self.ptr.as_ptr()) }
    }

    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        let mut col: u16 = 0;
        let mut row: u16 = 0;
        let ok = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_cursor_position(
                self.ptr.as_ptr(),
                &mut col as *mut u16,
                &mut row as *mut u16,
            )
        };
        ok.then_some((col, row))
    }

    pub fn hyperlink_at(&self, col: u16, row: u16) -> Option<String> {
        let bytes = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_hyperlink_at(self.ptr.as_ptr(), col, row)
        };
        if bytes.ptr.is_null() || bytes.len == 0 {
            return None;
        }

        let slice = unsafe { std::slice::from_raw_parts(bytes.ptr, bytes.len) };
        let s = String::from_utf8_lossy(slice).into_owned();
        unsafe { ghostty_vt_sys::ghostty_vt_bytes_free(bytes) };
        Some(s)
    }

    pub fn scroll_viewport(&mut self, delta_lines: i32) -> Result<(), Error> {
        let rc = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_scroll_viewport(self.ptr.as_ptr(), delta_lines)
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }

    pub fn scroll_viewport_top(&mut self) -> Result<(), Error> {
        let rc =
            unsafe { ghostty_vt_sys::ghostty_vt_terminal_scroll_viewport_top(self.ptr.as_ptr()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }

    pub fn scroll_viewport_bottom(&mut self) -> Result<(), Error> {
        let rc = unsafe {
            ghostty_vt_sys::ghostty_vt_terminal_scroll_viewport_bottom(self.ptr.as_ptr())
        };
        if rc == 0 {
            Ok(())
        } else {
            Err(Error::ScrollFailed(rc))
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        unsafe { ghostty_vt_sys::ghostty_vt_terminal_free(self.ptr.as_ptr()) }
    }
}

pub fn terminal_new(cols: u16, rows: u16) -> Result<Terminal, Error> {
    Terminal::new(cols, rows)
}
