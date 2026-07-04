const std = @import("std");
const ghostty_input = @import("ghostty_src/input.zig");
const terminal = @import("ghostty_src/terminal/main.zig");

const Allocator = std.mem.Allocator;

const TerminalHandle = struct {
    alloc: Allocator,
    terminal: terminal.Terminal,
    stream: terminal.Stream(*Handler),
    handler: Handler,
    default_fg: terminal.color.RGB,
    default_bg: terminal.color.RGB,
    viewport_top_y_screen: u32,
    has_viewport_top_y_screen: bool,
    /// TLV event queue produced by the OSC handler. Drained by Rust via
    /// `ghostty_vt_terminal_drain_events`. Each record is:
    ///   tag (u8)  payload...
    /// Tags:
    ///   0x01 Notification: title_len(u16 LE) title... body_len(u16 LE) body...
    ///   0x02 CommandStart: no payload
    ///   0x03 CommandEnd:   exit_code(u8)
    ///   0x04 Bell:         no payload
    events: std.ArrayList(u8),

    fn init(alloc: Allocator, cols: u16, rows: u16) !*TerminalHandle {
        const handle = try alloc.create(TerminalHandle);
        errdefer alloc.destroy(handle);

        const t = try terminal.Terminal.init(alloc, .{
            .cols = cols,
            .rows = rows,
        });
        errdefer {
            var tmp = t;
            tmp.deinit(alloc);
        }

        handle.* = .{
            .alloc = alloc,
            .terminal = t,
            .handler = .{ .handle = undefined, .inner = undefined },
            .stream = undefined,
            .default_fg = .{ .r = 0xFF, .g = 0xFF, .b = 0xFF },
            .default_bg = .{ .r = 0x00, .g = 0x00, .b = 0x00 },
            .viewport_top_y_screen = 0,
            .has_viewport_top_y_screen = true,
            .events = .{},
        };
        handle.handler.handle = handle;
        handle.handler.inner = terminal.ReadonlyHandler.init(&handle.terminal);
        handle.stream = terminal.Stream(*Handler).init(&handle.handler);
        handle.stream.parser.osc_parser.alloc = alloc;
        return handle;
    }

    fn deinit(self: *TerminalHandle) void {
        self.stream.deinit();
        self.terminal.deinit(self.alloc);
        self.events.deinit(self.alloc);
        self.alloc.destroy(self);
    }
};

/// Stream handler: thin wrapper over ghostty's own
/// `stream_readonly.Handler` (applies every terminal-state action to the
/// Terminal — including SU/SD, semantic prompts, palette color ops and
/// alt-screen mode switches), plus our host-event hooks that queue OSC /
/// control events for the Rust side to drain.
const Handler = struct {
    handle: *TerminalHandle,
    inner: terminal.ReadonlyHandler,

    const Action = terminal.StreamAction;

    pub fn deinit(self: *Handler) void {
        self.inner.deinit();
    }

    pub fn vt(
        self: *Handler,
        comptime action: Action.Tag,
        value: Action.Value(action),
    ) !void {
        switch (action) {
            // ---- host events (queued for Rust drain) ----
            .bell => try self.handle.events.append(self.handle.alloc, 0x04),

            .show_desktop_notification => {
                try self.showDesktopNotification(value.title, value.body);
            },

            .semantic_prompt => {
                switch (value.action) {
                    // OSC 133;C — user hit enter, command is about to
                    // execute. The canonical "command start" signal in
                    // semantic-prompt shell integrations (zsh preexec,
                    // fish fish_preexec, …).
                    .end_input_start_output => try self.handle.events.append(self.handle.alloc, 0x02),
                    // OSC 133;D — command finished (with optional exit code).
                    .end_command => {
                        const code: u8 = code: {
                            const raw: i32 = value.readOption(.exit_code) orelse 0;
                            break :code std.math.cast(u8, raw) orelse 1;
                        };
                        try self.handle.events.append(self.handle.alloc, 0x03);
                        try self.handle.events.append(self.handle.alloc, code);
                    },
                    else => {},
                }
                // Terminal keeps its own semantic-prompt row state.
                try self.inner.vt(action, value);
            },

            // DECSET/DECRST 5 (reverse colors) repaints every cell; the
            // readonly handler flips the mode but doesn't mark anything
            // dirty (the ghostty app repaints via its own renderer state).
            // Our Rust side keys the full-redraw path off this flag.
            .set_mode, .reset_mode => {
                const enabled = action == .set_mode;
                const prev = self.handle.terminal.modes.get(value.mode);
                try self.inner.vt(action, value);
                if (prev != enabled and value.mode == .reverse_colors) {
                    self.handle.terminal.flags.dirty.reverse_colors = true;
                }
            },

            // ---- everything else: ghostty's Terminal-applying handler ----
            else => try self.inner.vt(action, value),
        }
    }

    fn showDesktopNotification(self: *Handler, title: []const u8, body: []const u8) !void {
        // Cap to avoid pathological payloads.
        const max_len: usize = 4096;
        const title_clipped = if (title.len > max_len) title[0..max_len] else title;
        const body_clipped = if (body.len > max_len) body[0..max_len] else body;

        try self.handle.events.append(self.handle.alloc, 0x01);
        try appendU16Le(&self.handle.events, self.handle.alloc, @intCast(title_clipped.len));
        try self.handle.events.appendSlice(self.handle.alloc, title_clipped);
        try appendU16Le(&self.handle.events, self.handle.alloc, @intCast(body_clipped.len));
        try self.handle.events.appendSlice(self.handle.alloc, body_clipped);
    }
};

