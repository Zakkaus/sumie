# sumie

[繁體中文](README.zh-TW.md)

sumie is a GPU terminal emulator for Linux Wayland and macOS, written in Rust and designed around libghostty-vt and wgpu. Status: pre-alpha, P0. The workspace builds, but does not yet open a window or run a terminal session.

## Build and checks

Build the workspace from the repository root with `cargo build --workspace --locked`. The VT crate needs Rust 1.96 or later, Zig 0.17 (on `PATH` or set `ZIG`), and `curl`, `tar`, and `shasum` to fetch, verify, and unpack Ghostty on the first build.

With Git, Python 3.11 or newer, rustfmt, and Clippy available, run `scripts/check-all.sh` from the repository root. Use `--fast` to skip the probes that temporarily edit source files. See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate boundaries and hook setup.

## License

[GNU Affero General Public License v3.0 or later](LICENSE) (`AGPL-3.0-or-later`).
