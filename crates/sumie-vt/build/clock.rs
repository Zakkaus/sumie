use std::fs;
use std::path::Path;

use super::{insert_once, replace_once};

pub(super) fn install(source: &Path) {
    let terminal = source.join("src/terminal");
    let kitty = terminal.join("kitty");
    let storage = kitty.join("graphics_storage.zig");
    let mut text = fs::read_to_string(&storage).expect("read pinned Ghostty animation tick");
    patch_storage(&mut text, &storage);
    insert_once(
        &kitty.join("graphics_animation.zig"),
        "    frame_shown_at_ms: ?u64 = null,\n",
        "\n    /// Loading exhausted the available frames. Resume on the arrival clock.\n    waiting_for_frames: bool = false,\n",
    );
    replace_once(
        &terminal.join("c/kitty_graphics.zig"),
        &[("pub fn image_get_handle(\n", C_BRIDGE)],
    );
    insert_once(
        &source.join("src/lib_vt.zig"),
        "        @export(&c.kitty_graphics_animation_tick, .{ .name = \"ghostty_kitty_graphics_animation_tick\" });\n",
        "        @export(&@import(\"terminal/c/kitty_graphics.zig\").sumie_animation_tick_visible, .{ .name = \"sumie_kitty_graphics_animation_tick_visible\" });\n",
    );
}

fn patch_storage(text: &mut String, storage: &Path) {
    if !text.contains(STORAGE_TICK) {
        let start =
            "    pub fn animationTick(self: *ImageStorage, io: std.Io, now_ms: u64) ?u64 {\n";
        let end = "    /// Clear placements intersecting the active screen, then reclaim every\n";
        let (before, tail) = text
            .split_once(start)
            .expect("find pinned Ghostty tick start");
        let (old, _) = tail.split_once(end).expect("find pinned Ghostty tick end");
        let at = before.len();
        let len = start.len() + old.len();
        text.replace_range(at..at + len, STORAGE_TICK);
        fs::write(storage, text).expect("patch pinned Ghostty animation tick");
    }
}

const C_BRIDGE: &str = r"pub fn sumie_animation_tick_visible(
    graphics_: ?KittyGraphics,
    now_ms: u64,
    image_ids: [*]const u32,
    image_count: usize,
    next_ms: *u64,
    changed: *bool,
) callconv(lib.calling_conv) Result {
    if (comptime !lib.feature(.kitty_graphics)) return .unsupported;
    const storage = graphics_ orelse return .invalid_value;
    const before = storage.generation;
    next_ms.* = storage.animationTickVisible(terminal_c.Io.init.io(), now_ms, image_ids[0..image_count]) orelse 0;
    changed.* = before != storage.generation;
    return .success;
}

pub fn image_get_handle(
";

const STORAGE_TICK: &str = r"    pub fn animationTick(self: *ImageStorage, io: std.Io, now_ms: u64) ?u64 {
        if (self.animation_idle_generation == self.generation) return null;

        var earliest: u64 = 0;
        var it = self.images.iterator();
        while (it.next()) |entry| {
            const at = self.tickImage(io, entry.value_ptr, now_ms) orelse continue;
            earliest = if (earliest == 0) at else @min(earliest, at);
        }
        self.animation_idle_generation = if (earliest == 0) self.generation else null;
        return if (earliest == 0) null else earliest - now_ms;
    }

    /// Tick only the unique IDs visible to the caller, with no storage-wide walk.
    /// Return the first absolute deadline, if any.
    pub fn animationTickVisible(self: *ImageStorage, io: std.Io, now_ms: u64, ids: []const u32) ?u64 {
        var earliest: u64 = 0;
        for (ids) |id| {
            const img = self.images.getPtr(id) orelse continue;
            const at = self.tickImage(io, img, now_ms) orelse continue;
            earliest = if (earliest == 0) at else @min(earliest, at);
        }
        return if (earliest == 0) null else earliest;
    }

    fn tickImage(self: *ImageStorage, io: std.Io, img: *Image, now_ms: u64) ?u64 {
        const anim = img.animation orelse return null;
        if (anim.state == .stopped or anim.frames.items.len == 0 or img.metadata.placement_count == 0) return null;
        if (img.data.isPending() or anim.hasPendingFrames()) return null;
        if (anim.max_loops > 0 and anim.current_loop >= anim.max_loops) return null;
        const duration = anim.durationMs();
        if (duration == 0) return null;

        const initial = anim.current_index;
        const prior = anim.frame_shown_at_ms;
        const shown_at = @min(prior orelse now_ms, now_ms);
        anim.frame_shown_at_ms = shown_at;
        // An overdue loading animation may have waited for a frame that only just arrived.
        const waiting = anim.waiting_for_frames and prior != null;
        anim.waiting_for_frames = false;
        var due = if (waiting) now_ms else shown_at +| anim.gapAt(initial);
        if (due > now_ms) return due;

        if (anim.state == .running) {
            // Skip whole cycles first: a long suspension must not mean a long tick.
            // Leave the terminating wrap to the walk below, so it can show the last frame.
            var cycles = (now_ms - due) / duration;
            if (anim.max_loops != 0) cycles = @min(cycles, anim.max_loops - anim.current_loop - 1);
            due += cycles * duration;
            anim.current_loop +|= @intCast(@min(cycles, std.math.maxInt(u32)));
        }

        var scan = initial;
        const count = anim.frameCount();
        while (true) {
            scan = (scan + 1) % count;
            if (scan == 0) {
                if (anim.state == .loading) {
                    anim.waiting_for_frames = true;
                    break;
                }
                anim.current_loop +|= 1;
                if (anim.max_loops > 0 and anim.current_loop >= anim.max_loops) break;
            }
            const gap = anim.gapAt(scan);
            if (gap == 0) continue;
            anim.current_index = scan;
            anim.frame_shown_at_ms = due;
            if (gap > now_ms - due) {
                due +|= gap;
                break;
            }
            due += gap;
        }
        if (anim.current_index != initial) self.markImageContentChanged(io, img);
        return if (due > now_ms) due else null;
    }

";