fn appendU16Le(list: *std.ArrayList(u8), alloc: Allocator, v: u16) !void {
    try list.append(alloc, @intCast(v & 0xFF));
    try list.append(alloc, @intCast((v >> 8) & 0xFF));
}

/// Drain the queued OSC/control events. The returned bytes are owned by the
/// caller and must be freed with `ghostty_vt_bytes_free`. The internal queue
/// is cleared.
export fn ghostty_vt_terminal_drain_events(terminal_ptr: ?*anyopaque) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));
    if (handle.events.items.len == 0) return .{ .ptr = null, .len = 0 };

    const slice = handle.events.toOwnedSlice(handle.alloc) catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = slice.ptr, .len = slice.len };
}

export fn ghostty_vt_terminal_new(cols: u16, rows: u16) callconv(.c) ?*anyopaque {
    const alloc = std.heap.c_allocator;
    const handle = TerminalHandle.init(alloc, cols, rows) catch return null;
    return @ptrCast(handle);
}

export fn ghostty_vt_terminal_free(terminal_ptr: ?*anyopaque) callconv(.c) void {
    if (terminal_ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));
    handle.deinit();
}

export fn ghostty_vt_terminal_set_default_colors(
    terminal_ptr: ?*anyopaque,
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
) callconv(.c) void {
    if (terminal_ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));
    handle.default_fg = .{ .r = fg_r, .g = fg_g, .b = fg_b };
    handle.default_bg = .{ .r = bg_r, .g = bg_g, .b = bg_b };
}

