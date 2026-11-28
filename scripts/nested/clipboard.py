"""Verify selection release and middle-click paste against a raw PTY."""
import base64
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys
import time
import tty
sys.dont_write_bytecode = True

from run import stop

SENTINEL = b"clipboard-only"
START_PASTE = b"\x1b[200~"
END_PASTE = b"\x1b[201~"
TRACKED_CLICK = re.compile(rb"\x1b\[<1;([1-9]\d*;[1-9]\d*)M\x1b\[<1;\1m")


class Violation(Exception):
    pass


def receive_position_report():
    reply = b""
    while not reply.endswith(b"R"):
        chunk = os.read(0, 64)
        if not chunk:
            raise RuntimeError("the PTY closed before the cursor position report")
        reply += chunk


def record(runtime):
    tty.setraw(0)
    cols, rows = os.get_terminal_size(0)
    # The centre of the only window lands on the same test text regardless of its placement.
    line = "X" * (cols - 1)
    grid = "".join(f"\x1b[{row};1H{line}" for row in range(1, rows + 1))
    os.write(1, ("\x1b[?25l\x1b[?2004h" + grid + "\x1b[6n").encode())
    receive_position_report()
    with (runtime / "pty").open("wb", buffering=0) as output:
        (runtime / "ready").touch()
        while chunk := os.read(0, 4096):
            if chunk == b"\x01":  # Ctrl+A: change only the standard clipboard via OSC 52.
                os.write(1, b"\x1b]52;c;" + base64.b64encode(SENTINEL) + b"\x07")
            elif chunk == b"\x02":  # Ctrl+B: enable SGR mouse tracking.
                os.write(1, b"\x1b[?1002h\x1b[?1006h\x1b[6n")
                receive_position_report()
                (runtime / "tracking").touch()
            elif chunk == b"\x04":  # Ctrl+D: exit the fixture shell.
                return 0
            else:
                output.write(chunk)
    return 0


def build_fixture(runtime, protocol):
    header = runtime / "ext-data-control-client.h"
    code = runtime / "ext-data-control.c"
    binary = runtime / "selection-fixture"
    subprocess.run(["wayland-scanner", "client-header", protocol, header], check=True)
    subprocess.run(["wayland-scanner", "private-code", protocol, code], check=True)
    flags = subprocess.check_output(["pkg-config", "--cflags", "--libs", "wayland-client"], text=True)
    subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-I", runtime,
                    Path(__file__).with_suffix(".c"), code, *shlex.split(flags), "-o", binary], check=True)
    return binary


def until(sample, match, seconds=4):
    deadline = time.monotonic() + seconds
    while True:
        value = sample()
        if match(value) or time.monotonic() >= deadline:
            return value
        time.sleep(0.02)


def check_selections(key, gesture, fixture):
    def read_selection(kind):
        result = subprocess.run([fixture, kind], capture_output=True, timeout=2)
        if result.returncode == 3:
            return b""
        if result.returncode != 0:
            raise RuntimeError(result.stderr.decode().strip() or f"cannot read {kind}")
        return result.stdout

    gesture("drag:570,400:650,400")
    clipboard = until(lambda: read_selection("clipboard"), bool)
    if not re.fullmatch(rb"X{2,}", clipboard):
        raise Violation(f"drag release did not publish the clipboard selection: {clipboard!r}")
    if until(lambda: read_selection("primary"), bool) != clipboard:
        raise Violation("drag release did not publish the primary selection")
    key("ctrl+a")
    if until(lambda: read_selection("clipboard"), lambda data: data == SENTINEL) != SENTINEL:
        raise Violation("OSC 52 did not replace the standard clipboard")
    if read_selection("primary") != clipboard:
        raise Violation("replacing the clipboard changed primary selection")
    return clipboard


def check_mouse(key, gesture, runtime, primary):
    pty = runtime / "pty"
    expected = START_PASTE + primary + END_PASTE
    gesture("middle:650,400")
    actual = until(pty.read_bytes, lambda data: data.endswith(END_PASTE))
    if actual != expected:
        raise Violation(f"middle click did not paste primary text only: {actual!r}")
    key("ctrl+b")
    if not until((runtime / "tracking").is_file, bool):
        raise RuntimeError("the PTY did not enable mouse tracking")
    gesture("middle:650,400")
    # The next clipboard read cannot overtake a primary read accidentally started by the click.
    key("ctrl+shift+v")
    clipboard = START_PASTE + SENTINEL + END_PASTE
    actual = until(pty.read_bytes, lambda data: data.endswith(clipboard))
    if not actual.endswith(clipboard):
        raise Violation("Ctrl+Shift+V did not paste the standard clipboard")
    if not (actual.startswith(expected)
            and TRACKED_CLICK.fullmatch(actual[len(expected):-len(clipboard)])):
        raise Violation(f"tracked middle click did not reach the program: {actual[len(expected):]!r}")


def scenario(binary, protocol, runtime):
    fixture = build_fixture(runtime, protocol)
    shell = runtime / "recorder"
    shell.write_text(f"#!/bin/sh\nexec {shlex.join([sys.executable, __file__, 'record'])}\n")
    shell.chmod(0o700)
    config = Path(os.environ["XDG_CONFIG_HOME"]) / "sumie"
    config.mkdir()
    (config / "sumie.toml").write_text("[shell]\nintegration = false\n")
    present = runtime / "present.log"
    present.touch()
    env = dict(os.environ, SHELL=str(shell), SUMIE_PRESENT_LOG=str(present))

    injector = os.environ["SUMIE_NESTED_TYPE"]

    def key(chords):
        subprocess.run([injector, "--keys", chords], check=True, timeout=3)

    with subprocess.Popen([injector, "--pointer"], stdin=subprocess.PIPE,
                          stdout=subprocess.PIPE, text=True) as pointer:
        if pointer.stdout.readline() != "ready\n":
            raise RuntimeError("the fake pointer did not bind")

        def gesture(command):
            pointer.stdin.write(command + "\n")
            pointer.stdin.flush()
            if pointer.stdout.readline() != "ok\n":
                raise RuntimeError(f"the fake pointer did not send {command}")

        client = subprocess.Popen([binary], env=env, stdin=subprocess.DEVNULL)
        try:
            ready = runtime / "ready"
            if not until(lambda: ready.is_file() and "presented " in present.read_text(), bool, 8):
                raise RuntimeError("sumie did not present or the PTY fixture did not start")
            primary = check_selections(key, gesture, fixture)
            check_mouse(key, gesture, runtime, primary)
            key("ctrl+d")
            if client.wait(timeout=4) != 0:
                raise RuntimeError("sumie did not exit cleanly")
        finally:
            stop(client)
    print("PASS: drag published both selections; middle click pasted primary only; tracking reported the click")
    return 0


if __name__ == "__main__":
    runtime = Path(os.environ["XDG_RUNTIME_DIR"])
    if sys.argv[1:] == ["record"]:
        sys.exit(record(runtime))
    try:
        sys.exit(scenario(*sys.argv[1:], runtime))
    except Violation as error:
        print(f"FAIL: {error}")
        sys.exit(1)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"clipboard fixture: {error}", file=sys.stderr)
        sys.exit(2)
