use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "build/clock.rs"]
mod clock_patch;
#[path = "build/keys.rs"]
mod keys_patch;
#[path = "build/kitty.rs"]
mod kitty_patch;

const GHOSTTY_COMMIT: &str = "108cacb17ed8990840cd14abfce43d8ed8503ca1";
const GHOSTTY_ARCHIVE_SHA256: &str =
    "2c276d3e62cb15fb38f0d7f621f199029d1d4df86538096c6bec2a107cad7159";
// Bump when changing patches already applied to a cached extraction.
const GHOSTTY_PATCH_LEVEL: u32 = 5;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=build");
    println!("cargo::rerun-if-changed=src/search/grid.zig");
    println!("cargo::rerun-if-changed=src/selection.zig");
    println!("cargo::rerun-if-changed=src/image_file.zig");
    println!("cargo::rerun-if-changed=src/screen.zig");
    for var in [
        "ZIG",
        "PATH",
        "SUMIE_GHOSTTY_ARCHIVE",
        "GHOSTTY_ZIG_SYSTEM_DIR",
        "ZIG_GLOBAL_CACHE_DIR",
    ] {
        println!("cargo::rerun-if-env-changed={var}");
    }

    assert_eq!(
        env::var("HOST"),
        env::var("TARGET"),
        "sumie-vt requires a native build"
    );
    let zig = zig();
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let source = source(&out);
    resolve_cursor_default_at_snapshot(&source);
    install_search_bridge(&source);
    install_selection_bridge(&source);
    install_screen_bridge(&source);
    kitty_patch::install(&source);
    preserve_chunked_animation_frames(&source);
    install_thumbnail_policy(&source);
    expose_thumbnail_flag(&source.join("src/terminal"));
    install_placement_lifetime(&source);
    install_screen_clear_epoch(&source);
    clock_patch::install(&source);
    keys_patch::install_plain_enter(&source);
    keys_patch::install_control_bytes(&source);
    keys_patch::install_other_keys_without_text(&source);
    let prefix = out.join("ghostty-install");
    let mut build = Command::new(zig);
    build
        .current_dir(source)
        .args([
            "build",
            "-Demit-lib-vt=true",
            "-Demit-xcframework=false",
            "-Dapp-runtime=none",
            "-Dlib-version-string=1.0.1",
            "-Doptimize=ReleaseSafe",
            "-Dcpu=baseline",
        ])
        .arg("--prefix")
        .arg(&prefix)
        .arg("--cache-dir")
        .arg(out.join("zig-cache"));
    if let Some(dir) = env::var_os("GHOSTTY_ZIG_SYSTEM_DIR") {
        build
            .arg("--system")
            .arg(std::path::absolute(dir).expect("resolve GHOSTTY_ZIG_SYSTEM_DIR"));
    }
    run(&mut build, "build libghostty-vt with Zig");
    // Darwin passes -lNAME even for a Rust static link. Give the archive a
    // distinct name so ld cannot prefer Ghostty's public-ABI-only dylib.
    let lib = prefix.join("lib");
    fs::rename(lib.join("libghostty-vt.a"), lib.join("libsumie-vt.a"))
        .expect("name the static archive for the Rust linker");
    println!("cargo::warning=Built libghostty-vt vt-1.0.1 ({GHOSTTY_COMMIT})");
    println!("cargo::rustc-link-search=native={}", lib.display());
    println!("cargo::rustc-link-lib=static=sumie-vt");
}

fn resolve_cursor_default_at_snapshot(source: &Path) {
    // The pinned Ghostty stores each screen's last shape, but default-following
    // is terminal-wide. Resolve the default without changing the shared blink mode.
    const VISUAL_STYLE: &str = "self.cursor.visual_style = s.cursor.cursor_style;";
    const FIXED: &str = "self.cursor.visual_style = if (t.cursor.is_default)\n            \
        t.cursor.default_style\n        else\n            s.cursor.cursor_style;";
    let path = source.join("src/terminal/render.zig");
    let text = fs::read_to_string(&path).expect("read pinned Ghostty render.zig");
    if !text.contains(FIXED) {
        assert_eq!(
            text.matches(VISUAL_STYLE).count(),
            1,
            "Ghostty patch anchor"
        );
        fs::write(path, text.replacen(VISUAL_STYLE, FIXED, 1)).expect("patch Ghostty cursor shape");
    }
}

