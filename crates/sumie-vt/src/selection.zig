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

pub fn install() void {
    @export(&initLinks, .{ .name = "sumie_selection_init_links" });
    @export(&linkAt, .{ .name = "sumie_selection_link_at" });
}

// Rust initializes this once before publishing it to any terminal thread.
var regex: oni.Regex = undefined;

fn initLinks() callconv(.c) bool {
    oni.init(&.{oni.Encoding.utf8}) catch return false;
    regex = oni.Regex.init(url.regex, .{}, oni.Encoding.utf8, oni.Syntax.default, null) catch return false;
    return true;
}

fn linkAt(terminal: Terminal.Terminal, reference: *const GridRef, out: *Selection, writer: ?*const Writer) callconv(.c) Result {
    const t = Terminal.zigTerminal(terminal) orelse return .invalid_value;
    var pin = reference.toPin() orelse return .invalid_value;
    const screen = t.screens.active;
    if (writer) |sink| {
        if (pin.rowAndCell().cell.wide == .spacer_tail) pin = pin.leftWrap(1).?;
        if (oscEntry(pin)) |entry| {
            const uri = entry.uri.slice(pin.node.page().memory);
            if (!sink.emit(uri)) return .no_value;
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
            if (!sink.emit(map.string[start..end])) return .no_value;
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
        const candidate = oscEntry(next) orelse break;
        if (origin.node == next.node) {
            if (entry != candidate) break;
        } else if (!entry.eql(origin.node.page().memory, candidate, next.node.page().memory)) break;
        edge = next;
    }
    return edge;
}
