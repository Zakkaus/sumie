# sumie

sumie is a GPU terminal emulator written in Rust. It draws with wgpu (Vulkan or GL ES 3 on
Linux, Metal on macOS) and parses terminal output with
[libghostty-vt](https://github.com/ghostty-org/ghostty), the terminal core of Ghostty.

> **Status: pre-release.** The Linux (Wayland) build is a working terminal. The macOS build
> currently opens a fixed GPU test page, not a terminal: the PTY, tabs and input are not wired
> into it yet. There is no stable configuration or command-line interface until the first
> release.

## Screenshots

Screenshots are not published yet.

## Features

On Linux (Wayland):

- Tabs in a drawn tab bar, with move, cycle and select-by-number shortcuts.
- Split panes with directional focus, resize and zoom.
- Scrollback with smooth, pixel-based scrolling (or whole rows), and search.
- Text selection, copy and paste, and clickable links.
- Kitty graphics protocol images, with a configurable thumbnail height.
- Text shaping with HarfRust and glyph fallback through fontconfig.
- Input methods through `zwp_text_input_v3`, and fractional scaling.
- Desktop notifications and tab progress from terminal escape sequences (OSC 9;4 progress).
- Shell integration for bash, zsh and fish, which reports the working directory to sumie for
  the tab label and for new tabs and panes. It installs itself on first launch and can be
  turned off with `[shell] integration = false`.
- TOML configuration that reloads while sumie runs, with a JSON Schema for editors.
- Window background opacity and, where the compositor supports it, background blur.
- Minimum text contrast correction (WCAG), on by default.

The default colors are Cobalt2 and the default font is JetBrains Mono. Install a font you like
and set `[font] family`.

### Default shortcuts

| Keys | Action |
| --- | --- |
| Ctrl+Shift+T / Ctrl+Shift+W | New tab / close tab |
| Ctrl+Tab, Ctrl+Shift+Tab (or Ctrl+PageDown, Ctrl+PageUp) | Next / previous tab |
| Ctrl+Shift+PageUp / PageDown | Move the tab left / right |
| Alt+1 to Alt+8, Alt+9 | Select a tab by position, select the last tab |
| Ctrl+Shift+E / Ctrl+Shift+O | Split the pane horizontally / vertically |
| Ctrl+Alt+arrows | Move focus between panes |
| Ctrl+Alt+Shift+arrows | Resize the focused pane |
| Ctrl+Shift+Z | Zoom the focused pane |
| Ctrl+Shift+C / Ctrl+Shift+V | Copy / paste |
| Ctrl+Shift+F | Search the scrollback |
| Ctrl+`+`, Ctrl+`-`, Ctrl+`0` | Increase, decrease, reset the font size |

## Install

### From a release

Download an archive from the [releases page](https://github.com/Zakkaus/sumie/releases) and
check it against `SHA256SUMS`.

**Linux.** `sumie-VERSION-x86_64-linux.tar.gz` and `sumie-VERSION-aarch64-linux.tar.gz` hold the
binary, the terminfo source, the shell-integration scripts, a desktop entry and the icons.
Unpack it anywhere and run `./sumie`, or copy the files under `share/` into `/usr/local/share`
or `~/.local/share` to get the launcher entry. The binary needs a Wayland compositor; there is
no X11 backend.

**macOS.** `sumie-VERSION-aarch64-macos.dmg` is for Apple silicon only. Open it and drag
`sumie.app` to Applications. The app is signed ad hoc, not with a Developer ID, and is not
notarized, so macOS quarantines it after a download. Do one of the following:

- Open the app once, dismiss the warning, then open System Settings, Privacy & Security and
  choose "Open Anyway" next to sumie.
- Or remove the quarantine flag: `xattr -dr com.apple.quarantine /Applications/sumie.app`.

macOS 13 is the declared minimum. Only macOS 15 (the CI runner) and macOS 27 are tested. As
noted at the top, the macOS build shows a GPU test page for now.

### Distribution packages

AUR and Gentoo packages are planned and will be maintained in this project. For other
distributions see [docs/packaging.md](docs/packaging.md).

## Build from source

You need:

- Rust 1.96 or newer.
- [Zig](https://ziglang.org/download/) 0.17.1. The build compiles libghostty-vt with Zig and
  checks that `zig version` starts with `0.17.`; set `ZIG` to use a binary outside `PATH`.
- `curl`, `tar` and `shasum` (the build downloads and verifies the pinned Ghostty source and
  Zig packages; [docs/packaging.md](docs/packaging.md) covers building without a network).
- Linux: `pkg-config` and the development files for `wayland` and `xkbcommon`
  (`libwayland-dev libxkbcommon-dev` on Debian and Ubuntu).
- macOS: the Xcode command line tools.

```sh
cargo build --release --locked
target/release/sumie
```

The first build compiles libghostty-vt and takes a few minutes. Set `ZIG_GLOBAL_CACHE_DIR` to
keep Zig's package downloads between clean builds.

### Linux runtime requirements

The binary links only libc, libgcc and `libxkbcommon.so.0`. It loads these at run time, so it
starts without them but cannot open a window or draw:

| Library | For |
| --- | --- |
| `libwayland-client.so.0` and a Wayland compositor | The window, input and clipboard |
| `libEGL.so.1`, `libwayland-egl.so.1` | GL ES 3 rendering |
| `libvulkan.so.1` and a Vulkan driver | Vulkan rendering |
| `libfontconfig.so.1` | Finding fonts |
| `tic` (ncurses) | Compiling the bundled `xterm-sumie` terminfo entry on first launch |

You need at least one of the Vulkan or GL ES 3 stacks with a working driver. On Debian and
Ubuntu: `libwayland-client0 libwayland-egl1 libegl1 libvulkan1 libfontconfig1 ncurses-bin`
plus Mesa drivers (`mesa-vulkan-drivers libgl1-mesa-dri`) or your GPU vendor's driver.

## Configuration

sumie reads `$XDG_CONFIG_HOME/sumie/sumie.toml`, which is `~/.config/sumie/sumie.toml` when
`XDG_CONFIG_HOME` is unset. A missing file means defaults. sumie watches the file and the files
it includes and applies a change to a running window; a file with an error keeps the previous
settings.

```sh
sumie config show --defaults    # print every default as TOML
sumie config schema             # print the JSON Schema
```

Top-level tables are `font`, `colors`, `cursor`, `shell`, `images`, `window` and `scroll`. The
top-level keys `include`, `minimum-contrast` and `scrollback-limit` have no table. `include`
lists other files, relative to the file that names them. Each key has a doc comment in the
schema.

For font comparisons, `SUMIE_FONT`, `SUMIE_FONT_PX` (logical pixels),
`SUMIE_FONT_WEIGHT`, and `SUMIE_FONT_HINTING` override the file.
`SUMIE_FRAME_LOG=PATH` writes per-frame stage timestamps and repainted pane
IDs, with no terminal content, for the multi-pane frame budget harness.

## Project documents

- [CONTRIBUTING.md](CONTRIBUTING.md): build, test, and the checks every change passes.
- [ARCHITECTURE.md](ARCHITECTURE.md): crates, what each owns, and the allowed dependencies.
- [docs/packaging.md](docs/packaging.md): packaging, offline builds, install locations.

## License

AGPL-3.0-or-later. See [LICENSE](LICENSE).

The icon is outline art from the Yuji Syuku typeface (SIL Open Font License 1.1); see
[assets/icons/README.md](assets/icons/README.md).