fn install_search_bridge(source: &Path) {
    let bridge = source.join("src/sumie_search.zig");
    fs::write(bridge, include_str!("src/search/grid.zig")).expect("write Zig search bridge");
    insert_once(
        &source.join("src/lib_vt.zig"),
        "const terminal = @import(\"terminal/main.zig\");\n",
        "comptime { @import(\"sumie_search.zig\").install(); }\n",
    );
    insert_once(
        &source.join("src/build/uucode_config.zig"),
        "            \"case_folding_full\",\n",
        "            \"case_folding_simple\",\n",
    );
}

fn install_selection_bridge(source: &Path) {
    fs::write(
        source.join("src/sumie_selection.zig"),
        include_str!("src/selection.zig"),
    )
    .expect("write Zig selection bridge");
    insert_once(
        &source.join("src/lib_vt.zig"),
        "comptime { @import(\"sumie_search.zig\").install(); }\n",
        "comptime { @import(\"sumie_selection.zig\").install(); }\n",
    );
    let build = source.join("src/build/GhosttyZig.zig");
    insert_once(
        &build,
        "    vt_options.c_abi = true;\n",
        "    vt_options.oniguruma = true;\n",
    );
    insert_once(
        &build,
        "    vt.addImport(\"uucode\", deps.uucode_mod);\n",
        r#"
    if (vt_options.oniguruma) {
        if (b.lazyDependency("oniguruma", .{ .target = cfg.target, .optimize = cfg.optimize })) |dep| {
            vt.addImport("oniguruma", dep.module("oniguruma"));
            if (simd_libs) |libs| try libs.append(b.allocator, dep.artifact("oniguruma").getEmittedBin());
        }
    }
"#,
    );
}

fn install_screen_bridge(source: &Path) {
    let bridge = source.join("src/sumie_screen.zig");
    fs::write(bridge, include_str!("src/screen.zig")).expect("write Zig screen bridge");
    insert_once(
        &source.join("src/lib_vt.zig"),
        "comptime { @import(\"sumie_selection.zig\").install(); }\n",
        "comptime { @import(\"sumie_screen.zig\").install(); }\n",
    );
}

fn install_thumbnail_policy(source: &Path) {
    let terminal = source.join("src/terminal");
    insert_once(
        &terminal.join("Terminal.zig"),
        "height_px: u32 = 0,\n",
        "\n/// Row cap for new natural Kitty placements, or zero to leave them unchanged.\nsumie_thumbnail_rows: u16 = 0,\n",
    );
    insert_once(
        &terminal.join("c/terminal.zig"),
        "pub const Terminal = ?*TerminalWrapper;\n",
        r"
pub fn sumie_set_thumbnail_rows(terminal: Terminal, rows: u16) callconv(lib.calling_conv) void {
    const wrapper = terminal orelse return;
    wrapper.terminal.sumie_thumbnail_rows = rows;
}
",
    );
    insert_once(
        &source.join("src/lib_vt.zig"),
        "        @export(&c.terminal_set, .{ .name = \"ghostty_terminal_set\" });\n",
        "        @export(&@import(\"terminal/c/terminal.zig\").sumie_set_thumbnail_rows, .{ .name = \"sumie_set_thumbnail_rows\" });\n",
    );
    cap_new_placements(&terminal.join("kitty/graphics_exec.zig"));
    insert_once(
        &terminal.join("kitty/graphics_storage.zig"),
        "        z: i32 = 0,\n",
        "\n        sumie_thumbnail: bool = false,\n",
    );
    insert_once(
        &terminal.join("kitty/graphics_storage.zig"),
        "                const calc_width = scaleDimension(calc_height, width, height);\n",
        r"
                // A cell-size increase must not upscale a thumbnail beyond its source.
                if (self.sumie_thumbnail and width > 0 and height > 0 and calc_height > 0) {
                    return .{
                        .width = @max(1, @min(width, calc_width)),
                        .height = @min(height, calc_height),
                    };
                }
",
    );
    insert_once(
        &terminal.join("render.zig"),
        "    pub const Placement = struct {\n        kind: Kind,\n",
        "        sumie_thumbnail: bool = false,\n",
    );
}

