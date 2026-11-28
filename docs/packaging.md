# Packaging sumie

This page is for people who build sumie for a distribution. The project ships prebuilt
x86_64 and aarch64 Linux tarballs and an Apple silicon macOS disk image with every release;
use them when your policy allows, because the Zig build is the hardest part to package.

## Versions

| Tool | Version | Why |
| --- | --- | --- |
| Rust | 1.96 or newer | `rust-version` in `Cargo.toml` |
| Zig | 0.17.1 | `crates/sumie-vt/build.rs` runs `zig build` for libghostty-vt and accepts any `0.17.x`; CI and releases use 0.17.1 |
| C toolchain, `curl`, `tar`, `shasum` | any | `build.rs` shells out to them; `shasum -a 256` is Perl's `shasum`, and a one-line wrapper around `sha256sum` also works |
| `pkg-config`, Wayland and xkbcommon headers | any | Linux only (`libwayland-dev libxkbcommon-dev` on Debian) |

Zig is not a runtime dependency and it does not need to be the system Zig: put any 0.17 binary on
`PATH` or set `ZIG=/path/to/zig`.

The build is native-only: `build.rs` refuses to run when `HOST` differs from `TARGET`, so build
each architecture on its own hardware or in an emulated chroot. libghostty-vt is built with
`-Dcpu=baseline`, so the result runs on any CPU of its architecture.

## What the build downloads

`cargo build` fetches three things beyond the crates.io index:

1. **Git dependencies.** The root `Cargo.toml` pins forks of `wgpu` (`naga`, `naga-types`,
   `wgpu-hal`, `wgpu-naga-bridge`, `wgpu-types`) and `winit` by full commit
   (`third_party/README.md` lists why). `cargo vendor` handles them.
2. **The Ghostty source.** `build.rs` downloads
   `https://codeload.github.com/ghostty-org/ghostty/tar.gz/108cacb17ed8990840cd14abfce43d8ed8503ca1`
   and checks its SHA-256 (`2c276d3e62cb15fb38f0d7f621f199029d1d4df86538096c6bec2a107cad7159`).
   Set `SUMIE_GHOSTTY_ARCHIVE` to a local copy of that file to skip the download; the checksum
   is still enforced. The constants are `GHOSTTY_COMMIT` and `GHOSTTY_ARCHIVE_SHA256` at the
   top of `crates/sumie-vt/build.rs`.
3. **Zig packages.** Building libghostty-vt makes Zig fetch six packages from
   `deps.files.ghostty.org`:

   ```text
   aro-d0c8c4d9c55daa7ef6e40cf0f630a5b5e900989b.tar.gz
   highway-66486a10623fa0d72fe91260f96c892e41aceb06.tar.gz
   oniguruma-1220c15e72eadd0d9085a8af134904d9a0f5dfcbed5f606ad60edc60ebeccd9706bb.tar.gz
   translate_c-02ff0c523fb92a38939b04d0145c4a9602a74d56.tar.gz
   uucode-1fb73433bba5d93366c57f23ff2e9d7939746500.tar.gz
   wuffs-7411f488fe2e2c205c3d3b3d28638b7356522930.tar.gz
   ```

   The first five are Ghostty's own `deps/libghostty-vt.txt`; `oniguruma` is added because
   sumie builds with Ghostty's default `-Dsimd=true`, which needs it.

## Offline build

Prefetch everything on a machine with network access, then build with `--offline` and no
network. The steps below were run end to end inside a network-less namespace
(`unshare -rn`) to confirm that nothing else is fetched.

