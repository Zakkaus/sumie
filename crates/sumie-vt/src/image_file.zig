//! Linux file transport for the pinned Kitty loader. The stream controls `path`.
const std = @import("std");
const builtin = @import("builtin");
const Io = std.Io;
const linux = std.os.linux;

pub fn read(io: Io, alloc: std.mem.Allocator, path: []const u8, temporary: bool, offset: u64, size: u64) !std.ArrayList(u8) {
    if (comptime builtin.os.tag != .linux) return error.UnsupportedMedium;
    const root = for ([_][:0]const u8{ "/tmp", "/dev/shm" }) |dir| {
        if (path.len > dir.len and std.mem.startsWith(u8, path, dir) and path[dir.len] == '/') break dir;
    } else return error.InvalidData;
    // Ghostty's TinyIo does not support dirOpenDir or dirStatFile.
    var parent = try pathHandle(linux.AT.FDCWD, root, true);
    defer _ = linux.close(parent);
    var parts = std.mem.splitScalar(u8, path[root.len + 1 ..], '/');
    var name = parts.next() orelse return error.InvalidData;
    var name_buf: [linux.NAME_MAX + 1]u8 = undefined;
    const entry_name = while (true) {
        if (name.len == 0 or name.len > linux.NAME_MAX or std.mem.eql(u8, name, ".") or
            std.mem.eql(u8, name, "..")) return error.InvalidData;
        @memcpy(name_buf[0..name.len], name);
        name_buf[name.len] = 0;
        const name_z = name_buf[0..name.len :0];
        const next = parts.next() orelse break name_z;
        const child = try pathHandle(parent, name_z, true);
        _ = linux.close(parent);
        parent = child;
        name = next;
    };

    // Validate the held inode without opening a device, FIFO, or non-memory file for I/O.
    const entry = try pathHandle(parent, entry_name, false);
    defer _ = linux.close(entry);
    if (!memoryFilesystem(entry)) return error.InvalidData;
    var stat: linux.Statx = undefined;
    const mask: linux.STATX = .{ .TYPE = true, .NLINK = true, .INO = true, .SIZE = true };
    if (linux.errno(linux.statx(entry, "", linux.AT.EMPTY_PATH, mask, &stat)) != .SUCCESS or
        !stat.mask.TYPE or !stat.mask.NLINK or !stat.mask.INO or !stat.mask.SIZE or
        !linux.S.ISREG(stat.mode) or stat.nlink != 1 or stat.size > 320 * 1024 * 1024 or
        offset > stat.size or size > stat.size - offset) return error.InvalidData;
    var buf: [64]u8 = undefined;
    const proc_path = std.fmt.bufPrint(&buf, "/proc/self/fd/{d}", .{entry}) catch unreachable;
    const file = Io.Dir.openFileAbsolute(io, proc_path, .{}) catch return error.InvalidData;
    defer file.close(io);
    if (temporary and std.mem.indexOf(u8, path, "tty-graphics-protocol") != null) {
        var current: linux.Statx = undefined;
        if (linux.errno(linux.statx(parent, entry_name, linux.AT.SYMLINK_NOFOLLOW, .{ .INO = true }, &current)) == .SUCCESS and
            current.mask.INO and current.ino == stat.ino and
            current.dev_major == stat.dev_major and current.dev_minor == stat.dev_minor)
        {
            // Linux cannot unlink by inode. A rename can still race this last check.
            if (linux.errno(linux.unlinkat(parent, entry_name, 0)) != .SUCCESS) return error.InvalidData;
        }
    }

    // An explicit S is exact; S=0 reads up to the validated end, not a growing EOF.
    const limit: usize = @intCast(if (size == 0) stat.size - offset else size);
    var data = try std.ArrayList(u8).initCapacity(alloc, limit);
    errdefer data.deinit(alloc);
    const dest = data.allocatedSlice()[0..limit];
    const len = file.readPositionalAll(io, dest, offset) catch return error.InvalidData;
    if (size != 0 and len != limit) return error.InvalidData;
    data.items = dest[0..len];
    return data;
}

fn pathHandle(parent: linux.fd_t, name: [*:0]const u8, directory: bool) !linux.fd_t {
    const rc = linux.openat(parent, name, .{ .PATH = true, .DIRECTORY = directory, .NOFOLLOW = true, .CLOEXEC = true }, 0);
    if (linux.errno(rc) != .SUCCESS) return error.InvalidData;
    return @intCast(rc);
}

// Linux libc places f_type first; the backing storage covers the largest ABI layout.
const FsStat = extern union {
    magic: switch (builtin.cpu.arch) {
        .s390x => c_uint,
        else => if (builtin.abi == .gnux32) c_ulonglong else c_ulong,
    },
    storage: [16]u64,
};

extern "c" fn fstatfs(fd: linux.fd_t, stat: *FsStat) c_int;

fn memoryFilesystem(fd: linux.fd_t) bool {
    var stat: FsStat = undefined;
    if (fstatfs(fd, &stat) != 0) return false;
    // devtmpfs uses the tmpfs or ramfs superblock, depending on CONFIG_TMPFS.
    return switch (stat.magic) {
        0x01021994, // TMPFS_MAGIC
        0x858458f6, // RAMFS_MAGIC
        => true,
        else => false,
    };
}