export fn ghostty_vt_terminal_feed(
    terminal_ptr: ?*anyopaque,
    bytes: [*]const u8,
    len: usize,
) callconv(.c) c_int {
    if (terminal_ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    var i: usize = 0;
    while (i < len) : (i += 1) {
        handle.stream.next(bytes[i]) catch return 2;
    }

    return 0;
}

export fn ghostty_vt_terminal_resize(
    terminal_ptr: ?*anyopaque,
    cols: u16,
    rows: u16,
) callconv(.c) c_int {
    if (terminal_ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    handle.terminal.resize(
        handle.alloc,
        @as(terminal.size.CellCountInt, @intCast(cols)),
        @as(terminal.size.CellCountInt, @intCast(rows)),
    ) catch return 2;
    return 0;
}

export fn ghostty_vt_terminal_scroll_viewport(
    terminal_ptr: ?*anyopaque,
    delta_lines: i32,
) callconv(.c) c_int {
    if (terminal_ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    handle.terminal.scrollViewport(.{ .delta = @as(isize, delta_lines) });
    return 0;
}

export fn ghostty_vt_terminal_scroll_viewport_top(terminal_ptr: ?*anyopaque) callconv(.c) c_int {
    if (terminal_ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    handle.terminal.scrollViewport(.top);
    return 0;
}

export fn ghostty_vt_terminal_scroll_viewport_bottom(terminal_ptr: ?*anyopaque) callconv(.c) c_int {
    if (terminal_ptr == null) return 1;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    handle.terminal.scrollViewport(.bottom);
    return 0;
}

export fn ghostty_vt_terminal_cursor_position(
    terminal_ptr: ?*anyopaque,
    col_out: ?*u16,
    row_out: ?*u16,
) callconv(.c) bool {
    if (terminal_ptr == null) return false;
    if (col_out == null or row_out == null) return false;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    col_out.?.* = @intCast(handle.terminal.screens.active.cursor.x + 1);
    row_out.?.* = @intCast(handle.terminal.screens.active.cursor.y + 1);
    return true;
}

export fn ghostty_vt_terminal_dump_viewport(terminal_ptr: ?*anyopaque) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const alloc = std.heap.c_allocator;
    const slice = handle.terminal.screens.active.dumpStringAlloc(alloc, .{ .viewport = .{} }) catch {
        return .{ .ptr = null, .len = 0 };
    };

    return .{ .ptr = slice.ptr, .len = slice.len };
}

export fn ghostty_vt_terminal_dump_viewport_row(
    terminal_ptr: ?*anyopaque,
    row: u16,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const pt: terminal.point.Point = .{ .viewport = .{ .x = 0, .y = row } };
    const pin = handle.terminal.screens.active.pages.pin(pt) orelse return .{ .ptr = null, .len = 0 };

    const alloc = std.heap.c_allocator;
    var builder: std.Io.Writer.Allocating = .init(alloc);
    errdefer builder.deinit();

    // dumpString emits the selection tl..br inclusive; with tl == br that
    // is a single CELL, so stretch br to the last column for a full row.
    var br = pin;
    br.x = handle.terminal.cols - 1;
    handle.terminal.screens.active.dumpString(&builder.writer, .{
        .tl = pin,
        .br = br,
        .unwrap = false,
    }) catch return .{ .ptr = null, .len = 0 };

    const slice = builder.toOwnedSlice() catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = slice.ptr, .len = slice.len };
}

const CellStyle = extern struct {
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
    flags: u8,
    reserved: u8,
};

export fn ghostty_vt_terminal_dump_viewport_row_cell_styles(
    terminal_ptr: ?*anyopaque,
    row: u16,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const pt: terminal.point.Point = .{ .viewport = .{ .x = 0, .y = row } };
    const pin = handle.terminal.screens.active.pages.pin(pt) orelse return .{ .ptr = null, .len = 0 };
    const cells = pin.cells(.all);

    const default_fg: terminal.color.RGB = handle.default_fg;
    const default_bg: terminal.color.RGB = handle.default_bg;
    const palette: *const terminal.color.Palette = &handle.terminal.colors.palette.current;

    const alloc = std.heap.c_allocator;
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(alloc);

    out.ensureTotalCapacity(alloc, cells.len * @sizeOf(CellStyle)) catch return .{ .ptr = null, .len = 0 };

    for (cells) |*cell| {
        const s = pin.style(cell);

        var fg = s.fg(.{ .default = default_fg, .palette = palette, .bold = null });
        var bg = s.bg(cell, palette) orelse default_bg;

        var flags: u8 = 0;
        if (s.flags.inverse) flags |= 0x01;
        if (s.flags.bold) flags |= 0x02;
        if (s.flags.italic) flags |= 0x04;
        if (s.flags.underline != .none) flags |= 0x08;
        if (s.flags.faint) flags |= 0x10;
        if (s.flags.invisible) flags |= 0x20;
        if (s.flags.strikethrough) flags |= 0x40;

        if (s.flags.inverse) {
            const tmp = fg;
            fg = bg;
            bg = tmp;
        }
        if (s.flags.invisible) {
            fg = bg;
        }

        const rec = CellStyle{
            .fg_r = fg.r,
            .fg_g = fg.g,
            .fg_b = fg.b,
            .bg_r = bg.r,
            .bg_g = bg.g,
            .bg_b = bg.b,
            .flags = flags,
            .reserved = 0,
        };
        out.appendSlice(alloc, std.mem.asBytes(&rec)) catch return .{ .ptr = null, .len = 0 };
    }

    const slice = out.toOwnedSlice(alloc) catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = slice.ptr, .len = slice.len };
}

const StyleRun = extern struct {
    start_col: u16,
    end_col: u16,
    fg_r: u8,
    fg_g: u8,
    fg_b: u8,
    bg_r: u8,
    bg_g: u8,
    bg_b: u8,
    flags: u8,
    reserved: u8,
};

fn resolvedStyle(
    default_fg: terminal.color.RGB,
    default_bg: terminal.color.RGB,
    palette: *const terminal.color.Palette,
    s: anytype,
) struct {
    fg: terminal.color.RGB,
    bg: terminal.color.RGB,
    flags: u8,
} {
    var flags: u8 = 0;
    if (s.flags.inverse) flags |= 0x01;
    if (s.flags.bold) flags |= 0x02;
    if (s.flags.italic) flags |= 0x04;
    if (s.flags.underline != .none) flags |= 0x08;
    if (s.flags.faint) flags |= 0x10;
    if (s.flags.invisible) flags |= 0x20;
    if (s.flags.strikethrough) flags |= 0x40;

    const fg = s.fg(.{ .default = default_fg, .palette = palette, .bold = null });
    return .{ .fg = fg, .bg = default_bg, .flags = flags };
}

fn dumpStyleRunsForPin(
    handle: *TerminalHandle,
    pin: terminal.Pin,
) ghostty_vt_bytes_t {
    const cells = pin.cells(.all);

    const default_fg: terminal.color.RGB = handle.default_fg;
    const default_bg: terminal.color.RGB = handle.default_bg;
    const palette: *const terminal.color.Palette = &handle.terminal.colors.palette.current;

    const alloc = std.heap.c_allocator;
    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(alloc);

    if (cells.len == 0) {
        const slice = out.toOwnedSlice(alloc) catch return .{ .ptr = null, .len = 0 };
        return .{ .ptr = slice.ptr, .len = slice.len };
    }

    var current_style_id = cells[0].style_id;
    var current_style = pin.style(&cells[0]);
    const defaults = resolvedStyle(default_fg, default_bg, palette, current_style);

    var current_flags = defaults.flags;
    var current_base_fg = defaults.fg;
    var current_inverse = current_style.flags.inverse;
    var current_invisible = current_style.flags.invisible;

    var current_bg = current_style.bg(&cells[0], palette) orelse default_bg;
    var current_fg = current_base_fg;
    if (current_inverse) {
        const tmp = current_fg;
        current_fg = current_bg;
        current_bg = tmp;
    }
    if (current_invisible) {
        current_fg = current_bg;
    }

    var current_resolved = .{ .fg = current_fg, .bg = current_bg, .flags = current_flags };
    var run_start: u16 = 1;

    var col_idx: usize = 1;
    while (col_idx < cells.len) : (col_idx += 1) {
        const cell = &cells[col_idx];
        if (cell.style_id != current_style_id) {
            const end_col: u16 = @intCast(col_idx);
            const rec = StyleRun{
                .start_col = run_start,
                .end_col = end_col,
                .fg_r = current_resolved.fg.r,
                .fg_g = current_resolved.fg.g,
                .fg_b = current_resolved.fg.b,
                .bg_r = current_resolved.bg.r,
                .bg_g = current_resolved.bg.g,
                .bg_b = current_resolved.bg.b,
                .flags = current_resolved.flags,
                .reserved = 0,
            };
            out.appendSlice(alloc, std.mem.asBytes(&rec)) catch return .{ .ptr = null, .len = 0 };

            current_style_id = cell.style_id;
            current_style = pin.style(cell);
            const resolved = resolvedStyle(default_fg, default_bg, palette, current_style);
            current_flags = resolved.flags;
            current_base_fg = resolved.fg;
            current_inverse = current_style.flags.inverse;
            current_invisible = current_style.flags.invisible;

            run_start = @intCast(col_idx + 1);

            const bg_cell = current_style.bg(cell, palette) orelse default_bg;
            var fg_cell = current_base_fg;
            var bg = bg_cell;
            if (current_inverse) {
                const tmp = fg_cell;
                fg_cell = bg;
                bg = tmp;
            }
            if (current_invisible) {
                fg_cell = bg;
            }

            current_resolved = .{ .fg = fg_cell, .bg = bg, .flags = current_flags };
            continue;
        }

        const bg_cell = current_style.bg(cell, palette) orelse default_bg;
        var fg_cell = current_base_fg;
        var bg = bg_cell;
        if (current_inverse) {
            const tmp = fg_cell;
            fg_cell = bg;
            bg = tmp;
        }
        if (current_invisible) {
            fg_cell = bg;
        }

        const same = fg_cell.r == current_resolved.fg.r and fg_cell.g == current_resolved.fg.g and fg_cell.b == current_resolved.fg.b and
            bg.r == current_resolved.bg.r and bg.g == current_resolved.bg.g and bg.b == current_resolved.bg.b and
            current_flags == current_resolved.flags;
        if (same) continue;

        const end_col: u16 = @intCast(col_idx);
        const rec = StyleRun{
            .start_col = run_start,
            .end_col = end_col,
            .fg_r = current_resolved.fg.r,
            .fg_g = current_resolved.fg.g,
            .fg_b = current_resolved.fg.b,
            .bg_r = current_resolved.bg.r,
            .bg_g = current_resolved.bg.g,
            .bg_b = current_resolved.bg.b,
            .flags = current_resolved.flags,
            .reserved = 0,
        };
        out.appendSlice(alloc, std.mem.asBytes(&rec)) catch return .{ .ptr = null, .len = 0 };

        run_start = @intCast(col_idx + 1);
        current_resolved = .{ .fg = fg_cell, .bg = bg, .flags = current_flags };
    }

    const last = StyleRun{
        .start_col = run_start,
        .end_col = @intCast(cells.len),
        .fg_r = current_resolved.fg.r,
        .fg_g = current_resolved.fg.g,
        .fg_b = current_resolved.fg.b,
        .bg_r = current_resolved.bg.r,
        .bg_g = current_resolved.bg.g,
        .bg_b = current_resolved.bg.b,
        .flags = current_resolved.flags,
        .reserved = 0,
    };
    out.appendSlice(alloc, std.mem.asBytes(&last)) catch return .{ .ptr = null, .len = 0 };

    const slice = out.toOwnedSlice(alloc) catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = slice.ptr, .len = slice.len };
}