```sh
# 1. Rust crates and the git forks. Merge the printed [source] tables into .cargo/config.toml.
cargo vendor --locked vendor > cargo-vendor-config.toml

# 2. The pinned Ghostty source.
curl -fsSL -o ghostty.tar.gz \
  https://codeload.github.com/ghostty-org/ghostty/tar.gz/108cacb17ed8990840cd14abfce43d8ed8503ca1
echo '2c276d3e62cb15fb38f0d7f621f199029d1d4df86538096c6bec2a107cad7159  ghostty.tar.gz' |
  sha256sum -c -

# 3. The Zig packages, unpacked into a cache directory by Ghostty's own script.
mkdir ghostty-src && tar -xzf ghostty.tar.gz --strip-components=1 -C ghostty-src
{ cat ghostty-src/deps/libghostty-vt.txt
  echo https://deps.files.ghostty.org/oniguruma-1220c15e72eadd0d9085a8af134904d9a0f5dfcbed5f606ad60edc60ebeccd9706bb.tar.gz
} > zig-deps.txt
ZIG_GLOBAL_CACHE_DIR="$PWD/zig-cache" \
  sh ghostty-src/nix/build-support/fetch-zig-cache.sh zig-deps.txt

# 4. The offline build. Zig writes into its cache, so give each build a copy.
cp -a zig-cache zig-cache-build
SUMIE_GHOSTTY_ARCHIVE="$PWD/ghostty.tar.gz" \
ZIG_GLOBAL_CACHE_DIR="$PWD/zig-cache-build" \
  cargo build --release --locked --offline
```

Notes:

- Do **not** use `GHOSTTY_ZIG_SYSTEM_DIR` (which passes `zig build --system`) on Linux. In system
  mode Ghostty resolves oniguruma as a system library, the Zig build fails with
  `unable to find artifact "oniguruma"`, and `build.rs` would not link `libonig` anyway. A
  pre-populated `ZIG_GLOBAL_CACHE_DIR` is addressed by package hash and needs no extra flag.
- Use a fresh cache directory per build. Zig 0.17 can crash when a cache holds a package
  archive that a `--system` directory lacks, and a fresh copy also proves the build used only
  what you prefetched.
- The list changes when `GHOSTTY_COMMIT` changes. To regenerate it, run one online build with a
  new empty `ZIG_GLOBAL_CACHE_DIR` and read `p/*.tar.gz` in it: each file is one package that the
  build fetched. Update the URLs, hashes in your package recipe and this page together.
- `build.rs` patches the unpacked Ghostty source. Those patches add no downloads.
- `cargo build` does not strip the binary (about 20 MB with symbols). Strip it in your install
  step.

## Runtime dependencies

The Linux binary links `libc`, `libgcc_s`, `libm` and `libxkbcommon.so.0` at load time. It opens
the following with `dlopen`, so a package manager does not find them from `ldd`; declare them
by hand:

| Library | Package (Debian name) | Needed for |
| --- | --- | --- |
| `libwayland-client.so.0` | `libwayland-client0` | Window, input, clipboard |
| `libwayland-egl.so.1`, `libEGL.so.1` | `libwayland-egl1`, `libegl1` | GL ES 3 rendering |
| `libvulkan.so.1` | `libvulkan1` | Vulkan rendering |
| `libfontconfig.so.1` | `libfontconfig1` | Font discovery |
| `tic` | `ncurses-bin` | Compiling `xterm-sumie` on first launch |
| A GPU driver (Mesa or a vendor driver) | `mesa-vulkan-drivers`, `libgl1-mesa-dri` | At least one of Vulkan or GL ES 3 |

A Wayland compositor is required; there is no X11 backend. Suggest a monospace font such as
JetBrains Mono, the default family, and a CJK font for fallback. Check the list with
`strings -a target/release/sumie | grep -o 'lib[a-z0-9_+-]*\.so[.0-9]*' | sort -u` after a
dependency bump.

## Files to install

| Source | Destination | Notes |
| --- | --- | --- |
| `target/release/sumie` | `/usr/bin/sumie` | One static-ish executable; no helper programs |
| `assets/sumie.desktop` | `/usr/share/applications/sumie.desktop` | Launcher entry |
| `assets/icons/hicolor/scalable/apps/sumie.svg` | `/usr/share/icons/hicolor/scalable/apps/sumie.svg` | Application icon |
| `assets/icons/hicolor/symbolic/apps/sumie-symbolic.svg` | `/usr/share/icons/hicolor/symbolic/apps/sumie-symbolic.svg` | 16 px panel icon |
| `LICENSE` | `/usr/share/licenses/sumie/LICENSE` | AGPL-3.0-or-later |
| `assets/icons/OFL-YujiSyuku.txt` | `/usr/share/licenses/sumie/OFL-YujiSyuku.txt` | The icon outlines come from an OFL font |

