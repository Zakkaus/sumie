# sumie

[繁體中文](README.zh-TW.md)

sumie is a GPU terminal emulator written in Rust with libghostty-vt and wgpu. On Linux Wayland, drag with the left mouse button to select text. Double-click to select a word or a full URL. Triple-click to select a logical line, including any soft wraps. Drag after a double- or triple-click to extend the selection by words or lines. Releasing the button copies the selection to the clipboard. Press Ctrl+Shift+C to copy it again, or Ctrl+Shift+V to paste.

On Wayland, hold Ctrl over an OSC 8 link or a detected URL to underline it, including across soft wraps. Ctrl+click opens the link on release without changing the selection; Ctrl+drag selects text instead. Both gestures work even when the terminal program tracks the mouse. Sumie passes the URI directly to `xdg-open` and rejects bare paths, remote `file:` URLs, unrecognized schemes, and URIs containing control characters or bidirectional formatting marks.

Press Ctrl+Shift+F to search the screen and scrollback. Search treats wrapped rows as one line and ignores case unless the query contains an uppercase letter. Matches are numbered from oldest to newest. Enter or F3 selects an older match; hold Shift to select a newer one. Escape returns to the position where you opened search, or to the bottom if you were following output.

On Wayland, Ctrl+Shift+T opens a tab. Ctrl+Shift+E splits the focused pane to the right; Ctrl+Shift+O splits it down. Each new shell starts in the focused pane's last reported local directory (OSC 7), or in `$HOME` if none is available. New windows start in `$HOME`.

Click a pane to focus it, or press Ctrl+Alt+Arrow to focus a neighbour. Drag a divider or press Ctrl+Shift+Alt+Arrow to move it. Ctrl+Shift+Z shows the focused pane at the full tab size; press it again to restore the split. Ctrl+Shift+W closes the focused pane, as does exiting its shell. Its sibling takes the freed space. Closing the last pane closes the tab; closing the last tab closes the window.

Ctrl+Tab or Ctrl+PageDown selects the next tab; Ctrl+Shift+Tab or Ctrl+PageUp selects the previous one. Alt+1 through Alt+8 select tabs by position, and Alt+9 selects the last tab. Ctrl+Shift+PageUp and Ctrl+Shift+PageDown move the active tab left and right. The bar appears when a second tab opens. Click a tab to select it, middle-click to close it, or scroll over the bar to switch tabs.

Press Ctrl+= or Ctrl++ to increase the terminal font size by 1 pt, or Ctrl+- to decrease it. The shortcuts clamp zoom to 4–200 pt. All tabs in the window share the size, and the grid reflows without resizing the window. Ctrl+0 restores the configured size, as does changing `[font].size` in the config file.

On macOS 13 or later, sumie opens a Metal test page, not a shell. Run `cargo run --locked -- --exit-after-frames 3 --screenshot smoke.png` to save the third presented frame and close the window.

On first launch on Linux, sumie uses ncurses `tic` to install `xterm-sumie` under `$XDG_DATA_HOME/sumie/terminfo` (default: `~/.local/share/sumie/terminfo`). It reuses the entry until the bundled source changes. If it cannot install the entry, it starts the shell with `TERM=xterm-256color` instead.

For bash, zsh and fish login shells, sumie installs scripts under `$XDG_DATA_HOME/sumie/shell-integration` without editing your startup files. They mark prompts and commands (OSC 133) and report the working directory (OSC 7). The `ssh` wrapper sets `TERM=xterm-256color` for SSH only and requests `COLORTERM` and `TERM_PROGRAM` forwarding. The server must allow these variables through `AcceptEnv`. Set `integration = false` in `[shell]` to skip injection and leave `ssh` unchanged.

On Wayland, terminal programs can use OSC 52 to write up to 1 MiB of decoded data to the standard clipboard while the window has keyboard focus. Sumie denies OSC 52 reads and does not support primary selection. OSC 0 and OSC 2 update the window title; sumie does not support CSI 22/23 title push and pop.

The wheel scrolls three history lines per notch. If the program tracks the mouse, it receives wheel reports instead. In alternate-screen programs such as `less`, the wheel sends three arrow keys per notch when mode 1007 is enabled (the default). With mouse tracking on, hold Shift when you start a selection. You can release Shift while dragging without sending mouse motion to the program. Scrolling clears the selection; typing returns to the bottom.

On Linux, programs can display PNG, RGB and RGBA images with the Kitty graphics protocol. Direct transfers work over SSH. Local file transfers (`t=f` and `t=t`) accept regular files up to 320 MiB on tmpfs, ramfs or devtmpfs under `/tmp` or `/dev/shm`. Paths must be absolute, with no symlinks or hard links. With `t=t`, sumie checks that the path contains `tty-graphics-protocol` and still names the opened file before removing it. Use a direct transfer for images elsewhere, including on a disk-backed `/tmp`. Animation and video playback are not supported.

## Build and checks

Build the workspace from the repository root with `cargo build --workspace --locked`. The VT crate needs Rust 1.96 or later, Zig 0.17 (on `PATH` or set `ZIG`), and `curl`, `tar`, and `shasum` to fetch, verify, and unpack Ghostty on the first build.

With Git, Python 3.11 or newer, rustfmt, and Clippy available, run `scripts/check-all.sh` from the repository root. Use `--fast` to skip the probes that temporarily edit source files. See [ARCHITECTURE.md](ARCHITECTURE.md) for the crate boundaries and hook setup.

## Configure sumie on Linux

Create `$XDG_CONFIG_HOME/sumie/sumie.toml` (or `~/.config/sumie/sumie.toml`):

```toml
#:schema ./sumie.schema.json
include = []
minimum-contrast = 1.1 # 1.0 disables text contrast correction
scrollback-limit = 50000000

[font]
family = "JetBrains Mono"
size = 15.0 # points at 96 DPI
hinting = "light"

[colors]
foreground = "#ffffff"
background = "#122738"

[cursor]
style = "block"
blink = true
```

On Linux, saving the main file or an included file applies the changes without
restarting sumie. Invalid settings leave the previous settings in effect and show
the filename, line, and column at the top of the window until the next valid save.

Sumie uses Cobalt2 by default. Colors use `#rrggbb` notation; an optional
`palette` must contain all 16 ANSI colors in index order.

`scrollback-limit` sets the approximate scrollback limit in bytes. Lowering it
can discard history immediately. Set it to `0` to clear and disable scrollback.

Bold and italic use the font family's matching faces when available. They do not change the grid size. SGR 2 dims the foreground to 50% opacity. Underlines can be single, double, curly, dotted or dashed; SGR 58 sets their color independently of text. Strikethrough and overline can appear together with an underline.

Set `cursor.style` to `block`, `bar`, or `underline`. Terminal applications can
override the shape and blink setting with DECSCUSR until they reset it. An
unfocused window shows a steady outline for a visible cursor.

Run `sumie config show --defaults` for all options. To enable editor completion,
run `sumie config schema > sumie.schema.json` from the config directory.
For font comparisons, `SUMIE_FONT`, `SUMIE_FONT_PX` (logical pixels),
`SUMIE_FONT_WEIGHT`, and `SUMIE_FONT_HINTING` override the file.

## License

[GNU Affero General Public License v3.0 or later](LICENSE) (`AGPL-3.0-or-later`).
