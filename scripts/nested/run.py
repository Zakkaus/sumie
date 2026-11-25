import argparse
import math
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time


SCRIPTS = Path(__file__).resolve().parent
SOCKET = "wl-sumie"


def parse_args():
    parser = argparse.ArgumentParser(
        usage="run.sh SCALE SECONDS [--type TEXT [--after-ms MS]] -- CLIENT [ARGS ...]",
    )
    parser.add_argument("scale", type=float)
    parser.add_argument("seconds", type=float)
    parser.add_argument("--type", dest="text", help="UTF-8 text; a newline presses Enter")
    parser.add_argument("--after-ms", type=int, help="delay from client launch (default: 1500)")
    args = sys.argv[1:]
    split = args.index("--") if "--" in args else len(args)
    options = parser.parse_args(args[:split])
    client = args[split + 1:]
    if not client:
        parser.error("expected -- followed by a client command")
    if any(not math.isfinite(n) or n <= 0 for n in (options.scale, options.seconds)):
        parser.error("scale and seconds must be positive finite numbers")
    if options.after_ms is not None and options.text is None:
        parser.error("--after-ms requires --type")
    if options.text is not None:
        options.after_ms = 1500 if options.after_ms is None else options.after_ms
        if not 0 <= options.after_ms < options.seconds * 1000:
            parser.error("--after-ms must fall within the client timeout")
    return options, client


def build_injector(runtime):
    xml = "/usr/share/plasma-wayland-protocols/fake-input.xml"
    header = runtime / "fake-input-client.h"
    code = runtime / "fake-input.c"
    binary = runtime / "type"
    subprocess.run(["wayland-scanner", "client-header", xml, header], check=True)
    subprocess.run(["wayland-scanner", "private-code", xml, code], check=True)
    flags = subprocess.check_output(
        ["pkg-config", "--cflags", "--libs", "wayland-client", "xkbcommon"], text=True,
    )
    subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-I", runtime,
                    SCRIPTS / "type.c", code, *shlex.split(flags), "-o", binary], check=True)
    return binary


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


def run_client(options, command, injector):
    client = subprocess.Popen(["timeout", "--kill-after=2s", str(options.seconds), *command])
    deadline = time.monotonic() + options.seconds
    try:
        if injector is not None:
            try:
                status = client.wait(timeout=options.after_ms / 1000)
            except subprocess.TimeoutExpired:
                subprocess.run([injector, options.text], check=True,
                               timeout=max(0, deadline - time.monotonic()))
            else:
                if status == 0:
                    raise RuntimeError("client exited before the scheduled input")
                return status
        return client.wait()
    finally:
        stop(client)


def main():
    options, command = parse_args()
    runtime = Path(os.environ.get("XDG_RUNTIME_DIR", ""))
    bus = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").partition(",")[0]
    if (not runtime.is_absolute() or not runtime.name.startswith("sumie-rt.")
            or bus != f"unix:path={runtime}/bus" or os.environ.get("WAYLAND_DISPLAY") != SOCKET
            or "DISPLAY" in os.environ or "WAYLAND_SOCKET" in os.environ):
        raise RuntimeError("start with run.sh to create a private runtime and D-Bus session")

    injector = build_injector(runtime) if options.text is not None else None
    env = os.environ.copy()
    if injector is not None:
        env.update(KWIN_WAYLAND_NO_PERMISSION_CHECKS="1", KWIN_XKB_DEFAULT_KEYMAP="1",
                   XKB_DEFAULT_RULES="evdev", XKB_DEFAULT_MODEL="pc105", XKB_DEFAULT_LAYOUT="us",
                   XKB_DEFAULT_VARIANT="", XKB_DEFAULT_OPTIONS="")
    kwin = subprocess.Popen(["kwin_wayland", "--virtual", "--no-lockscreen", "--no-kactivities",
                             "--no-global-shortcuts", "--socket", SOCKET,
                             "--width", "1280", "--height", "800", "--scale", str(options.scale)],
                            env=env)
    try:
        deadline = time.monotonic() + 10
        while not (runtime / SOCKET).is_socket():
            if kwin.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError("KWin did not create its Wayland socket")
            time.sleep(0.05)

        # Virtual outputs can start at scale 1 despite KWin's --scale argument.
        subprocess.run(["kscreen-doctor", f"output.Virtual-0.scale.{options.scale}"],
                       check=True, timeout=10, env=dict(os.environ, QT_QPA_PLATFORM="wayland"))
        return run_client(options, command, injector)
    finally:
        stop(kwin)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, signal.default_int_handler)
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
    except subprocess.CalledProcessError as error:
        # A failed injector's arguments contain the input text.
        print(f"nested: {Path(error.cmd[0]).name} exited {error.returncode}", file=sys.stderr)
        sys.exit(1)
    except subprocess.TimeoutExpired as error:
        print(f"nested: {Path(error.cmd[0]).name} timed out", file=sys.stderr)
        sys.exit(124)
    except (OSError, RuntimeError) as error:
        print(f"nested: {error}", file=sys.stderr)
        sys.exit(1)