fn cap_new_placements(path: &Path) {
    // Replace the existing policy when Cargo reuses a previously patched extraction.
    let mut text = fs::read_to_string(path).expect("read pinned placement code");
    let anchor =
        "        if (terminal.height_px / terminal.rows > 0) p.y_offset = cell_offset.y;\n";
    let start = text.find(anchor).expect("find placement offset clamp") + anchor.len();
    let end = start
        + text[start..]
            .find("\n        break :placement p;")
            .expect("find placement end");
    let policy = r"
        // Change the footprint only for inline output that advances the cursor.
        const rows = terminal.sumie_thumbnail_rows;
        const cell_height = terminal.height_px / terminal.rows;
        const animated = if (img.animation) |anim| anim.frameCount() > 1 else false;
        if (terminal.screens.active_key == .primary and location == .pin and
            d.cursor_movement == .after and rows > 0 and cell_height > 0 and
            !animated and p.columns == 0 and p.rows == 0)
        {
            const source = p.sourceRect(img);
            // cellOffset clamps Y to cell_height - 1, even for oversized requests.
            const max_height = (cell_height *| @as(u32, rows)) -| cell_offset.y;
            if (source.width > 0 and source.height > max_height) {
                p.rows = rows;
                p.sumie_thumbnail = true;
            }
        }
";
    if text[start..end] != *policy {
        text.replace_range(start..end, policy);
        fs::write(path, text).expect("patch pinned placement code");
    }
}

fn expose_thumbnail_flag(terminal: &Path) {
    insert_once(
        &terminal.join("render.zig"),
        "                .kind = kind,\n",
        "                .sumie_thumbnail = p.sumie_thumbnail,\n",
    );
    let render = terminal.join("c/render.zig");
    insert_once(
        &render,
        "    kitty_kind = 19,\n",
        "    sumie_thumbnail = 20,\n",
    );
    insert_once(
        &render,
        "            .kitty_kind => KittyPlacementKind,\n",
        "            .sumie_thumbnail => bool,\n",
    );
    insert_once(
        &render,
        "            .kitty => |kitty| out.* = kitty.kind,\n        },\n",
        "        .sumie_thumbnail => switch (p.protocol) {\n            .kitty => |kitty| out.* = kitty.sumie_thumbnail,\n        },\n",
    );
}

fn install_placement_lifetime(source: &Path) {
    let terminal = source.join("src/terminal");
    let storage = terminal.join("kitty/graphics_storage.zig");
    insert_once(
        &storage,
        "    pub const Placement = struct {\n",
        "        sumie_revision: u64 = 0,\n\n",
    );
    insert_once(
        &storage,
        "        gop.value_ptr.* = p;\n\n        // This always mutates\n        self.markMutated(io);\n",
        "        gop.value_ptr.sumie_revision = self.generation;\n",
    );
    expose_placement_key(&terminal);
    insert_once(
        &terminal.join("c/terminal.zig"),
        "    wrapper.terminal.sumie_thumbnail_rows = rows;\n}\n",
        r"
pub fn sumie_placement_image_generation(
    terminal: Terminal,
    image_id: u32,
    storage_id: u64,
    revision: u64,
    transmission: u64,
) callconv(lib.calling_conv) u64 {
    if (comptime !build_options.kitty_graphics) return 0;
    const wrapper = terminal orelse return 0;
    const storage = &wrapper.terminal.screens.active.kitty_images;
    const image = storage.images.getPtr(image_id) orelse return 0;
    if (image.transmission != transmission or revision == 0) return 0;

    const Storage = @TypeOf(storage.*);
    const key: Storage.PlacementKey = .{
        .image_id = image_id,
        .placement_id = @bitCast(std.math.cast(u33, storage_id) orelse return 0),
    };
    const placement = storage.placements.getPtr(key) orelse return 0;
    if (placement.sumie_revision != revision) return 0;

    const live = switch (placement.location) {
        .pin => |pin| !pin.garbage,
        .virtual => true,
        .relative => |rel| if (storage.resolveChain(rel)) |chain| switch (chain.root.location) {
            .pin => |pin| !pin.garbage,
            .virtual => true,
            .relative => unreachable,
        } else false,
    };
    return if (live) image.generation else 0;
}

pub fn sumie_screen_clear_epoch(terminal: Terminal) callconv(lib.calling_conv) u64 {
    const wrapper = terminal orelse return 0;
    return wrapper.terminal.sumie_screen_clear_epoch;
}
",
    );
    insert_once(
        &source.join("src/lib_vt.zig"),
        "        @export(&@import(\"terminal/c/terminal.zig\").sumie_set_thumbnail_rows, .{ .name = \"sumie_set_thumbnail_rows\" });\n",
        concat!(
            "        @export(&@import(\"terminal/c/terminal.zig\").sumie_placement_image_generation, .{ .name = \"sumie_placement_image_generation\" });\n",
            "        @export(&@import(\"terminal/c/terminal.zig\").sumie_screen_clear_epoch, .{ .name = \"sumie_screen_clear_epoch\" });\n",
        ),
    );
}

