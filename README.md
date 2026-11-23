# sumie

[繁體中文](README.zh-TW.md)

sumie is a GPU terminal emulator written in Rust with libghostty-vt and wgpu. On Linux Wayland, drag with the left mouse button to select text. Releasing the button copies the selection to the clipboard. Press Ctrl+Shift+C to copy it again, or Ctrl+Shift+V to paste. macOS support is planned.

## Build and checks

Build the workspace from the repository root with `cargo build --workspace --locked`. The VT crate needs Rust 1.96 or later, Zig 0.17 (on `PATH` or set `ZIG`), and `curl`, `tar`, and `shasum` to fetch, verify, and unpack Ghostty on the first build.

With Git, Python 3.11 or newer, rustfmt, and Clippy available, run `scripts/check-all.sh` from the repository root. Use `--fast` to skip the probes that temporarily edit source files. See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate boundaries and hook setup.

## Configure fonts on Linux

Create `$XDG_CONFIG_HOME/sumie/sumie.toml` (or `~/.config/sumie/sumie.toml`):

```toml
#:schema ./sumie.schema.json
include = []

[font]
family = "JetBrains Mono"
size = 15.0 # points at 96 DPI
hinting = "light"
```

Run `sumie config show --defaults` for all options. To enable editor completion,
run `sumie config schema > sumie.schema.json` from the config directory.
For font comparisons, `SUMIE_FONT`, `SUMIE_FONT_PX` (logical pixels),
`SUMIE_FONT_WEIGHT`, and `SUMIE_FONT_HINTING` override the file.

## License

[GNU Affero General Public License v3.0 or later](LICENSE) (`AGPL-3.0-or-later`).