export fn ghostty_vt_terminal_dump_viewport_row_style_runs(
    terminal_ptr: ?*anyopaque,
    row: u16,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const pt: terminal.point.Point = .{ .viewport = .{ .x = 0, .y = row } };
    const pin = handle.terminal.screens.active.pages.pin(pt) orelse return .{ .ptr = null, .len = 0 };
    return dumpStyleRunsForPin(handle, pin);
}

/// Resolve a pin pointing at a row above the current viewport top.
/// `rows_above_viewport_top = 0` means the row immediately above the viewport.
fn pinAboveViewport(handle: *TerminalHandle, rows_above_viewport_top: u32) ?terminal.Pin {
    const top: terminal.Pin = handle.terminal.screens.active.pages.getTopLeft(.viewport);
    const moved = top.upOverflow(rows_above_viewport_top + 1);
    return switch (moved) {
        .offset => |p| p,
        .overflow => null,
    };
}

/// Read a single row from scrollback above the viewport. Used by
/// pixel-smooth scroll to fetch the bleed row that appears when the
/// viewport visually slides between line boundaries.
export fn ghostty_vt_terminal_dump_screen_row(
    terminal_ptr: ?*anyopaque,
    rows_above_viewport_top: u32,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const pin = pinAboveViewport(handle, rows_above_viewport_top) orelse
        return .{ .ptr = null, .len = 0 };

    const alloc = std.heap.c_allocator;
    var builder: std.Io.Writer.Allocating = .init(alloc);
    errdefer builder.deinit();

    // dumpString emits the selection tl..br inclusive; with tl == br that
    // is a single CELL, so stretch br to the last column for a full row.
    var br = pin;
    br.x = handle.terminal.cols - 1;
    handle.terminal.screens.active.dumpString(&builder.writer, .{
        .tl = pin,
        .br = br,
        .unwrap = false,
    }) catch return .{ .ptr = null, .len = 0 };

    const slice = builder.toOwnedSlice() catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = slice.ptr, .len = slice.len };
}

