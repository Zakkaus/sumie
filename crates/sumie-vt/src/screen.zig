//! VT title stack and viewport reads that preserve renderer dirty bits.
const std = @import("std");
const options = @import("terminal_options");
const terminal = @import("terminal/c/terminal.zig");
const render = @import("terminal/c/render.zig");
const color = @import("terminal/color.zig");
const Result = @import("terminal/c/result.zig").Result;
const Color = @import("terminal/style.zig").Style.Color;

pub fn install() void {
    @export(&stamp, .{ .name = "sumie_screen_stamp" });
    @export(&paletteColor, .{ .name = "sumie_palette_color" });
    @export(&semanticPromptSeen, .{ .name = "sumie_semantic_prompt_seen" });
}

pub const TitleStack = struct {
    // Match xterm's ten slots. An explicit index addresses a slot without moving the stack.
    slots: [10]?std.ArrayList(u8) = @splat(null),
    next: usize = 0,
    depth: usize = 0,

    pub fn save(t: *terminal.ZigTerminal, index: u16) !void {
        const stack = &t.sumie_titles;
        const slot = if (index == 0) stack.next else (index - 1) % stack.slots.len;
        var saved = stack.slots[slot] orelse std.ArrayList(u8).empty;
        try saved.resize(t.gpa(), t.title.items.len);
        @memcpy(saved.items, t.title.items);
        stack.slots[slot] = saved;
        if (index == 0) {
            stack.next = (slot + 1) % stack.slots.len;
            stack.depth = @min(stack.depth + 1, stack.slots.len);
        }
    }

    pub fn restore(t: *terminal.ZigTerminal, index: u16) !bool {
        const stack = &t.sumie_titles;
        if (index != 0) {
            const saved = stack.slots[(index - 1) % stack.slots.len] orelse return false;
            const bytes = saved.items;
            try t.setTitle(if (bytes.len == 0) "" else bytes[0 .. bytes.len - 1]);
        } else {
            if (stack.depth == 0) return false;
            stack.next = (stack.next + stack.slots.len - 1) % stack.slots.len;
            stack.depth -= 1;
            t.title.deinit(t.gpa());
            t.title = stack.slots[stack.next].?;
            stack.slots[stack.next] = null;
        }
        return true;
    }

    pub fn clear(self: *TitleStack, alloc: std.mem.Allocator) void {
        for (&self.slots) |*slot| if (slot.*) |*saved| saved.deinit(alloc);
        self.* = .{};
    }
};

fn paletteColor(raw: render.RenderState, index: u8, out: *[3]u8) callconv(.c) Result {
    const snapshot = raw orelse return .invalid_value;
    const rgb = snapshot.state.colors.palette[index];
    out.* = .{ rgb.r, rgb.g, rgb.b };
    return .success;
}

fn semanticPromptSeen(raw: terminal.Terminal) callconv(.c) bool {
    const t = terminal.zigTerminal(raw) orelse return false;
    return t.screens.get(.primary).?.semantic_prompt.seen;
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
