const std = @import("std");
const oni = @import("oniguruma");
const url = @import("config/url.zig");
const Terminal = @import("terminal/c/terminal.zig");
const GridRef = @import("terminal/c/grid_ref.zig").CGridRef;
const Selection = @import("terminal/c/selection.zig").CSelection;
const Result = @import("terminal/c/result.zig").Result;

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

fn linkAt(terminal: Terminal.Terminal, reference: *const GridRef, out: *Selection) callconv(.c) Result {
    const t = Terminal.zigTerminal(terminal) orelse return .invalid_value;
    const pin = reference.toPin() orelse return .invalid_value;
    const screen = t.screens.active;
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
        out.* = .fromZig(selection);
        return .success;
    }
    return .no_value;
}
