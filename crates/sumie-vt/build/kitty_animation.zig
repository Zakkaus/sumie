const std = @import("std");
const Allocator = std.mem.Allocator;
const Screen = @import("../Screen.zig");
const Animation = @import("graphics_animation.zig").Animation;
const Image = @import("graphics_image.zig").Image;
const pixel = @import("graphics_pixel.zig");

pub const Error = Allocator.Error || error{AnimationStorageFull};

const Prepared = struct {
    image: *Image,
    canvas: ?[]u8,
    converted_root: bool,
};

/// Preflight the net growth before conversion. Stage every fallible allocation
/// before evicting anything or updating the image and its byte accounting.
/// The caller must attach the returned canvas before returning to the terminal.
pub fn prepare(
    io: std.Io,
    alloc: Allocator,
    screen: *Screen,
    original: *Image,
    mode: enum { append, edit, compose },
) Error!Prepared {
    const storage = &screen.kitty_images;
    const old_anim = original.animation;
    const rgba_len = @as(usize, original.width) * original.height * 4;
    const root_growth = rgba_len - original.data.len();
    const canvas_len = if (mode == .append) rgba_len else 0;
    const old_capacity = if (mode == .append and old_anim != null) old_anim.?.frames.capacity else 0;
    const capacity = if (mode == .append) capacity: {
        const required = if (old_anim) |anim| anim.frames.items.len + 1 else 1;
        break :capacity if (required > old_capacity)
            std.ArrayList(Animation.Frame).growCapacity(required)
        else
            old_capacity;
    } else 0;
    const growth = root_growth + canvas_len + (capacity - old_capacity) * @sizeOf(Animation.Frame);
    const evict = growth > storage.total_limit - storage.total_bytes;
    if (evict and (growth > storage.total_limit or
        original.storageSize() > storage.total_limit - growth)) return error.AnimationStorageFull;

    const rgba = if (root_growth > 0)
        try pixel.rgbaFromFormat(alloc, original.format, original.data.bytes().?)
    else
        null;
    errdefer if (rgba) |data| alloc.free(data);

    const canvas = if (mode == .append) try alloc.alloc(u8, canvas_len) else null;
    errdefer if (canvas) |data| alloc.free(data);

    const new_anim = if (old_anim == null and mode != .compose) try alloc.create(Animation) else null;
    errdefer if (new_anim) |anim| alloc.destroy(anim);
    if (new_anim) |anim| anim.* = .{};
    if (capacity > old_capacity) {
        const anim = old_anim orelse new_anim.?;
        try anim.frames.ensureTotalCapacityPrecise(alloc, capacity);
    }

    // A successful preflight guarantees the growth fits after eviction. No
    // allocation can fail after the frame array has grown in place.
    const id = original.id;
    if (growth > 0) storage.reserveAnimationBytes(io, alloc, screen, id, growth) catch unreachable;
    const img = if (evict) storage.imagePtrByIdOrNumber(id, 0).? else original;
    if (rgba) |data| {
        img.data.deinit(alloc);
        img.data = .{ .complete = data };
        img.format = .rgba;
    }
    if (new_anim) |anim| img.animation = anim;
    return .{ .image = img, .canvas = canvas, .converted_root = rgba != null };
}