export fn ghostty_vt_terminal_dump_screen_row_style_runs(
    terminal_ptr: ?*anyopaque,
    rows_above_viewport_top: u32,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const pin = pinAboveViewport(handle, rows_above_viewport_top) orelse
        return .{ .ptr = null, .len = 0 };
    return dumpStyleRunsForPin(handle, pin);
}

export fn ghostty_vt_terminal_take_dirty_viewport_rows(
    terminal_ptr: ?*anyopaque,
    rows: u16,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null or rows == 0) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const alloc = std.heap.c_allocator;

    var out: std.ArrayList(u8) = .{};
    errdefer out.deinit(alloc);

    const dirty = handle.terminal.flags.dirty;
    const force_full_redraw = dirty.clear or dirty.palette or dirty.reverse_colors or dirty.preedit;
    if (force_full_redraw) {
        handle.terminal.flags.dirty.clear = false;
        handle.terminal.flags.dirty.palette = false;
        handle.terminal.flags.dirty.reverse_colors = false;
        handle.terminal.flags.dirty.preedit = false;
    }

    var y: u32 = 0;
    while (y < rows) : (y += 1) {
        const pt: terminal.point.Point = .{ .viewport = .{ .x = 0, .y = y } };
        const pin = handle.terminal.screens.active.pages.pin(pt) orelse continue;
        if (!force_full_redraw and !pin.isDirty()) continue;

        const v: u16 = @intCast(y);
        out.append(alloc, @intCast(v & 0xFF)) catch return .{ .ptr = null, .len = 0 };
        out.append(alloc, @intCast((v >> 8) & 0xFF)) catch return .{ .ptr = null, .len = 0 };
    }

    // Ghostty 1.3 tracks dirtiness as a per-page bool + per-row bool
    // (Pin.isDirty is the OR of both) instead of the old per-page bitset.
    // We consumed the whole viewport above, so clear everything — the
    // same pattern the upstream renderer uses after a frame. Rows outside
    // the viewport never repaint from dirty flags (scroll paths re-fetch
    // them), so the broader clear is safe.
    handle.terminal.screens.active.pages.clearDirty();

    const slice = out.toOwnedSlice(alloc) catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = slice.ptr, .len = slice.len };
}

