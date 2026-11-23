const std = @import("std");
const oni = @import("oniguruma");
const url = @import("config/url.zig");
const Terminal = @import("terminal/c/terminal.zig");
const GridRef = @import("terminal/c/grid_ref.zig").CGridRef;
const Selection = @import("terminal/c/selection.zig").CSelection;
const Result = @import("terminal/c/result.zig").Result;
const Pin = @import("terminal/PageList.zig").Pin;
const PageEntry = @import("terminal/hyperlink.zig").PageEntry;
const NativeSelection = @import("terminal/Selection.zig");

const Writer = extern struct {
    write: *const fn (?*anyopaque, [*]const u8, usize) callconv(.c) bool,
    userdata: ?*anyopaque,

    fn emit(self: Writer, bytes: []const u8) bool {
        return self.write(self.userdata, bytes.ptr, bytes.len);
    }
};

const LinkSink = extern struct {
    uri: Writer,
    span: *const fn (?*anyopaque, u16, u16, u16) callconv(.c) void,
};

pub fn install() void {
    @export(&initLinks, .{ .name = "sumie_selection_init_links" });
    @export(&linkAt, .{ .name = "sumie_selection_link_at" });
    @export(&linkStamp, .{ .name = "sumie_selection_link_stamp" });
}

// Rust initializes this once before publishing it to any terminal thread.
var regex: oni.Regex = undefined;

fn initLinks() callconv(.c) bool {
    oni.init(&.{oni.Encoding.utf8}) catch return false;
    regex = oni.Regex.init(url.regex, .{}, oni.Encoding.utf8, oni.Syntax.default, null) catch return false;
    return true;
}

fn linkAt(terminal: Terminal.Terminal, reference: *const GridRef, out: *Selection, writer: ?*const LinkSink) callconv(.c) Result {
    const t = Terminal.zigTerminal(terminal) orelse return .invalid_value;
    var pin = reference.toPin() orelse return .invalid_value;
    const screen = t.screens.active;
    if (writer) |sink| {
        if (pin.rowAndCell().cell.wide == .spacer_tail) pin = pin.leftWrap(1).?;
        if (oscEntry(pin)) |entry| {
            const uri = entry.uri.slice(pin.node.page().memory);
            if (!sink.uri.emit(uri)) return .no_value;
            if (entry.id == .explicit) {
                var rows = screen.pages.getTopLeft(.viewport).rowIterator(.right_down, screen.pages.getBottomRight(.viewport));
                var y: u16 = 0;
                while (rows.next()) |row| : (y += 1) {
                    if (row.rowAndCell().row.hyperlink) emitRuns(sink, y, row, pin, entry);
                }
                return .success;
            }
            const first = oscEdge(pin, entry, screen.pages.getTopLeft(.viewport), .left);
            const last = oscEdge(pin, entry, screen.pages.getBottomRight(.viewport).?, .right);
            out.* = .fromZig(NativeSelection.init(first, last, false));
            return .success;
        }
    }
    // Match Surface.linkAtPin without link configuration or modifier checks.
    const line = screen.selectLine(.{ .pin = pin, .whitespace = null, .semantic_prompt_boundary = true }) orelse return .no_value;
    const alloc = std.heap.c_allocator;
    const map = screen.selectionStringMap(alloc, .{ .sel = line, .trim = false }) catch return .out_of_memory;
    defer map.deinit(alloc);

    var iterator = map.searchIterator(regex);
    while (iterator.next() catch return .no_value) |m| {
        var match = m;
        defer match.deinit();
        const selection = match.selection();
        if (!selection.contains(screen, pin)) continue;
        if (writer) |sink| {
            const start = match.offset + @as(usize, @intCast(match.region.starts()[0]));
            const end = match.offset + @as(usize, @intCast(match.region.ends()[0]));
            if (!sink.uri.emit(map.string[start..end])) return .no_value;
        }
        out.* = .fromZig(selection);
        return .success;
    }
    return .no_value;
}

fn oscEntry(pin: Pin) ?*const PageEntry {
    const page = pin.node.page();
    const cell = pin.rowAndCell().cell;
    if (!cell.hyperlink) return null;
    const id = page.lookupHyperlink(cell) orelse return null;
    return page.hyperlink_set.get(page.memory, id);
}

