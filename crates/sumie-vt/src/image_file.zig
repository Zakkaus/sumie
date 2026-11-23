//! Linux file transport for the pinned Kitty loader. The stream controls `path`.
const std = @import("std");
const Io = std.Io;

pub fn open(io: Io, path: []const u8, temporary: bool, offset: u64, size: u64) !Io.File {
    if (comptime @import("builtin").os.tag != .linux) return error.UnsupportedMedium;
    const root = for ([_][]const u8{ "/tmp", "/dev/shm" }) |dir| {
        if (path.len > dir.len and std.mem.startsWith(u8, path, dir) and path[dir.len] == '/') break dir;
    } else return error.InvalidData;
    // Ghostty's TinyIo supports O_PATH handles but not dirOpenDir or dirStatFile.
    var parent = try pathHandle(io, .cwd(), root);
    defer parent.close(io);
    var parts = std.mem.splitScalar(u8, path[root.len + 1 ..], '/');
    var name = parts.next() orelse return error.InvalidData;
    while (true) {
        if (name.len == 0 or std.mem.eql(u8, name, ".") or std.mem.eql(u8, name, "..")) return error.InvalidData;
        const next = parts.next() orelse break;
        const child = try pathHandle(io, .{ .handle = parent.handle }, name);
        parent.close(io);
        parent = child;
        name = next;
    }
    if (temporary and std.mem.indexOf(u8, name, "tty-graphics-protocol") == null) return error.InvalidData;

    // O_PATH lets us reject devices and FIFOs without opening them for I/O.
    const dir: Io.Dir = .{ .handle = parent.handle };
    const entry = try pathHandle(io, dir, name);
    defer entry.close(io);
    const stat = entry.stat(io) catch return error.InvalidData;
    if (stat.kind != .file or stat.nlink != 1 or stat.size > 320 * 1024 * 1024 or
        offset > stat.size or size > stat.size - offset) return error.InvalidData;
    var buf: [64]u8 = undefined;
    const proc_path = std.fmt.bufPrint(&buf, "/proc/self/fd/{d}", .{entry.handle}) catch unreachable;
    const file = Io.Dir.openFileAbsolute(io, proc_path, .{}) catch return error.InvalidData;
    errdefer file.close(io);
    if (temporary) {
        // Unlink relative to the held parent, never by resolving the original path again.
        const named = try pathHandle(io, dir, name);
        defer named.close(io);
        const current = named.stat(io) catch return error.InvalidData;
        if (current.inode != stat.inode or current.kind != .file) return error.InvalidData;
        dir.deleteFile(io, name) catch return error.InvalidData;
    }
    return file;
}

fn pathHandle(io: Io, dir: Io.Dir, name: []const u8) !Io.File {
    return dir.openFile(io, name, .{ .path_only = true, .follow_symlinks = false }) catch error.InvalidData;
}