fn pinScreenRow(pin: terminal.Pin) u32 {
    var y: u32 = @intCast(pin.y);
    var node_ = pin.node;
    while (node_.prev) |node| {
        y += @intCast(node.data.size.rows);
        node_ = node;
    }
    return y;
}

/// Read the current viewport scroll position, expressed in absolute
/// scrollback coordinates. This is what a scrollbar UI needs:
///
/// * `out_viewport_top` — index (0-based) of the row currently shown at the
///   *top* of the visible viewport, counting from the oldest scrollback row.
/// * `out_total_rows` — total number of rows currently materialised in the
///   screen (scrollback history + on-screen rows). Note this is the
///   *current* size, not a maximum capacity.
/// * `out_viewport_rows` — number of rows in the viewport itself, equal to
///   the screen rows configured at construction time.
///
/// Returns `false` only if `terminal_ptr` is null. The traversal walks the
/// page list — O(pages), not O(rows) — so it's fine to call once per
/// scroll event but not per frame at 60+Hz on a large scrollback.
export fn ghostty_vt_terminal_scroll_position(
    terminal_ptr: ?*anyopaque,
    out_viewport_top: *u32,
    out_total_rows: *u32,
    out_viewport_rows: *u32,
) callconv(.c) bool {
    if (terminal_ptr == null) return false;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const top = handle.terminal.screens.active.pages.getTopLeft(.viewport);
    out_viewport_top.* = pinScreenRow(top);

    var total: u32 = 0;
    var node_ = handle.terminal.screens.active.pages.pages.first;
    while (node_) |node| {
        total += @intCast(node.data.size.rows);
        node_ = node.next;
    }
    out_total_rows.* = total;
    out_viewport_rows.* = @intCast(handle.terminal.rows);

    return true;
}

/// Hard-reset the terminal — equivalent to the `tput reset` shell command,
/// or what Terminal.app's "Reset" menu item does. Clears scrollback, resets
/// modes, scrolling region, tab stops, character set, and forces a full
/// redraw. The shell process is unaffected — it'll print its prompt on the
/// next opportunity.
export fn ghostty_vt_terminal_full_reset(terminal_ptr: ?*anyopaque) callconv(.c) void {
    if (terminal_ptr == null) return;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));
    handle.terminal.fullReset();
}

