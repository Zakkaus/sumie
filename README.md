# sumie

[繁體中文](README.zh-TW.md)

sumie is a GPU terminal emulator written in Rust with libghostty-vt and wgpu. On Linux Wayland, drag with the left mouse button to select text. Releasing the button copies the selection to the clipboard. Press Ctrl+Shift+C to copy it again, or Ctrl+Shift+V to paste. macOS support is planned.

On first launch, sumie uses ncurses `tic` to install `xterm-sumie` under `$XDG_DATA_HOME/sumie/terminfo` (default: `~/.local/share/sumie/terminfo`). It reuses the entry until the bundled source changes. If it cannot install the entry, it starts the shell with `TERM=xterm-256color` instead.

On Wayland, terminal programs can use OSC 52 to write up to 1 MiB of decoded data to the standard clipboard while the window has keyboard focus. Sumie denies OSC 52 reads and does not support primary selection. OSC 0 and OSC 2 update the window title; sumie does not support CSI 22/23 title push and pop.
The wheel scrolls three history lines per notch. If the program tracks the mouse, it receives wheel reports instead. In alternate-screen programs such as `less`, the wheel sends three arrow keys per notch when mode 1007 is enabled (the default). With mouse tracking on, click to send a left click, drag to select terminal text, or hold Alt while dragging to send the drag to the program. Scrolling clears the selection; typing returns to the bottom.

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