// ID and URI together identify explicit links, including across page boundaries.
fn sameEntry(origin: Pin, entry: *const PageEntry, pin: Pin) bool {
    const candidate = oscEntry(pin) orelse return false;
    return if (origin.node == pin.node) entry == candidate else entry.eql(origin.node.page().memory, candidate, pin.node.page().memory);
}

fn emitRuns(sink: *const LinkSink, y: u16, row: Pin, origin: Pin, entry: *const PageEntry) void {
    var start: ?u16 = null;
    var end: u16 = 0;
    var pin = row;
    for (row.cells(.all), 0..) |cell, x| {
        pin.x = @intCast(x);
        if (cell.wide == .spacer_tail) continue;
        if (sameEntry(origin, entry, pin)) {
            if (start == null) start = pin.x;
            end = pin.x + @intFromBool(cell.wide == .wide);
        } else if (start) |left| {
            sink.span(sink.uri.userdata, y, left, end);
            start = null;
        }
    }
    if (start) |left| sink.span(sink.uri.userdata, y, left, end);
}

fn oscEdge(origin: Pin, entry: *const PageEntry, limit: Pin, comptime direction: enum { left, right }) Pin {
    var edge = origin;
    var next = origin;
    while (!next.eql(limit)) {
        next = switch (direction) {
            .left => next.leftWrap(1).?,
            .right => next.rightWrap(1).?,
        };
        // Wide glyphs and wrap padding do not break a run.
        switch (next.rowAndCell().cell.wide) {
            .spacer_head, .spacer_tail => continue,
            else => {},
        }
        if (!sameEntry(origin, entry, next)) break;
        edge = next;
    }
    return edge;
}

// The render snapshot consumes visible rows' dirty bits. Offscreen continuations
// may stay dirty indefinitely; fingerprint their text instead of rerunning the regex.
fn linkStamp(terminal: Terminal.Terminal, x: u16, y: u16, stamp: *u64) callconv(.c) bool {
    const t = Terminal.zigTerminal(terminal) orelse return true;
    const screen = t.screens.active;
    const pages = &screen.pages;
    if (y >= pages.rows) return true;
    var hit = pages.pin(.{ .viewport = .{ .x = x, .y = y } }) orelse return true;
    if (hit.rowAndCell().cell.wide == .spacer_tail) hit = hit.leftWrap(1).?;
    const osc = hit.rowAndCell().cell.hyperlink;
    var first = hit;
    var last = hit;
    first.x = 0;
    last.x = 0;
    var above: usize = 0;
    if (osc) {
        first = pages.getTopLeft(.viewport);
        last = pages.getBottomRight(.viewport).?;
    } else {
        while (first.up(1)) |prev| {
            if (!prev.rowAndCell().row.wrap) break;
            first = prev;
            above += 1;
        }
        while (last.rowAndCell().row.wrap) last = last.down(1) orelse break;
    }
    first.x = 0;
    last.x = last.node.cols() - 1;
    var hash = std.hash.Wyhash.init(@intCast(@intFromEnum(t.screens.active_key)));
    std.hash.autoHash(&hash, .{ pages.cols, pages.rows, hit.node.serial, hit.y, osc });
    var dirty = !std.meta.eql(t.flags.dirty, .{}) or !std.meta.eql(screen.dirty, .{});
    var rows = first.rowIterator(.right_down, last);
    var index: usize = 0;
    while (rows.next()) |row| : (index += 1) {
        if (osc and !row.rowAndCell().row.hyperlink) continue;
        std.hash.autoHash(&hash, .{ index, row.node.serial, row.y });
        if (!row.isDirty()) continue;
        if (osc or (index + y >= above and index + y - above < pages.rows)) {
            dirty = true;
        } else {
            const cells = row.cells(.all);
            hash.update(std.mem.sliceAsBytes(cells));
            if (row.rowAndCell().row.grapheme) for (cells) |*cell| {
                if (cell.hasGrapheme()) if (row.grapheme(cell)) |extra| {
                    std.hash.autoHash(&hash, extra.len);
                    hash.update(std.mem.sliceAsBytes(extra));
                };
            };
        }
    }
    stamp.* = hash.final();
    return dirty;
}