export fn ghostty_vt_terminal_take_viewport_scroll_delta(
    terminal_ptr: ?*anyopaque,
) callconv(.c) i32 {
    if (terminal_ptr == null) return 0;
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const tl = handle.terminal.screens.active.pages.getTopLeft(.viewport);
    const current: u32 = pinScreenRow(tl);

    if (!handle.has_viewport_top_y_screen) {
        handle.viewport_top_y_screen = current;
        handle.has_viewport_top_y_screen = true;
        return 0;
    }

    const prev: u32 = handle.viewport_top_y_screen;
    handle.viewport_top_y_screen = current;

    const delta64: i64 = @as(i64, @intCast(current)) - @as(i64, @intCast(prev));
    if (delta64 > std.math.maxInt(i32)) return std.math.maxInt(i32);
    if (delta64 < std.math.minInt(i32)) return std.math.minInt(i32);
    return @intCast(delta64);
}

export fn ghostty_vt_terminal_hyperlink_at(
    terminal_ptr: ?*anyopaque,
    col: u16,
    row: u16,
) callconv(.c) ghostty_vt_bytes_t {
    if (terminal_ptr == null or col == 0 or row == 0) return .{ .ptr = null, .len = 0 };
    const handle: *TerminalHandle = @ptrCast(@alignCast(terminal_ptr.?));

    const x: terminal.size.CellCountInt = @intCast(col - 1);
    const y: u32 = @intCast(row - 1);
    const pt: terminal.point.Point = .{ .viewport = .{ .x = x, .y = y } };
    const pin = handle.terminal.screens.active.pages.pin(pt) orelse return .{ .ptr = null, .len = 0 };
    const rac = pin.rowAndCell();
    if (!rac.cell.hyperlink) return .{ .ptr = null, .len = 0 };

    const id = pin.node.data.lookupHyperlink(rac.cell) orelse return .{ .ptr = null, .len = 0 };
    const entry = pin.node.data.hyperlink_set.get(pin.node.data.memory, id).*;
    const uri = entry.uri.offset.ptr(pin.node.data.memory)[0..entry.uri.len];

    const alloc = std.heap.c_allocator;
    const duped = alloc.dupe(u8, uri) catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = duped.ptr, .len = duped.len };
}

export fn ghostty_vt_encode_key_named(
    name_ptr: ?[*]const u8,
    name_len: usize,
    modifiers: u16,
) callconv(.c) ghostty_vt_bytes_t {
    if (name_ptr == null or name_len == 0) return .{ .ptr = null, .len = 0 };

    const name = name_ptr.?[0..name_len];

    const key_value: ghostty_input.Key = if (std.mem.eql(u8, name, "up"))
        .arrow_up
    else if (std.mem.eql(u8, name, "down"))
        .arrow_down
    else if (std.mem.eql(u8, name, "left"))
        .arrow_left
    else if (std.mem.eql(u8, name, "right"))
        .arrow_right
    else if (std.mem.eql(u8, name, "home"))
        .home
    else if (std.mem.eql(u8, name, "end"))
        .end
    else if (std.mem.eql(u8, name, "pageup") or std.mem.eql(u8, name, "page_up") or std.mem.eql(u8, name, "page-up"))
        .page_up
    else if (std.mem.eql(u8, name, "pagedown") or std.mem.eql(u8, name, "page_down") or std.mem.eql(u8, name, "page-down"))
        .page_down
    else if (std.mem.eql(u8, name, "insert"))
        .insert
    else if (std.mem.eql(u8, name, "delete"))
        .delete
    else if (std.mem.eql(u8, name, "backspace"))
        .backspace
    else if (std.mem.eql(u8, name, "enter"))
        .enter
    else if (std.mem.eql(u8, name, "tab"))
        .tab
    else if (std.mem.eql(u8, name, "escape"))
        .escape
    else if (name.len >= 2 and name[0] == 'f')
        parse_function_key(name[1..]) orelse return .{ .ptr = null, .len = 0 }
    else
        return .{ .ptr = null, .len = 0 };

    var mods: ghostty_input.Mods = .{};
    if ((modifiers & 0x0001) != 0) mods.shift = true;
    if ((modifiers & 0x0002) != 0) mods.ctrl = true;
    if ((modifiers & 0x0004) != 0) mods.alt = true;
    if ((modifiers & 0x0008) != 0) mods.super = true;

    const event: ghostty_input.KeyEvent = .{
        .action = .press,
        .key = key_value,
        .mods = mods,
    };

    // Ghostty 1.3: KeyEncoder struct became key_encode.encode(writer, ...).
    var buf: [128]u8 = undefined;
    var writer = std.Io.Writer.fixed(buf[0..]);
    ghostty_input.key_encode.encode(
        &writer,
        event,
        .{ .alt_esc_prefix = true },
    ) catch return .{ .ptr = null, .len = 0 };
    const encoded = writer.buffered();
    if (encoded.len == 0) return .{ .ptr = null, .len = 0 };

    const alloc = std.heap.c_allocator;
    const duped = alloc.dupe(u8, encoded) catch return .{ .ptr = null, .len = 0 };
    return .{ .ptr = duped.ptr, .len = duped.len };
}

