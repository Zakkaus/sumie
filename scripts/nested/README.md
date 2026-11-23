# Keyboard input in virtual KWin

`run.sh` starts a private KWin and types into its focused Wayland client. It needs
no root, desktop-file permission, or change to the desktop compositor. Run it
from the repository root on Linux with `/scratch/ram`, KWin with fake input v6
(tested on 6.8.3), `kscreen-doctor`, `dbus-run-session`, Python 3, and GNU `timeout`.
For `--type`, also install `wayland-scanner`, a C compiler, `pkg-config`, the
Wayland and xkbcommon development files, and the Plasma Wayland protocols.

```bash
scripts/nested/run.sh 1.5 10 --type $'echo injected-ok\nexit\n' --after-ms 1500 -- /path/to/sumie
```

The runner sets the virtual output scale, launches the client, then sends the
UTF-8 text after the specified delay (default: 1500 ms). Use a literal newline
for Enter and a literal tab for Tab. Choose a delay long enough for the client
window to take focus. Omit `--type` to run without injection. `SECONDS` limits the
client's runtime; a timeout exits with status 124 (137 if forced to kill it).

For a screenshot, pass `--exit-after-frames N --screenshot /path/to/output.png`
to sumie. Choose a frame count reached after the output you want to capture.
Use changing terminal output or an enabled cursor blink to reach later frames.

## Why fake input

KWin advertises [`org_kde_kwin_fake_input`](https://invent.kde.org/libraries/plasma-wayland-protocols/-/blob/master/src/protocols/fake-input.xml)
v6 to the helper on the private socket. The helper calls `authenticate`, then
sends `keyboard_keysym` presses and releases. KWin 6.8.3 accepts the request for
this unsandboxed client without bypassing its protocol filter.

- `zwp_virtual_keyboard_v1`: the nested KWin registry did not advertise its manager.
- KWin scripts: the [scripting API](https://develop.kde.org/docs/plasma/kwin/api/)
  can register shortcuts and set focus but does not expose keyboard injection.
- `--virtual` alone: KWin's [virtual backend](https://invent.kde.org/plasma/kwin/-/blob/v6.8.3/src/backends/virtual/virtual_backend.cpp)
  provides outputs, not an input device that the installed binary can drive from the CLI.
- `ydotool`/uinput: sends kernel input to the host seat, not the virtual KWin;
  not run against the host.

The private session bus has no desktop service activation. The runner removes
its `/scratch/ram/sumie-rt.*` directory on exit. It does not sandbox the client.

## Terminal performance baseline

Use [`scripts/perf/baseline.sh`](../perf/baseline.sh) for the Ghostty and sumie
startup, idle, and memory comparison at scale 1.25. See
[`REQUIREMENTS.md`](../../REQUIREMENTS.md#ghostty-performance-baseline) for the
build command, measurement method, and recorded results.
