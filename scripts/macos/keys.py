#!/usr/bin/env python3
"""U9 keyboard and focus check on the terminal driver, run by check-gate.py.

usage: keys.py BINARY [EVIDENCE_DIR]

Starts the terminal runner like frames.py, then runs keyrec.py in its shell and posts real
`CGEvent` keys to the runner's PID, recording the bytes that reach the PTY. Exit 0 on a pass,
1 on a violation (including missing permission to post events), 2 if it cannot check.
  plain  a, Enter, Tab, Backspace, Left and Ctrl+C give 61 0d 09 7f 1b5b44 03; Cmd+C gives
         nothing.
  deckm  Up and Left give 1b4f41 1b4f44.
  kitty  A press of a, then focus moves to another window: one release, in the pane that had
         the press. After refocus, a's late repeat and release are dropped; b still types.
  focus  Mode 1004: a Shift tap (its ModifiersChanged) reports nothing; with Shift held, focus
         loss reports once (winit also sends ModifiersChanged(empty)), and refocus once.
"""
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time

# Importing frames must not leave bytecode in the worktree: the negative controls compare its
# status before and after.
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from frames import CannotCheck, Runner, build_tools  # noqa: E402

KEYREC = Path(__file__).resolve().parent / "keyrec.py"
# AppKit virtual key codes.
A, B, C, TAB, ENTER, BACKSPACE, LEFT, UP = 0x00, 0x0B, 0x08, 0x30, 0x24, 0x33, 0x7B, 0x7E
SHIFT, CONTROL, COMMAND = 0x38, 0x3B, 0x37
FOCUS_OUT, FOCUS_IN = "1b5b4f", "1b5b49"
SETTLE = 0.3


class Violation(Exception):
    pass


class Keys:
    def __init__(self, runner):
        self.runner = runner
        self.phases = {}

    def key(self, code, phase, mods="-"):
        asked = time.monotonic()
        self.runner.send(f"key {code} {phase} {mods}")
        _, fields = self.runner.wait_for("runner", "posted", "key", since=asked)
        if fields[3] != "true":
            raise Violation("keys: could not post a key: no permission to post events, or the "
                            "window is not key")
        time.sleep(0.02)

    def tap(self, code, mods="-"):
        self.key(code, "down", mods)
        self.key(code, "up", mods)

    def chord(self, modifier, name, code):
        self.key(modifier, "down", name)
        self.tap(code, name)
        self.key(modifier, "up")

    def focus(self, focused):
        asked = time.monotonic()
        self.runner.send("focus" if focused else "blur")
        self.runner.wait_for("runner", "focused", str(focused).lower(), since=asked)
        time.sleep(0.1)

    def start(self, name, modes):
        out = self.runner.home / f"keys-{name}"
        self.runner.send(f"type {shlex.quote(sys.executable)} -I {shlex.quote(str(KEYREC))} {modes} "
                         f"{shlex.quote(str(out))}")
        self.runner.wait_for_file(out.with_name(out.name + ".ready"))
        self.phases[name] = out
        return out

    def recorded(self, out):
        time.sleep(SETTLE)
        return "".join(out.read_text().split())

    def stop(self, name):
        out = self.phases[name]
        out.with_name(out.name + ".stop").touch()
        self.runner.wait_for_file(out.with_name(out.name + ".done"))
        return self.recorded(out)


def expect(name, got, want):
    if got != want:
        raise Violation(f"keys: {name}: recorded {got or 'nothing'} instead of {want}")
    print(f"keys: {name}: {want}")


def case_plain(keys):
    keys.start("plain", "plain")
    for code in (A, ENTER, TAB, BACKSPACE, LEFT):
        keys.tap(code)
    keys.chord(CONTROL, "control", C)
    keys.chord(COMMAND, "command", C)
    expect("plain", keys.stop("plain"), "610d097f1b5b4403")


def case_deckm(keys):
    keys.start("deckm", "deckm")
    keys.tap(UP)
    keys.tap(LEFT)
    expect("deckm", keys.stop("deckm"), "1b4f411b4f44")


def case_kitty(keys):
    out = keys.start("kitty", "kitty")
    keys.key(A, "down")
    expect("kitty press", keys.recorded(out), "1b5b393775")
    keys.focus(False)
    release = keys.recorded(out)[len("1b5b393775"):]
    keys.focus(True)
    keys.key(A, "repeat")
    keys.key(A, "up")
    keys.tap(B)
    expect("kitty release on focus loss", release, "1b5b39373b313a3375")
    expect("kitty after refocus", keys.stop("kitty")[len("1b5b3937751b5b39373b313a3375"):],
           "1b5b3938751b5b39383b313a3375")


def case_focus(keys):
    out = keys.start("focus", "focus")
    keys.key(SHIFT, "down", "shift")
    keys.key(SHIFT, "up")
    tapped = keys.recorded(out)
    keys.key(SHIFT, "down", "shift")
    keys.focus(False)
    lost = keys.recorded(out)[len(tapped):]
    keys.focus(True)
    gained = keys.stop("focus")[len(tapped) + len(lost):]
    errors = []
    if tapped:
        errors.append(f"keys: focus: a modifier change reported {tapped}")
    for change, report, got in (("loss", FOCUS_OUT, lost), ("gain", FOCUS_IN, gained)):
        count = got.count(report)
        print(f"keys: focus: {count} report(s) for one focus {change}: {got or 'nothing'}")
        if count != 1 or got != report * count:
            errors.append(f"keys: focus: {count} focus-{'out' if change == 'loss' else 'in'} reports "
                          f"for one focus {change} ({got or 'nothing'})")
    if errors:
        raise Violation("\n".join(errors))


CASES = (case_plain, case_deckm, case_kitty, case_focus)


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    binary = sys.argv[1]
    evidence = Path(sys.argv[2]) if len(sys.argv) == 3 else None
    errors = []
    with tempfile.TemporaryDirectory(prefix="sumie-keys.") as scratch:
        try:
            hook, _ = build_tools(Path(scratch), False)
            runner = Runner(binary, hook, str(Path(scratch) / "home"))
        except (OSError, subprocess.CalledProcessError) as error:
            print(f"keys: could not start: {error}", file=sys.stderr)
            return 2
        try:
            if not runner.first.wait(30):
                raise CannotCheck("no first frame")
            runner.wait_for("runner", "focused", "true", timeout=10)
            keys = Keys(runner)
            # Each phase ends its recorder, so the next starts at the shell prompt.
            for case in CASES:
                case(keys)
        except CannotCheck as error:
            print(f"keys: could not check: {error}", file=sys.stderr)
            return 2
        except Violation as error:
            errors += str(error).splitlines()
        finally:
            code = runner.close()
    if code != 0:
        errors.append(f"keys: the runner exited {code}")
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / "keys.json").write_text(json.dumps({"errors": errors, "load": os.getloadavg()}, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
