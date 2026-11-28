"""The macOS window gates: wake, keys, typed shell, pasteboard, frame slot, and close and Quit."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from gates.build import ROOT, CannotCheck, runner_binary

WAKE_MODES = ("default", "event-tracking", "modal-panel")
WAKE_LIMIT_NS = 100_000_000
WAKE_TIMEOUT = 120
# The terminal driver's frame-slot checks; scripts/macos/frames.py runs one case per gate.
FRAME_GATES = {"macos-resize": "resize", "macos-idle": "idle", "macos-flood": "flood",
               "macos-timeout": "timeout", "macos-throughput": "throughput",
               "macos-loading": "loading"}
FRAME_TIMEOUT = 300
# The close and Quit contract; scripts/macos/lifetime.py runs each case in a fresh process.
# Hosted CI only records macos-cycles' counts (SUMIE_TIMING=record).
LIFETIME_GATES = {"macos-close": ("close",), "macos-repeat": ("repeat",),
                  "macos-fatal": ("fatal", "fatal-quit"), "macos-system": ("system", "system-fatal"),
                  "macos-stuck": ("stuck", "stuck-system"), "macos-exit": ("exit", "cycle-quit"),
                  "macos-cycles": ("cycles",)}
LIFETIME_TIMEOUT = 120
# Close and Quit the way a person does; scripts/macos/routes.py needs Accessibility, so hosts
# without it set SUMIE_QUIT_ROUTES=skip.
ROUTE_CASES = ("close-button", "cmd-q", "menu-quit", "dock-quit", "minimized")
# 200 cycles, with repeated resource probes after each reap.
CYCLES_TIMEOUT = 900


def check_macos_wake():
    """A pipe write during each AppKit run-loop mode reaches the handler within 100 ms."""
    if sys.platform != "darwin":
        raise CannotCheck("the wake scenario needs the macOS window server")
    binary = runner_binary()
    with tempfile.TemporaryDirectory(prefix="sumie-wake.") as home:
        env = {**os.environ, "SUMIE_MACOS_CI_RUNNER": "wake", "HOME": home,
               "XDG_CONFIG_HOME": str(Path(home) / "config")}
        try:
            result = subprocess.run([binary], env=env, stdin=subprocess.DEVNULL, capture_output=True,
                                    text=True, timeout=WAKE_TIMEOUT)
        except subprocess.TimeoutExpired:
            print("macos-wake: the scenario timed out", file=sys.stderr)
            return 1
    print(result.stderr, end="", file=sys.stderr)
    legs = {}
    for line in result.stdout.splitlines():
        fields = line.split()
        if fields[:2] == ["runner", "wake"] and len(fields) == 6:
            legs[fields[2]] = fields[3:]
    errors = []
    if result.returncode or "runner wake done" not in result.stdout:
        errors.append(f"macos-wake: the scenario exited {result.returncode} without finishing")
    for mode in WAKE_MODES:
        if mode not in legs:
            errors.append(f"macos-wake: {mode}: no report")
            continue
        latency, arrived, inside = legs[mode]
        if latency == "none" or inside != "true":
            errors.append(f"macos-wake: {mode}: no wake inside the 300 ms interval")
        elif arrived != mode:
            errors.append(f"macos-wake: {mode}: the wake ran in the {arrived} mode")
        elif int(latency) > WAKE_LIMIT_NS:
            errors.append(f"macos-wake: {mode}: {int(latency) / 1e6:.1f} ms exceeds 100 ms")
        else:
            print(f"macos-wake: {mode}: write to handler {int(latency) / 1e6:.3f} ms")
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


def check_macos_frames(case):
    """One frame-slot case on the terminal driver; see scripts/macos/frames.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the frame-slot checks need the macOS window server")
    binary = runner_binary()
    try:
        return subprocess.run([sys.executable, ROOT / "scripts/macos/frames.py", case, binary],
                              timeout=FRAME_TIMEOUT).returncode
    except subprocess.TimeoutExpired:
        print(f"macos-{case}: the case timed out", file=sys.stderr)
        return 1


def check_macos_keys():
    """Keys and focus through the terminal driver to a recorder; see scripts/macos/keys.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the keyboard check needs the macOS window server")
    binary = runner_binary()
    try:
        return subprocess.run([sys.executable, ROOT / "scripts/macos/keys.py", binary],
                              timeout=FRAME_TIMEOUT).returncode
    except subprocess.TimeoutExpired:
        print("macos-keys: the check timed out", file=sys.stderr)
        return 1


def check_macos_shell():
    """Typed commands in the account's login shell, against the VT and the presented frames; see
    scripts/macos/shell.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the typed-shell check needs the macOS window server")
    binary = runner_binary()
    try:
        return subprocess.run([sys.executable, ROOT / "scripts/macos/shell.py", binary],
                              timeout=FRAME_TIMEOUT).returncode
    except subprocess.TimeoutExpired:
        print("macos-shell: the check timed out", file=sys.stderr)
        return 1


def check_macos_ime():
    """Input-method events and the real input method to a recorder; see scripts/macos/ime.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the input-method check needs the macOS window server")
    binary = runner_binary()
    try:
        return subprocess.run([sys.executable, ROOT / "scripts/macos/ime.py", binary],
                              timeout=FRAME_TIMEOUT).returncode
    except subprocess.TimeoutExpired:
        print("macos-ime: the check timed out", file=sys.stderr)
        return 1


def check_macos_pasteboard():
    """Copy and paste through the helpers on a named board; see scripts/macos/pasteboard.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the pasteboard check needs the macOS window server")
    binary = runner_binary()
    try:
        return subprocess.run([sys.executable, ROOT / "scripts/macos/pasteboard.py", binary],
                              timeout=FRAME_TIMEOUT).returncode
    except subprocess.TimeoutExpired:
        print("macos-pasteboard: the check timed out", file=sys.stderr)
        return 1


def check_macos_routes():
    """Close button, Cmd+Q, menu, Dock and repeated Quit while minimized; see scripts/macos/routes.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the close and Quit routes need the macOS window server")
    if os.environ.get("SUMIE_QUIT_ROUTES") == "skip":
        print("macos-routes: skipped (SUMIE_QUIT_ROUTES=skip: no Accessibility on this host)")
        return 0
    binary = runner_binary()
    codes = []
    for case in ROUTE_CASES:
        try:
            codes.append(subprocess.run([sys.executable, ROOT / "scripts/macos/routes.py", case, binary],
                                        timeout=LIFETIME_TIMEOUT).returncode)
        except subprocess.TimeoutExpired:
            print(f"macos-routes: {case}: the case timed out", file=sys.stderr)
            codes.append(1)
    return 2 if 2 in codes else int(any(codes))


def check_macos_lifetime(cases):
    """Close and Quit cases on the terminal driver; see scripts/macos/lifetime.py."""
    if sys.platform != "darwin":
        raise CannotCheck("the close and Quit checks need the macOS window server")
    binary = runner_binary()
    codes = []
    for case in cases:
        try:
            codes.append(subprocess.run([sys.executable, ROOT / "scripts/macos/lifetime.py", case, binary],
                                        timeout=CYCLES_TIMEOUT if case == "cycles" else LIFETIME_TIMEOUT
                                        ).returncode)
        except subprocess.TimeoutExpired:
            print(f"macos-{case}: the case timed out", file=sys.stderr)
            codes.append(1)
    return 2 if 2 in codes else int(any(codes))
