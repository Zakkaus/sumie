//! Patches Ghostty's Kitty transport and resident animation storage policies.

use super::{insert_once, replace_once};
use std::{fs, path::Path};

pub(super) fn install(source: &Path) {
    let kitty = source.join("src/terminal/kitty");
    install_transport(source, &kitty);
    fs::write(
        kitty.join("sumie_animation.zig"),
        include_str!("kitty_animation.zig"),
    )
    .expect("install transactional animation allocation");

    let exec = kitty.join("graphics_exec.zig");
    patch_frame_commands(&exec);
    patch_frame_accounting(&kitty, &exec);
    patch_frame_deletion(&kitty.join("graphics_storage.zig"));
}

fn install_transport(source: &Path, kitty: &Path) {
    // Wuffs is already linked for pixel operations. Select Ghostty's bounded PNG decoder
    // at build time instead of mutating its process-wide callback after startup.
    insert_once(
        &source.join("src/terminal/sys.zig"),
        "pub var decode_png: ?DecodePngFn = png: {\n",
        "    if (build_options.kitty_graphics) break :png &decodePngWuffs;\n",
    );
    fs::write(
        kitty.join("sumie_file.zig"),
        include_str!("../src/image_file.zig"),
    )
    .expect("install confined image file reader");
    replace_between(
        &kitty.join("graphics_image.zig"),
        "            else => @compileError(\"readFile only supports file and temporary_file\"),\n        }\n",
        "\n    }\n",
        "\n        assert(self.data.items.len == 0);\n        self.data = try @import(\"sumie_file.zig\").read(io, alloc, path, medium == .temporary_file, t.offset, t.size);",
    );
}

fn patch_frame_commands(path: &Path) {
    insert_once(
        path,
        "const animation = @import(\"graphics_animation.zig\");\n",
        "const frame_update = @import(\"sumie_animation.zig\");\n",
    );
    replace_once(
        path,
        &[
            (
                "    var img = storage.imagePtrByIdOrNumber(\n        loading.image.id,",
                "    const img = storage.imagePtrByIdOrNumber(\n        loading.image.id,",
            ),
            (
                "const EncodeableError = Image.Error || Allocator.Error;",
                "const EncodeableError = Image.Error || frame_update.Error;",
            ),
        ],
    );
    insert_once(
        path,
        "        error.OutOfMemory => r.message = \"ENOMEM: out of memory\",\n",
        "        error.AnimationStorageFull => r.message = \"ENOSPC: animation frame storage full\",\n",
    );
    patch_frame_loading(path);
    patch_frame_composition(path);
}

fn patch_frame_loading(path: &Path) {
    replace_between(
        path,
        "        result.message = \"EINVAL: frame dimensions exceed image\";\n        return result;\n    }\n",
        "\n/// Control animation playback (a=a).",
        r#"
    // r in 1..count edits a frame; otherwise append and report the assigned number.
    const count: u32 = if (img.animation) |anim| anim.frameCount() else 1;
    const number = if (f.edit_frame == 0 or f.edit_frame > count) count + 1 else f.edit_frame;
    result.frame = number;
    const append = number > count;
    if (append) {
        if (f.create_frame > 0 and img.frameData(f.create_frame) == null) {
            result.message = "EINVAL: base frame not found";
            return result;
        }
    } else if (img.frameData(number) == null) {
        result.message = "EINVAL: frame data incomplete";
        return result;
    }

    if (frame_img.format != .rgba) {
        const rgba = pixel.rgbaFromFormat(alloc, frame_img.format, frame_img.data.bytes().?) catch |err| {
            encodeError(&result, err);
            return result;
        };
        frame_img.data.deinit(alloc);
        frame_img.data = .{ .complete = rgba };
        frame_img.format = .rgba;
    }
    const prepared = frame_update.prepare(io, alloc, terminal.screens.active, img, if (append) .append else .edit) catch |err| {
        encodeError(&result, err);
        return result;
    };
    const target = prepared.image;
    const anim = target.animation.?;
    const dst = if (prepared.canvas) |canvas| dst: {
        if (f.create_frame > 0) {
            @memcpy(canvas, target.frameData(f.create_frame).?);
        } else {
            pixel.fillBackground(canvas, f.background);
        }
        break :dst canvas;
    } else @constCast(target.frameData(number).?);

    pixel.composeRect(
        dst,
        target.width,
        target.height,
        frame_img.data.bytes().?,
        frame_img.width,
        frame_img.height,
        f.x,
        f.y,
        f.composition_mode,
    );

    if (append) {
        anim.frames.appendAssumeCapacity(.{
            .data = dst,
            .gap_ms = if (f.gap_ms == 0) animation.default_gap_ms else @intCast(@max(0, f.gap_ms)),
        });
    } else if (f.gap_ms != 0) {
        anim.setGapAt(number - 1, @intCast(@max(0, f.gap_ms)));
    }

    const edited_current = !append and number - 1 == anim.current_index;
    if (edited_current) anim.frame_shown_at_ms = null;
    if (edited_current or prepared.converted_root) {
        storage.markImageContentChanged(io, target);
    } else {
        storage.markMutated(io);
    }
    return result;
}
"#,
    );
}

