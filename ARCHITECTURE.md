# Architecture

The [design book](https://claude.ai/artifact/A3KR8SWWJouwinJsSZUje8) defines the crate boundaries. Before adding a crate or module, document what it owns and what it must not do. Define interfaces at their call sites and record constraints on thread, blocking, and call order when those interfaces exist.

## Crates and allowed dependencies

Edges in the last column go from the row's crate to the listed crates. They are permissions, not dependencies to declare before use. The P3 packages are listed for reference and are not workspace members.

| Crate | Owns | Must not do | May depend on |
| --- | --- | --- | --- |
| `sumie` (binary) | Composition, CLI parsing, platform traits, config-to-core option conversion, passing font atlases to the renderer | Business logic | All library crates |
| `sumie-core` | Tabs, panes, selections, link and shortcut matching, commands, agent state, plain-data options | Platform code, config, GPU, FFI, unsafe code, blocking I/O | None |
| `sumie-config` | Settings types, loading, validation, hot reload, writing TOML, JSON Schema generation | Manage windows or render | None |
| `sumie-vt` | Safe libghostty-vt wrapper | Render, call platform code, match shortcuts | `sumie-core`, `sumie-config` |
| `sumie-font` | Font discovery and fallback, shaping, rasterization, glyph atlas data | Issue GPU calls or read or change terminal state | `sumie-core`, `sumie-config` |
| `sumie-render` | wgpu rendering from atlas data and draw lists | Discover fonts, depend on `sumie-font`, change terminal state, allocate GPU objects per frame, submit GPU work while holding a terminal lock | `sumie-core`, `sumie-config` |
| `sumie-ui` | Drawn tab bar, sidebar, command palette, menus, hints | Make business decisions or call Wayland or AppKit directly | `sumie-core`, `sumie-config`, `sumie-font`, `sumie-render` |
| `sumie-wayland` (Linux) | Wayland connection, windows, input, input methods, clipboard, fractional scaling | Match shortcuts, make business decisions, block the main loop on clipboard reads | `sumie-core`, `sumie-config` |
| `sumie-macos` (macOS) | Windows, input, input methods, clipboard | Match shortcuts, make business decisions, block the main loop on clipboard reads | `sumie-core`, `sumie-config` |
| `sumie-pty` | Shared Unix PTY, child processes, reader threads | Touch window objects or render | `sumie-core`, `sumie-config` |
| `sumie-ipc` (P3) | Socket protocol and client shared by the CLI and settings TUI | Make business decisions or render | `sumie-core`, `sumie-config` |
| `sumie-settings` (P3) | Settings TUI | Parse or write TOML itself or access GUI state directly | `sumie-config`, `sumie-ipc` |

Two edges are deliberately closed:

- `sumie-render` → `sumie-font`: the font crate produces CPU-side glyph atlas data. The binary passes that data to the renderer, which accepts only atlas data and draw lists.
- `sumie-core` → `sumie-config`: core defines plain-data options. The binary translates parsed TOML settings into those options, so headless core tests and the future daemon do not require TOML.

## Single-pane Linux application

`main` parses the capture options and selects the platform. `app` drives Wayland and the PTY on one calloop thread. `pane` owns the local terminal, key encoder and PTY. The consumer calls `Pane::command` and draws from its own `Pane::snapshot` result; neither method waits for PTY I/O. `view` composes the owned snapshot and IME preedit into device-pixel draw lists and hands font atlas data to the renderer. `capture` handles opt-in frame readback and PNG output; normal presentation does not read back pixels.

PTY readiness clears the wake before draining output into VT. Key commands carry unencoded input; the local pane encodes them with the terminal's current modes. IME preedit only changes the overlay; only commit bytes cross the pane boundary. Input writes remain ordered in the PTY FIFO. Wayland configures the first frame slot. After that, output, geometry or preedit can use a free slot, but a frame callback alone never dirties the view. Close drops GPU state before the Wayland window and drives child reaping from pidfd readiness and the PTY close deadline.

On Linux, core matches Ctrl+Shift+V by evdev position or keysym before key encoding. The Wayland `clipboard` module reads the keyboard seat's selection on a nonblocking calloop pipe. It neither matches shortcuts nor writes to the PTY. At EOF, the app passes `PaneCommand::Paste` to the pane. The VT wrapper reads mode 2004, removes embedded end markers in bracketed mode, and calls `ghostty_paste_encode`. The pane enqueues the encoded bytes in one operation, including both markers. An oversized transfer is discarded; warnings report byte counts, never clipboard text.

If surface acquisition times out, reports occlusion, or needs reconfiguration, the app retains the dirty frame and retries on a deadline. An idle or suspended surface has no retry deadline. Runtime font errors leave affected cells blank if no replacement glyph can be drawn; the view logs each error variant once without logging the input text.

For screenshots, `--exit-after-frames N --screenshot PATH` captures the Nth actual presentation. Use changing terminal output to reach N: the debug path does not generate frames on an idle terminal.

`gpu` creates the wgpu surface with safe `Instance::create_surface` and passes it to `SurfaceRenderer`. wgpu retains the shared `WindowOwner`; the renderer's GPU resources drop before the surface, and the native window cannot drop while the surface still owns it.

## Terminal core (`sumie-vt`)

`Terminal` owns one libghostty-vt terminal, one render state, and their row and cell iterators. Its handles are `!Send` and `!Sync`: this API keeps the terminal on its owner thread. Writes, resizes, and snapshots require an exclusive borrow. The PTY reply callback only appends bytes to a bounded buffer on that thread. A snapshot copies the viewport into Rust-owned rows, with one entry per column and width zero on a wide glyph's trailing cell. It includes resolved foreground and background colors and the viewport cursor. No handles or borrowed native data escape the crate.

`build.rs` fetches the Ghostty archive by commit and checks its SHA-256 before extracting it under Cargo's `OUT_DIR`. Zig uses its package cache on repeat builds. For a clean offline build, set `SUMIE_GHOSTTY_ARCHIVE` to the prefetched archive and `GHOSTTY_ZIG_SYSTEM_DIR` to the directory of unpacked, hash-named Zig packages. The private FFI module vendors the declarations it uses from the libghostty-rs revision for vt-1.0.1, without taking on its separate native build.

## Font pipeline (`sumie-font`)

`FontSystem` owns font discovery, loaded faces, shaping and rasterization scratch space, and one CPU atlas. Linux uses `fontconfig` for family aliases and locale-aware charset matches. Font discovery and the first use of a fallback face can block on file I/O. macOS discovery returns an unsupported-platform error until P1 Core Text integration.

The caller supplies the grid widths for an already ordered, left-to-right run, including zero-width continuations. The font crate does not infer widths from Unicode or consult terminal state. HarfRust shapes face runs with `calt` and `liga`; glyph advances inside a cluster do not change its grid span. Wide clusters use a two-cell slot, with fallback sizing based on the advance of `水` and ink centered in the slot.

Skrifa draws unhinted outlines into Zeno's 8-bit coverage rasterizer. Glyph origins and cell metrics use integer device pixels. Atlas entries retain their pixel rectangles when the shelf packer grows; growth marks the full atlas dirty. At a new scale or font size, replace the `FontSystem` and its atlas together. The binary reads the atlas dimensions, bytes, and dirty rectangles, then clears the dirty regions after consuming them. The font crate does not call the GPU.

## GPU renderer (`sumie-render`)

The caller owns the process-wide wgpu instance, adapter, and device and passes device and queue handles to the renderer. `backends()` selects `PRIMARY` (Vulkan on Linux, Metal on macOS); on Linux set `SUMIE_GPU_BACKEND=gl` to select the GLES backend instead. Pass that selection and the caller's instance descriptor to `instance_descriptor()`. It preserves the display handle required by GLES on Wayland and disables only indirect command validation on GL, where wgpu's internal compute pipeline can fail device creation. The renderer issues no indirect commands. `sumie-render` does not create native windows or depend on `sumie-font`.

`AtlasUpload` borrows a tightly packed R8 coverage atlas and dirty rectangles. Upload the full atlas on the first call or when its dimensions change; otherwise upload only dirty regions. `DrawList` borrows row-major cell backgrounds and glyph quads. Input colors are straight sRGB RGBA8. Geometry uses integer device pixels, and each glyph's atlas and destination rectangles have the same size. A glyph names its owning cell for contrast and coverage correction, including when its ink overhangs that cell.

The renderer uploads instances through a reusable staging belt and draws backgrounds, glyphs, and the optional cursor in that order in one render pass. The shader raises glyph contrast to at least 1.1 by moving its RGB toward black or white. Following Ghostty, it corrects grayscale coverage so the resulting luminance matches an sRGB interpolation, then blends premultiplied colors in linear light into an sRGB attachment. wgpu supplies the transient command encoder and acquired surface image each frame. The pipeline persists, and draw buffers and bindings change only when capacity or atlas dimensions change.

On the render thread, call `upload_atlas` and `draw` only after releasing the terminal lock. The caller schedules frames; the renderer never draws or presents on its own. `Renderer` draws to a reusable offscreen target. `SurfaceRenderer` owns a caller-created surface, configures it at nonzero device-pixel sizes, and presents once per successful draw. `resize` may wait for swapchain GPU work. Drop `SurfaceRenderer` on the render thread; its renderer resources drop before its surface.

The workspace tests use the same pixel checks on the selected backend and on Linux GLES. Set `SUMIE_REQUIRE_GPU=1` to fail instead of skipping when an adapter is unavailable. Set `SUMIE_RENDER_ARTIFACT_DIR` to an existing directory to save `grid.png` from the selected backend test.

## Unix PTY (`sumie-pty`)

`Pty` owns the Unix master, child process group, and one reader thread. It does not own terminal state, windows, rendering, or an event loop. `SpawnOptions` supplies the command or login shell, the complete child environment (including `TERM` and `COLORTERM`), the initial cell and pixel dimensions, and a close grace period. The default grace period is three seconds, with a five-second maximum. Spawn can wait for exec; the live-pane methods do not wait for PTY I/O or child exit.

The reader polls the master and a stop descriptor. It sends byte chunks through a bounded channel and signals `output_fd()` with an eventfd on Linux or a pipe on macOS. On readiness, clear the wake with `clear_output_wake()` before draining `try_recv_output()` to empty or disconnected. `enqueue_input()` admits whole messages into a 1 MiB FIFO. Poll `input_fd()` only while it is present and call `flush_input()` on write readiness; partial writes keep their original order. A closed peer returns `false` from `flush_input()` and the app closes the pane. `resize()` applies `TIOCSWINSZ`.

Keep the `Pty` after removing the pane from the UI. `close()` sends SIGHUP to the child group, discards queued I/O, and signals the reader to release the master. Call `service_child()` on `exit_fd()` readiness or at `close_deadline()`. It reaps a ready child or sends SIGKILL once at the deadline, then keeps watching until exit. The absence of `exit_fd()` marks completion even when no status is available because the child was already reaped. No other code may wait on the child or enable automatic reaping. `Drop` never joins or waits for the child; it hands any unreaped child to the reader thread for the same deadline and exit handling.

Linux uses rustix for pidfd, `waitid(P_PIDFD)`, and Unix descriptor operations. macOS uses nix for the safe kqueue API and rustix for `waitpid` after `EVFILT_PROC` reports `NOTE_EXIT`. `pty-process` supplies safe Unix PTY allocation and controlling-terminal spawn through its synchronous API; its Tokio feature is disabled.

## Wayland platform (`sumie-wayland`)

`Platform::connect` creates one xdg-toplevel with app ID `sumie` and registers its private Wayland queue in a calloop event loop. Connection setup performs a registry round trip. The caller dispatches and drains events on the same thread and registers non-Wayland sources with `Platform::loop_handle()`. The window requests server-side decorations. If the compositor does not grant them, P0 is undecorated and logs the fallback once.

Wait for `Resized` before sizing the renderer. `scale` is in units of 1/120. With fractional-scale-v1 and viewporter, the surface keeps `buffer_scale = 1`; its viewport destination is the logical size, and device dimensions round `logical × scale / 120`. Otherwise SCTK supplies the integer scale from `wl_surface.preferred_buffer_scale` or the outputs. Call `begin_frame()` immediately before committing a ready buffer. The initial configure and each completed frame callback release one frame slot; neither forces a redraw. Finish that buffer commit before changing IME state or dispatching again.

SCTK and calloop supply window, seat, and XKB dispatch and key repeat. `wayland-protocols` supplies the fractional-scale, viewporter, and text-input-v3 bindings. The adapter uses the first available keyboard seat and emits raw evdev codes and interpreted key data without matching shortcuts. The text-input object uses terminal content type, emits committed text before the next preedit at `done`, and applies the batch even when its serial is stale. A stale serial defers outgoing cursor changes until a matching `done`. The adapter resends the rectangle after focus returns, but does not resend an unchanged rectangle within a focus session. Text-input interface version 2 pairs its state commit with a surface commit.

`Platform::window_owner()` shares the SCTK window and its display connection with wgpu. The window owner supplies borrowed raw-window-handle traits and exposes no Wayland proxy. `Platform::display_handle_source()` retains only the connection for wgpu instance creation, including on GLES. SCTK destroys the xdg-toplevel, xdg-surface, and wl_surface in order after the last window owner drops. `cargo run -p sumie-wayland --example window` demonstrates `present_shm` and logs event metadata without input contents.

## Gates

Run `scripts/check-all.sh` from the repository root. It runs the read-only checks, then the negative controls. Use `--fast` to run only the read-only checks. Negative controls temporarily edit files and restore their bytes. They also compare Git porcelain status before and after. Do not edit the worktree while they run. Cargo uses `$CARGO_TARGET_DIR` if set, otherwise a `sumie/target` directory under the user's cache.

- Dependencies: inspect declarations in `cargo metadata`, including renamed, build, dev, and target-specific dependencies, regardless of the current platform.
- Workspace membership: every immediate directory under `crates/` must be a workspace member, and every member other than the root package belongs directly under `crates/`.
- Unsafe code: every Rust target root declares unconditional `#![forbid(unsafe_code)]`, except in `sumie-vt`, `sumie-font`, `sumie-render`, and `sumie-wayland`. In `sumie-wayland`, unsafe is limited to raw-window-handle trait implementations. Clippy denies undocumented unsafe blocks. `sumie-macos` is not exempt.
- File length and names: cap Rust sources and gate scripts at 600 physical lines. Reject reserved module and crate names (`util`, `common`, `helper`, `misc`). The module check is lexical: it checks source paths and `mod` declarations, ignores comments and literals, and does not expand macros.
- Lint policy: each manifest inherits workspace lints. Only the root `clippy.toml` is allowed. `allow` and `expect` cannot suppress `warnings`, `clippy::all`, `clippy::pedantic`, `clippy::too_many_lines`, `clippy::cognitive_complexity`, or `clippy::undocumented_unsafe_blocks`, including inside `cfg_attr`.
- Rust checks: check formatting, run Clippy with warnings denied (including an 80-line function limit and a cognitive complexity limit of 15), and require at least one passing workspace test. Gate scripts require Python 3.11 or newer.

A gate exits `1` for a violation and `2` if it cannot check its inputs. `scripts/install-hooks.sh` sets `core.hooksPath` to `scripts/hooks`. Undo that setting with `git config --local --unset core.hooksPath`; `git commit --no-verify` skips the hook for one commit.

Cloud CI runs the P0 merge gate on Linux x86_64, Linux arm64, and macOS arm64. The terminal snapshot test checks the wide-cell invariant at the VT boundary; the negative control changes a wide cell's width from two to one. Extend coverage when selection, editing, and rendering start consuming snapshots. Wire the remaining invariant checks alongside their features.