The Wayland `app_id` is `sumie`, which matches `StartupWMClass` in the desktop entry.

### Terminfo and shell integration

Both are embedded in the binary. On the first launch sumie writes them under
`$XDG_DATA_HOME/sumie/` (`~/.local/share/sumie/` by default):

- `terminfo/`: it compiles `terminfo/xterm-sumie.terminfo` with `tic -x`, and starts the shell
  with `TERM=xterm-sumie` and `TERMINFO` pointing there. If `tic` fails, sumie logs a warning
  and falls back to `TERM=xterm-256color`.
- `shell-integration/`: the bash, zsh and fish scripts from `shell-integration/`. The shell
  starts through them, and `[shell] integration = false` turns this off.

The binary never reads a system copy of either, so installing them system-wide is optional. The
one reason to do it is that other programs on the same machine, such as `sudo` or a `tmux`
started under another user, can then resolve `xterm-sumie`:

```sh
tic -x -o /usr/share/terminfo terminfo/xterm-sumie.terminfo
```

Do not install the shell scripts into `/etc/profile.d`; sumie loads its own copy.

## Configuration

There is no system-wide configuration file. Users write `~/.config/sumie/sumie.toml`
(or `$XDG_CONFIG_HOME/sumie/sumie.toml`). `sumie config show --defaults` prints a complete
example and `sumie config schema` prints the JSON Schema; ship either as documentation if you
like.

## Checks for a package recipe

```sh
scripts/check-all.sh --fast   # needs Python 3.11 or newer
cargo test --release --locked --offline
```

The tests that draw need a GPU; CI runs them on Mesa software drivers (`lavapipe` for Vulkan)
with `SUMIE_REQUIRE_GPU=1`. Without a driver those tests skip.

## The macOS bundle's font

`scripts/macos/bundle.sh` copies JetBrains Mono 2.304 (SIL Open Font License 1.1) into
`sumie.app/Contents/Resources/Fonts/` (Regular, Bold, Italic, BoldItalic and `OFL.txt`) and
registers the directory with `ATSApplicationFontsPath`, because stock macOS lacks the default
`font.family`. The script downloads
`https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip`
and checks its SHA-256 (`6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`);
set `SUMIE_FONT_ZIP` to a local copy of that file for an offline build. The checksum is still
enforced. The constants are `FONT_VERSION`, `FONT_URL` and `FONT_SHA256` at the top of the script.

## Release archive notices

Both release packagers take a `LICENSES` path and copy it next to `LICENSE` in the artifact.
Install `cargo-about` 0.9.3 (`cargo install --locked --version 0.9.3 --features cli cargo-about`).
From the repository root, generate the report for the locked Cargo dependencies:

```sh
cargo about generate --locked --fail -c third_party/about.toml third_party/licenses.hbs \
  -o /path/to/THIRD-PARTY-LICENSES
```

Pass that file to `scripts/linux/package.sh BINARY OUTPUT_DIR VERSION ARCH LICENSES` or
`scripts/macos/bundle.sh --dmg BINARY OUTPUT_DIR LICENSES VERSION`.

## Cutting a release

1. Set `version` under `[workspace.package]` in `Cargo.toml`, refresh `Cargo.lock`, and merge.
2. Push the tag `v<version>`. The release workflow stops if the tag, without the leading `v`,
   differs from the workspace version in `cargo metadata`.
3. The workflow builds both Linux tarballs and the macOS disk image, then opens a draft
   release whose notes come from [release-notes.md](release-notes.md). Review the draft and
   publish it by hand.

Before publishing, follow the draft's Gatekeeper instructions on a clean macOS 26 or 27
session with a freshly downloaded copy. The initial launch should show the warning; the
Privacy & Security override should then open the app without disabling Gatekeeper.

The notes mark the release as a preview and state the macOS limits: arm64 only, ad-hoc signed,
not notarized, declared minimum macOS 13 with only macOS 26 and 27 tested. Linux is Wayland only.
