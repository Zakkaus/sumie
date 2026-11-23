# sumie

[繁體中文](README.zh-TW.md)

sumie is a GPU terminal emulator written in Rust with libghostty-vt and wgpu. On Linux Wayland, drag with the left mouse button to select text. Releasing the button copies the selection to the clipboard. Press Ctrl+Shift+C to copy it again, or Ctrl+Shift+V to paste. macOS support is planned.

## Build and checks

Build the workspace from the repository root with `cargo build --workspace --locked`. The VT crate needs Rust 1.96 or later, Zig 0.17 (on `PATH` or set `ZIG`), and `curl`, `tar`, and `shasum` to fetch, verify, and unpack Ghostty on the first build.

With Git, Python 3.11 or newer, rustfmt, and Clippy available, run `scripts/check-all.sh` from the repository root. Use `--fast` to skip the probes that temporarily edit source files. See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate boundaries and hook setup.

## Font comparison on Linux

Set `SUMIE_FONT` to the font family and `SUMIE_FONT_PX` to the logical pixel size.
`SUMIE_FONT_HINTING` accepts `off` (default), `light`, or `normal`. Set
`SUMIE_FONT_WEIGHT` to an integer from 100 through 900 to select the primary
font weight; omit it to use the platform's default match.

## License

[GNU Affero General Public License v3.0 or later](LICENSE) (`AGPL-3.0-or-later`).
