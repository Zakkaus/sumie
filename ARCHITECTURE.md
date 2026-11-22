# Architecture

The [design book](https://claude.ai/artifact/A3KR8SWWJouwinJsSZUje8) defines the crate boundaries. Before adding a crate or module, document what it owns and what it must not do. Define interfaces at their call sites and record constraints on thread, blocking, and call order when those interfaces exist.

## Crates and allowed dependencies

Edges in the last column go from the row's crate to the listed crates. They are permissions, not dependencies to declare before use. The P0 skeleton declares only the binary's target-specific platform dependencies. The P3 packages are listed for reference and are not workspace members.

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

## Terminal core (`sumie-vt`)

`Terminal` owns one libghostty-vt terminal, one render state, and their row and cell iterators. Its handles are `!Send` and `!Sync`: this API keeps the terminal on its owner thread. Writes, resizes, and snapshots require an exclusive borrow. The wrapper registers no callbacks. A snapshot copies the viewport into Rust-owned rows, with one entry per column and width zero on a wide glyph's trailing cell. No handles or borrowed native data escape the crate.

`build.rs` fetches the Ghostty archive by commit and checks its SHA-256 before extracting it under Cargo's `OUT_DIR`. Zig uses its package cache on repeat builds. For a clean offline build, set `SUMIE_GHOSTTY_ARCHIVE` to the prefetched archive and `GHOSTTY_ZIG_SYSTEM_DIR` to the directory of unpacked, hash-named Zig packages. The private FFI module vendors the declarations it uses from the libghostty-rs revision for vt-1.0.1, without taking on its separate native build.

## Font pipeline (`sumie-font`)

`FontSystem` owns font discovery, loaded faces, shaping and rasterization scratch space, and one CPU atlas. Linux uses `fontconfig` for family aliases and locale-aware charset matches. Font discovery and the first use of a fallback face can block on file I/O. macOS discovery returns an unsupported-platform error until P1 Core Text integration.

The caller supplies the grid widths for an already ordered, left-to-right run, including zero-width continuations. The font crate does not infer widths from Unicode or consult terminal state. HarfRust shapes face runs with `calt` and `liga`; glyph advances inside a cluster do not change its grid span. Wide clusters use a two-cell slot, with fallback sizing based on the advance of `水` and ink centered in the slot.

Skrifa draws unhinted outlines into Zeno's 8-bit coverage rasterizer. Glyph origins and cell metrics use integer device pixels. Atlas entries retain their pixel rectangles when the shelf packer grows; growth marks the full atlas dirty. At a new scale or font size, replace the `FontSystem` and its atlas together. The binary reads the atlas dimensions, bytes, and dirty rectangles, then clears the dirty regions after consuming them. The font crate does not call the GPU.

## Gates

Run `scripts/check-all.sh` from the repository root. It runs the read-only checks, then the negative controls. Use `--fast` to run only the read-only checks. Negative controls temporarily edit files and restore their bytes. They also compare Git porcelain status before and after. Do not edit the worktree while they run. Cargo uses `$CARGO_TARGET_DIR` if set, otherwise a `sumie/target` directory under the user's cache.

- Dependencies: inspect declarations in `cargo metadata`, including renamed, build, dev, and target-specific dependencies, regardless of the current platform.
- Workspace membership: every immediate directory under `crates/` must be a workspace member, and every member other than the root package belongs directly under `crates/`.
- Unsafe code: every Rust target root declares unconditional `#![forbid(unsafe_code)]`, except in `sumie-vt`, `sumie-font`, and `sumie-render`. Clippy denies undocumented unsafe blocks. macOS is not exempt.
- File length and names: cap Rust sources and gate scripts at 600 physical lines. Reject reserved module and crate names (`util`, `common`, `helper`, `misc`). The module check is lexical: it checks source paths and `mod` declarations, ignores comments and literals, and does not expand macros.
- Lint policy: each manifest inherits workspace lints. Only the root `clippy.toml` is allowed. `allow` and `expect` cannot suppress `warnings`, `clippy::all`, `clippy::pedantic`, `clippy::too_many_lines`, `clippy::cognitive_complexity`, or `clippy::undocumented_unsafe_blocks`, including inside `cfg_attr`.
- Rust checks: check formatting, run Clippy with warnings denied (including an 80-line function limit and a cognitive complexity limit of 15), and require at least one passing workspace test. Gate scripts require Python 3.11 or newer.

A gate exits `1` for a violation and `2` if it cannot check its inputs. `scripts/install-hooks.sh` sets `core.hooksPath` to `scripts/hooks`. Undo that setting with `git config --local --unset core.hooksPath`; `git commit --no-verify` skips the hook for one commit.

Cloud CI runs the P0 merge gate on Linux x86_64, Linux arm64, and macOS arm64. The terminal snapshot test checks the wide-cell invariant at the VT boundary; the negative control changes a wide cell's width from two to one. Extend coverage when selection, editing, and rendering start consuming snapshots. Wire the remaining invariant checks alongside their features.