fn parse_function_key(digits: []const u8) ?ghostty_input.Key {
    if (digits.len == 1) {
        return switch (digits[0]) {
            '1' => .f1,
            '2' => .f2,
            '3' => .f3,
            '4' => .f4,
            '5' => .f5,
            '6' => .f6,
            '7' => .f7,
            '8' => .f8,
            '9' => .f9,
            else => null,
        };
    }

    if (digits.len == 2 and digits[0] == '1') {
        return switch (digits[1]) {
            '0' => .f10,
            '1' => .f11,
            '2' => .f12,
            else => null,
        };
    }

    return null;
}

const ghostty_vt_bytes_t = extern struct {
    ptr: ?[*]const u8,
    len: usize,
};

export fn ghostty_vt_bytes_free(bytes: ghostty_vt_bytes_t) callconv(.c) void {
    if (bytes.ptr == null or bytes.len == 0) return;
    std.heap.c_allocator.free(bytes.ptr.?[0..bytes.len]);
}

// Ghostty's terminal stream uses this symbol as an optimization hook.
// Provide a portable scalar implementation so we don't need C++ SIMD deps.
export fn ghostty_simd_decode_utf8_until_control_seq(
    input: [*]const u8,
    count: usize,
    output: [*]u32,
    output_count: *usize,
) callconv(.c) usize {
    var i: usize = 0;
    var out_i: usize = 0;
    while (i < count) {
        if (input[i] == 0x1B) break;

        const b0 = input[i];
        var cp: u32 = 0xFFFD;
        var need: usize = 1;

        if (b0 < 0x80) {
            cp = b0;
            need = 1;
        } else if (b0 & 0xE0 == 0xC0) {
            need = 2;
            if (i + need > count) break;
            const b1 = input[i + 1];
            if (b1 & 0xC0 != 0x80) {
                cp = 0xFFFD;
                need = 1;
            } else {
                cp = ((@as(u32, b0 & 0x1F)) << 6) | (@as(u32, b1 & 0x3F));
            }
        } else if (b0 & 0xF0 == 0xE0) {
            need = 3;
            if (i + need > count) break;
            const b1 = input[i + 1];
            const b2 = input[i + 2];
            if (b1 & 0xC0 != 0x80 or b2 & 0xC0 != 0x80) {
                cp = 0xFFFD;
                need = 1;
            } else {
                cp = ((@as(u32, b0 & 0x0F)) << 12) |
                    ((@as(u32, b1 & 0x3F)) << 6) |
                    (@as(u32, b2 & 0x3F));
            }
        } else if (b0 & 0xF8 == 0xF0) {
            need = 4;
            if (i + need > count) break;
            const b1 = input[i + 1];
            const b2 = input[i + 2];
            const b3 = input[i + 3];
            if (b1 & 0xC0 != 0x80 or b2 & 0xC0 != 0x80 or b3 & 0xC0 != 0x80) {
                cp = 0xFFFD;
                need = 1;
            } else {
                cp = ((@as(u32, b0 & 0x07)) << 18) |
                    ((@as(u32, b1 & 0x3F)) << 12) |
                    ((@as(u32, b2 & 0x3F)) << 6) |
                    (@as(u32, b3 & 0x3F));
            }
        } else {
            cp = 0xFFFD;
            need = 1;
        }

        output[out_i] = cp;
        out_i += 1;
        i += need;
    }

    output_count.* = out_i;
    return i;
}
