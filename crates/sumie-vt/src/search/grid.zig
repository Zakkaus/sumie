//! Project pinned Ghostty page rows into the search ABI without copying styles.
const std = @import("std");
const uucode = @import("uucode");
const grid_ref = @import("terminal/c/grid_ref.zig");
const GridRef = grid_ref.CGridRef;
const Result = @import("terminal/c/result.zig").Result;

pub fn install() void {
    @export(&readRow, .{ .name = "sumie_search_read_row" });
    @export(&checkRows, .{ .name = "sumie_search_check_rows" });
    @export(&lineStart, .{ .name = "sumie_search_line_start" });
    @export(&graphemes, .{ .name = "sumie_search_graphemes" });
    @export(&fold, .{ .name = "sumie_search_fold" });
}

const Row = extern struct {
    len: u16,
    cols: u16,
    wrapped: bool,
};

// This is Sumie's projection, not the layout of a Ghostty cell.
const Glyph = packed struct(u32) {
    codepoint: u21 = 0,
    wide: bool = false,
    spacer: bool = false,
    grapheme: bool = false,
    padding: u8 = 0,
};

fn readRow(reference: *const GridRef, next: *GridRef, buf: [*]u32, cap: u16, meta: *Row) callconv(.c) bool {
    const pin = reference.toPin() orelse return false;
    const page = pin.node.page();
    const row = page.getRow(pin.y);
    const cells = page.getCells(row);
    const cols = @min(cells.len, cap);

    // Padding at the end of a hard line never reaches the matcher. On soft
    // wraps it can be significant if subsequent rows contain text.
    var len = cols;
    if (!row.wrap) while (len > 0 and !cells[len - 1].hasText()) {
        len -= 1;
    };
    for (cells[0..len], buf[0..len]) |cell, *out| {
        const glyph: Glyph = switch (cell.wide) {
            .spacer_head, .spacer_tail => .{ .spacer = true },
            .narrow, .wide => .{
                .codepoint = cell.codepoint(),
                .wide = cell.wide == .wide,
                .grapheme = cell.hasGrapheme(),
            },
        };
        out.* = @bitCast(glyph);
    }
    meta.* = .{ .len = @intCast(len), .cols = @intCast(cols), .wrapped = row.wrap };
    next.* = if (pin.down(1)) |p| GridRef.fromPin(p) else .{};
    return true;
}

const Stamp = extern struct { hash: u64, dirty: bool, neutral: bool };

// Render updates consume Ghostty's dirty bits, even though search may be
// reading a different viewport. Keep fingerprints only for scanned active rows.
fn checkRows(reference: *const GridRef, stamps: [*]Stamp, len: usize) callconv(.c) bool {
    var pin = reference.toPin() orelse return false;
    for (stamps[0..len], 0..) |*stamp, i| {
        const page = pin.node.page();
        const row = page.getRow(pin.y);
        const cells = page.getCells(row);
        var hash = std.hash.Wyhash.init(@intFromBool(row.wrap));
        hash.update(std.mem.sliceAsBytes(cells));
        if (row.grapheme) for (cells) |*cell| {
            if (cell.hasGrapheme()) if (pin.grapheme(cell)) |extra| {
                const n = extra.len;
                hash.update(std.mem.asBytes(&n));
                hash.update(std.mem.sliceAsBytes(extra));
            };
        };
        const value = hash.final();
        stamp.dirty = stamp.dirty or stamp.hash != value;
        stamp.hash = value;
        if (i + 1 < len) pin = pin.down(1) orelse return false;
    }
    return true;
}

fn lineStart(reference: *const GridRef, boundary: u32) callconv(.c) u32 {
    var pin = reference.toPin() orelse return boundary;
    var row = boundary;
    while (pin.rowAndCell().row.wrap) {
        row -= 1;
        pin = pin.up(1) orelse break;
    }
    return row;
}

fn graphemes(reference: *const GridRef, col: u16, buf: [*]u32, len: usize, written: *usize) callconv(.c) Result {
    var cell = reference.*;
    cell.x = col;
    return grid_ref.grid_ref_graphemes(&cell, buf, len, written);
}

fn fold(cp: u32) callconv(.c) u32 {
    return uucode.get(.case_folding_simple, @intCast(cp));
}
