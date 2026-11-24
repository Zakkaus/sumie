//! Fingerprint the live viewport without copying rows or consuming renderer dirty bits.
const std = @import("std");
const options = @import("terminal_options");
const terminal = @import("terminal/c/terminal.zig");
const color = @import("terminal/color.zig");
const Color = @import("terminal/style.zig").Style.Color;

pub fn install() void {
    @export(&stamp, .{ .name = "sumie_screen_stamp" });
}

fn stamp(raw: terminal.Terminal, has_placements: *bool) callconv(.c) u64 {
    const t = terminal.zigTerminal(raw) orelse return 0;
    const screen = t.screens.active;
    has_placements.* = if (comptime options.kitty_graphics)
        screen.kitty_images.placements.count() != 0
    else
        false;
    const pages = &screen.pages;
    const palette = &t.colors.palette.current;
    var hash = std.hash.Wyhash.init(0);
    std.hash.autoHash(&hash, pages.cols);
    std.hash.autoHash(&hash, pages.rows);
    const reverse = t.modes.get(.reverse_colors);
    const fg = t.colors.foreground.get() orelse color.RGB{ .r = 255, .g = 255, .b = 255 };
    const bg = t.colors.background.get() orelse color.RGB{};
    std.hash.autoHash(&hash, if (reverse) bg else fg);
    std.hash.autoHash(&hash, if (reverse) fg else bg);
    var rows = pages.getTopLeft(.viewport).rowIterator(.right_down, pages.getBottomRight(.viewport));
    while (rows.next()) |row| for (row.cells(.all)) |*cell| {
        const styled = cell.hasStyling();
        var ink = cell.*;
        // Style IDs are page-local. Protection and prompt marks are not visible.
        ink.style_id = @intFromBool(styled);
        ink.protected = false;
        ink.semantic_content = .output;
        if (ink.content_tag == .bg_color_palette) {
            const rgb = palette[ink.content.color_palette.data];
            ink.content_tag = .bg_color_rgb;
            ink.content = .{ .color_rgb = .{ .r = rgb.r, .g = rgb.g, .b = rgb.b } };
        }
        std.hash.autoHash(&hash, @as(u64, @bitCast(ink)));
        if (styled) {
            var style = row.style(cell);
            style.fg_color = resolve(style.fg_color, palette);
            style.bg_color = resolve(style.bg_color, palette);
            style.underline_color = resolve(style.underline_color, palette);
            std.hash.autoHash(&hash, style.bits());
        }
        if (cell.hasGrapheme()) {
            const extra = row.grapheme(cell) orelse &.{};
            std.hash.autoHash(&hash, extra.len);
            for (extra) |cp| std.hash.autoHash(&hash, @as(u32, cp));
        }
    };
    return hash.final();
}

fn resolve(value: Color, palette: *const color.Palette) Color {
    return switch (value.value()) {
        .palette => |index| .rgb(palette[index]),
        else => value,
    };
}
