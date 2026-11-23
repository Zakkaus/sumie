# sumie

[繁體中文](README.zh-TW.md)

sumie is a GPU terminal emulator written in Rust with libghostty-vt and wgpu. On Linux Wayland, drag with the left mouse button to select text. Releasing the button copies the selection to the clipboard. Press Ctrl+Shift+C to copy it again, or Ctrl+Shift+V to paste.

Press Ctrl+Shift+F to search the screen and scrollback. Search treats wrapped rows as one line and ignores case unless the query contains an uppercase letter. Enter or F3 selects an older match; hold Shift to select a newer one. Escape closes the bar and returns to the bottom unless you scrolled manually.

On macOS 13 or later, sumie opens a Metal test page, not a shell. Run `cargo run --locked -- --exit-after-frames 3 --screenshot smoke.png` to save the third presented frame and close the window.

On first launch on Linux, sumie uses ncurses `tic` to install `xterm-sumie` under `$XDG_DATA_HOME/sumie/terminfo` (default: `~/.local/share/sumie/terminfo`). It reuses the entry until the bundled source changes. If it cannot install the entry, it starts the shell with `TERM=xterm-256color` instead.

For bash, zsh and fish login shells, sumie installs scripts under `$XDG_DATA_HOME/sumie/shell-integration` without editing your startup files. They mark prompts and commands (OSC 133) and report the working directory (OSC 7). The `ssh` wrapper sets `TERM=xterm-256color` for SSH only and requests `COLORTERM` and `TERM_PROGRAM` forwarding. The server must allow these variables through `AcceptEnv`. Set `integration = false` in `[shell]` to skip injection and leave `ssh` unchanged.

On Wayland, terminal programs can use OSC 52 to write up to 1 MiB of decoded data to the standard clipboard while the window has keyboard focus. Sumie denies OSC 52 reads and does not support primary selection. OSC 0 and OSC 2 update the window title; sumie does not support CSI 22/23 title push and pop.

The wheel scrolls three history lines per notch. If the program tracks the mouse, it receives wheel reports instead. In alternate-screen programs such as `less`, the wheel sends three arrow keys per notch when mode 1007 is enabled (the default). With mouse tracking on, click to send a left click, drag to select terminal text, or hold Alt while dragging to send the drag to the program. Scrolling clears the selection; typing returns to the bottom.

## Build and checks

Build the workspace from the repository root with `cargo build --workspace --locked`. The VT crate needs Rust 1.96 or later, Zig 0.17 (on `PATH` or set `ZIG`), and `curl`, `tar`, and `shasum` to fetch, verify, and unpack Ghostty on the first build.

With Git, Python 3.11 or newer, rustfmt, and Clippy available, run `scripts/check-all.sh` from the repository root. Use `--fast` to skip the probes that temporarily edit source files. See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate boundaries and hook setup.

## Configure fonts, colors, and the cursor on Linux

Create `$XDG_CONFIG_HOME/sumie/sumie.toml` (or `~/.config/sumie/sumie.toml`):

```toml
#:schema ./sumie.schema.json
include = []
minimum-contrast = 1.1 # 1.0 disables text contrast correction

[font]
family = "JetBrains Mono"
size = 15.0 # points at 96 DPI
hinting = "light"

[colors]
foreground = "#cccccc"
background = "#1e1e1e"

[cursor]
style = "block"
blink = true
```

On Linux, saving the main file or an included file applies the changes without
restarting sumie. Invalid settings leave the previous settings in effect and show
the filename, line, and column at the top of the window until the next valid save.
Colors use `#rrggbb` notation; an optional `palette` must contain all 16 ANSI colors
in index order.

Set `cursor.style` to `block`, `bar`, or `underline`. Terminal applications can
override the shape and blink setting with DECSCUSR until they reset it. An
unfocused window shows a steady outline for a visible cursor.

Run `sumie config show --defaults` for all options. To enable editor completion,
run `sumie config schema > sumie.schema.json` from the config directory.
For font comparisons, `SUMIE_FONT`, `SUMIE_FONT_PX` (logical pixels),
`SUMIE_FONT_WEIGHT`, and `SUMIE_FONT_HINTING` override the file.

## License

[GNU Affero General Public License v3.0 or later](LICENSE) (`AGPL-3.0-or-later`).