fn expose_placement_key(terminal: &Path) {
    let render = terminal.join("render.zig");
    insert_once(
        &render,
        "        sumie_thumbnail: bool = false,\n",
        "        sumie_storage_key: [3]u64 = .{ 0, 0, 0 },\n",
    );
    insert_once(
        &render,
        "                .sumie_thumbnail = p.sumie_thumbnail,\n",
        "                .sumie_storage_key = .{ @as(u33, @bitCast(key.placement_id)), p.sumie_revision, image.transmission },\n",
    );
    insert_once(
        &render,
        "                            .kind = .virtual,\n",
        "                            .sumie_storage_key = g.sumie_storage_key,\n",
    );
    insert_once(
        &render,
        "        pub const Geometry = struct {\n            image_generation: u64,\n",
        "            sumie_storage_key: [3]u64 = .{ 0, 0, 0 },\n",
    );
    insert_once(
        &render,
        "        run.geometry = .{\n",
        "            .sumie_storage_key = .{ @as(u33, @bitCast(target.key.placement_id)), target.placement.sumie_revision, image.transmission },\n",
    );
    let c_render = terminal.join("c/render.zig");
    insert_once(
        &c_render,
        "    sumie_thumbnail = 20,\n",
        "    sumie_storage_key = 21,\n",
    );
    insert_once(
        &c_render,
        "            .sumie_thumbnail => bool,\n",
        "            .sumie_storage_key => [3]u64,\n",
    );
    insert_once(
        &c_render,
        "            .kitty => |kitty| out.* = kitty.sumie_thumbnail,\n        },\n",
        "        .sumie_storage_key => switch (p.protocol) {\n            .kitty => |kitty| out.* = kitty.sumie_storage_key,\n        },\n",
    );
}

fn install_screen_clear_epoch(source: &Path) {
    let terminal = source.join("src/terminal/Terminal.zig");
    insert_once(
        &terminal,
        "sumie_thumbnail_rows: u16 = 0,\n",
        "sumie_screen_clear_epoch: u64 = 0,\n",
    );
    insert_once(
        &terminal,
        "            // All active area\n",
        "            self.sumie_screen_clear_epoch +%= 1;\n",
    );
    insert_once(
        &terminal,
        "                self.eraseDisplay(.complete, protected_req);\n                return;\n            };\n",
        "            self.sumie_screen_clear_epoch +%= 1;\n",
    );
    insert_once(
        &terminal,
        "pub fn fullReset(self: *Terminal) void {\n",
        "    self.sumie_screen_clear_epoch +%= 1;\n",
    );
}

fn preserve_chunked_animation_frames(source: &Path) {
    let kitty = source.join("src/terminal/kitty");
    // Playback can update the pixel generation between reads of the same transmission.
    replace_once(
        &kitty.join("graphics_image.zig"),
        &[(
            concat!(
                "        /// The generation of the target image when the load began.\n",
                "        /// A different generation at completion means the image was\n",
                "        /// replaced or evicted mid-transmission and the frame must be\n",
                "        /// discarded rather than composed onto the wrong image.\n",
                "        image_generation: u64,",
            ),
            concat!(
                "        /// The target image's transmission stamp survives playback but\n",
                "        /// changes when the image is replaced.\n",
                "        image_transmission: u64,",
            ),
        )],
    );
    replace_once(
        &kitty.join("graphics_exec.zig"),
        &[
            (
                ".image_generation = img.generation,",
                ".image_transmission = img.transmission,",
            ),
            (
                "// generation pins the exact image the load started against.",
                "// transmission stamp pins the image the load started against.",
            ),
            (
                "img.generation != loading.frame.?.image_generation",
                "img.transmission != loading.frame.?.image_transmission",
            ),
        ],
    );
    replace_once(
        &source.join("src/terminal/snapshot/kitty.zig"),
        &[(
            concat!(
                "    // is identified by its generation. Stamps are process-local, so use\n",
                "    // the restored image's fresh one.\n",
                "    if (frame) |f| loading.frame = .{\n",
                "        .cmd = f,\n",
                "        .image_generation = if (storage.imagePtrByIdOrNumber(id, number)) |img|\n",
                "            img.generation\n",
            ),
            concat!(
                "    // is identified by its transmission stamp. Stamps are process-local,\n",
                "    // so use the restored image's fresh one.\n",
                "    if (frame) |f| loading.frame = .{\n",
                "        .cmd = f,\n",
                "        .image_transmission = if (storage.imagePtrByIdOrNumber(id, number)) |img|\n",
                "            img.transmission\n",
            ),
        )],
    );
}