fn patch_frame_composition(path: &Path) {
    replace_once(
        path,
        &[
            (
                "    const img = storage.imagePtrByIdOrNumber(\n        c.image_id,",
                "    var img = storage.imagePtrByIdOrNumber(\n        c.image_id,",
            ),
            (
                r"    storage.convertImageToRgba(
        io,
        alloc,
        img,
    ) catch |err| {
        encodeError(&result, err);
        return result;
    };",
                r"    const prepared = frame_update.prepare(io, alloc, terminal.screens.active, img, .compose) catch |err| {
        encodeError(&result, err);
        return result;
    };
    img = prepared.image;",
            ),
            (
                "    if (c.dest_frame - 1 == current) {",
                "    if (prepared.converted_root or c.dest_frame - 1 == current) {",
            ),
        ],
    );
}

fn patch_frame_accounting(kitty: &Path, exec: &Path) {
    replace_once(
        &kitty.join("graphics_animation.zig"),
        &[
            (
                "    /// Bytes of frame data held by this animation, counted against\n    /// the image storage limit. Pending frames count at their full size.\n    pub fn frameBytes(self: *const Animation) usize {\n        var total: usize = 0;",
                r"    pub fn metadataBytes(self: *const Animation) usize {
        return self.frames.capacity * @sizeOf(Frame);
    }

    /// Resident frame metadata and pixel bytes, including pending pixels.
    pub fn frameBytes(self: *const Animation) usize {
        var total: usize = self.metadataBytes();",
            ),
            (
                "try testing.expectEqual(@as(usize, 4), anim.frameBytes());",
                "try testing.expectEqual(4 + anim.metadataBytes(), anim.frameBytes());",
            ),
        ],
    );
    let storage = kitty.join("graphics_storage.zig");
    replace_between(
        &storage,
        "        img.generation = self.generation;\n    }\n\n",
        "    /// Reserve `bytes` of storage for animation frame data belonging",
        "",
    );
    replace_between(
        &storage,
        "const animation = @import(\"graphics_animation.zig\");\n",
        "const command = @import(\"graphics_command.zig\");\n",
        "",
    );
    patch_accounting_assertions(&storage, exec);
}

fn patch_accounting_assertions(storage: &Path, exec: &Path) {
    replace_once(
        exec,
        &[
            (
                "    // Frame bytes count against the storage total.\n    try testing.expectEqual(@as(usize, 8), storage.total_bytes);",
                "    try testing.expectEqual(8 + anim.metadataBytes(), storage.total_bytes);",
            ),
            (
                "try testing.expectEqual(total_before - 8, storage.total_bytes);",
                "try testing.expectEqual(@as(usize, 4), storage.total_bytes);",
            ),
            (
                "try testing.expectEqual(@as(usize, 8), storage.total_bytes);\n\n    // Retransmit the base image:",
                "try testing.expectEqual(8 + storage.images.get(1).?.animation.?.metadataBytes(), storage.total_bytes);\n\n    // Retransmit the base image:",
            ),
        ],
    );
    replace_once(
        storage,
        &[
            (
                "    try testing.expectEqual(@as(usize, 12), s.total_bytes);\n    try testing.expectEqual(@as(usize, 12), s.imageById(1).?.storageSize());",
                "    try testing.expectEqual(12 + anim.metadataBytes(), s.total_bytes);\n    try testing.expectEqual(s.total_bytes, s.imageById(1).?.storageSize());",
            ),
            (
                "    try testing.expectEqual(@as(usize, 8), s.total_bytes);\n    try testing.expect(s.animationTick(io, 0) != null);",
                "    try testing.expectEqual(8 + anim.metadataBytes(), s.total_bytes);\n    try testing.expect(s.animationTick(io, 0) != null);",
            ),
        ],
    );
}

fn patch_frame_deletion(path: &Path) {
    replace_between(
        path,
        "            self.releaseAnimationBytes(removed.storageSize());\n            alloc.free(removed.data);\n        }\n",
        "\n    /// Delete an image if it is unused.",
        r"
        if (anim.frames.items.len == 0) {
            self.releaseAnimationBytes(anim.metadataBytes());
            anim.frames.clearAndFree(alloc);
        }

        // The current index includes the root; the frame array does not.
        const removed_index = number - 1;
        if (anim.current_index == removed_index) {
            // Keep the successor at this index, or the predecessor if
            // the removed frame was the last one.
            anim.current_index = @min(removed_index, @as(u32, @intCast(anim.frames.items.len)));
            anim.frame_shown_at_ms = null;
            self.markImageContentChanged(io, img);
        } else {
            if (anim.current_index > removed_index) anim.current_index -= 1;
            self.markMutated(io);
        }
    }
",
    );
}

fn replace_between(path: &Path, before: &str, after: &str, replacement: &str) {
    let mut text = fs::read_to_string(path).expect("read pinned Ghostty source");
    let start = text.find(before).expect("find start of pinned patch") + before.len();
    let end = start + text[start..].find(after).expect("find end of pinned patch");
    if text[start..end] != *replacement {
        text.replace_range(start..end, replacement);
        fs::write(path, text).expect("patch pinned Ghostty source");
    }
}