fn replace_once(path: &Path, edits: &[(&str, &str)]) {
    let mut text = fs::read_to_string(path).expect("read pinned Ghostty source");
    let mut changed = false;
    for &(old, new) in edits {
        if !text.contains(new) {
            let at = text.find(old).expect("find pinned Ghostty patch anchor");
            text.replace_range(at..at + old.len(), new);
            changed = true;
        }
    }
    if changed {
        fs::write(path, text).expect("patch pinned Ghostty source");
    }
}

fn insert_once(path: &Path, anchor: &str, addition: &str) {
    let mut text = fs::read_to_string(path).expect("read pinned Ghostty source");
    let at = text.find(anchor).expect("find pinned Ghostty patch anchor") + anchor.len();
    if !text[at..].starts_with(addition) {
        text.insert_str(at, addition);
        fs::write(path, text).expect("patch pinned Ghostty source");
    }
}

fn zig() -> PathBuf {
    let zig = PathBuf::from(env::var_os("ZIG").unwrap_or_else(|| "zig".into()));
    let zig = if zig.is_relative() && zig.parent().is_some_and(|path| path != Path::new("")) {
        std::path::absolute(zig).expect("resolve ZIG")
    } else {
        zig
    };
    let output = Command::new(&zig)
        .arg("version")
        .output()
        .unwrap_or_else(|err| {
            panic!(
                "sumie-vt requires Zig 0.17; install it on PATH or set ZIG: {}: {err}",
                zig.display()
            )
        });
    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && version.trim().starts_with("0.17."),
        "sumie-vt requires Zig 0.17; {} version returned {}: {version}{}",
        zig.display(),
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    zig
}

fn source(out: &Path) -> PathBuf {
    let source = out.join(format!("ghostty-{GHOSTTY_COMMIT}-{GHOSTTY_PATCH_LEVEL}"));
    if source.is_dir() {
        return source;
    }

    let download = out.join("ghostty.tar.gz");
    let archive = if let Some(path) = env::var_os("SUMIE_GHOSTTY_ARCHIVE") {
        PathBuf::from(path)
    } else {
        run(
            Command::new("curl")
                .args(["-fsSL", "--output"])
                .arg(&download)
                .arg(format!(
                    "https://codeload.github.com/ghostty-org/ghostty/tar.gz/{GHOSTTY_COMMIT}"
                )),
            "download the pinned Ghostty source",
        );
        download.clone()
    };
    let digest = Command::new("shasum")
        .args(["-a", "256"])
        .arg(&archive)
        .output()
        .expect("run shasum to verify the pinned Ghostty source");
    assert!(
        digest.status.success()
            && String::from_utf8_lossy(&digest.stdout)
                .split_whitespace()
                .next()
                == Some(GHOSTTY_ARCHIVE_SHA256),
        "Ghostty source checksum mismatch for {}: {}",
        archive.display(),
        String::from_utf8_lossy(&digest.stderr),
    );

    let staging = out.join("ghostty-unpack");
    if staging.exists() {
        fs::remove_dir_all(&staging).expect("remove incomplete Ghostty extraction");
    }
    fs::create_dir(&staging).expect("create Ghostty extraction directory");
    run(
        Command::new("tar")
            .args(["-xzf"])
            .arg(&archive)
            .arg("--strip-components=1")
            .arg("-C")
            .arg(&staging),
        "extract the pinned Ghostty source",
    );
    fs::rename(staging, &source).expect("install verified Ghostty source");
    if archive == download {
        fs::remove_file(download).expect("remove extracted Ghostty archive");
    }
    source
}

fn run(command: &mut Command, context: &str) {
    let status = command
        .status()
        .unwrap_or_else(|err| panic!("{context}: {err}"));
    assert!(status.success(), "{context}: {status}");
}
