#!/usr/bin/env python3
"""U9 keyboard and focus check on the terminal driver, run by check-gate.py.

usage: keys.py BINARY [EVIDENCE_DIR]

Starts the terminal runner like frames.py, then runs keyrec.py in its shell and posts real
`CGEvent` keys to the runner's PID, recording the bytes that reach the PTY. Exit 0 on a pass,
1 on a violation (including missing permission to post events), 2 if it cannot check.
  plain    a, Enter, Tab, Backspace, Left and Ctrl+C give 61 0d 09 7f 1b5b44 03; Cmd+C gives
           nothing.
  deckm    Up and Left give 1b4f41 1b4f44.
  kitty    A press of a, then focus moves to another window: one release, in the pane that had
           the press. After refocus, a's late repeat and release are dropped; b still types.
  focus    Mode 1004: a Shift tap (its ModifiersChanged) reports nothing; with Shift held, focus
           loss reports once (winit also sends ModifiersChanged(empty)), and refocus once.
  command  Under Kitty flags 11, which report every key and release: Command itself, Cmd+C
           (a shortcut with nothing to copy) and Cmd+K (unbound) send nothing for their press,
           repeat or release, even when Command is lifted before the key; a still types.
  option   Left Option is Meta once: a and 1 give 1b61 1b31 (1 does not select a tab). Right
           Option types the layout's characters: a gives c3a5 (å on Latin layouts), and 1 one
           character without ESC (¡ on US, 1 on some layouts), not a tab switch.
  links    With https://a.example/one and https://bb.example/two on row 0: Cmd+click on the
           first opens it; a plain click, and a Cmd press dragged onto the second and back to
           its own cell, open nothing; Cmd+click on the second opens it. The runner's opener stub prints the URI's
           length, so no browser starts; no mouse bytes reach the program.
  tracking Mode 1002 with SGR reports: a click in one cell gives its press and release; a drag
           to another cell selects and reports nothing; a left Option drag reaches the program
           as press, motion and release without the Alt bit.
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
A, B, C, K, DIGIT1 = 0x00, 0x0B, 0x08, 0x28, 0x12
TAB, ENTER, BACKSPACE, LEFT, UP = 0x30, 0x24, 0x33, 0x7B, 0x7E
SHIFT, CONTROL, COMMAND, LEFT_OPTION, RIGHT_OPTION = 0x38, 0x3B, 0x37, 0x3A, 0x3D
KITTY_A = "1b5b3937751b5b39373b313a3375"
LINKS = ("https://a.example/one", "https://bb.example/two")
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

    def mouse(self, phase, cell, mods="-"):
        asked = time.monotonic()
        self.runner.send(f"mouse {phase} {cell[0]} {cell[1]} {mods}")
        _, fields = self.runner.wait_for("runner", "posted", "mouse", since=asked)
        if fields[3] != "true":
            raise Violation("keys: could not post a mouse event: the window is not key or shows no pane")
        time.sleep(0.05)

    def click(self, cell, mods="-"):
        self.mouse("down", cell, mods)
        self.mouse("up", cell, mods)
        # Past the double-click interval, so the next press starts a new gesture.
        time.sleep(0.7)

    def drag(self, start, end, mods="-"):
        self.drag_path([start, ((start[0] + end[0]) // 2, start[1]), end], mods)

    def drag_path(self, cells, mods="-"):
        self.mouse("down", cells[0], mods)
        for cell in cells[1:]:
            self.mouse("drag", cell, mods)
        self.mouse("up", cells[-1], mods)
        time.sleep(0.7)

    def focus(self, focused):
        asked = time.monotonic()
        self.runner.send("focus" if focused else "blur")
        self.runner.wait_for("runner", "focused", str(focused).lower(), since=asked)
        time.sleep(0.1)

    def start(self, name, modes, before=""):
        out = self.runner.home / f"keys-{name}"
        self.runner.send(f"type {before}{shlex.quote(sys.executable)} -I {shlex.quote(str(KEYREC))} {modes} "
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
    print(f"keys: {name}: {want or 'nothing'}")


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
    expect("kitty after refocus", keys.stop("kitty")[len(KITTY_A):],
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


def case_command(keys):
    keys.start("command", "kitty")
    keys.key(COMMAND, "down", "command")
    for code in (C, K):
        keys.key(code, "down", "command")
        keys.key(code, "repeat", "command")
    keys.key(C, "up", "command")
    # Command lifted before K: K's release still belongs to the shortcut table.
    keys.key(COMMAND, "up")
    keys.key(K, "up")
    keys.tap(A)
    expect("command", keys.stop("command"), KITTY_A)


def case_option(keys):
    keys.start("option", "plain")
    for side, code in ((LEFT_OPTION, "loption"), (RIGHT_OPTION, "roption")):
        keys.key(side, "down", code)
        keys.tap(A, code)
        keys.tap(DIGIT1, code)
        keys.key(side, "up")
    got = keys.stop("option")
    meta, composed = "1b611b31", "c3a5"
    rest = bytes.fromhex(got[len(meta) + len(composed):]) if got.startswith(meta + composed) else b""
    try:
        digit = rest.decode()
    except UnicodeDecodeError:
        digit = ""
    if len(digit) != 1 or digit == "\x1b":
        raise Violation(f"keys: option: recorded {got or 'nothing'} instead of {meta}{composed} and one "
                        "character for right Option+1")
    print(f"keys: option: left Option {meta}, right Option {composed} and {rest.hex()}")


def case_links(keys):
    shown = "printf '\\033[3J\\033[2J\\033[H%s  %s\\n' " + " ".join(LINKS) + "; "
    keys.start("links", "plain", before=shown)
    first, second = (3, 0), (len(LINKS[0]) + 4, 0)
    since = time.monotonic()
    keys.key(COMMAND, "down", "command")
    keys.click(first, "command")
    keys.key(COMMAND, "up")
    keys.click(first)
    keys.key(COMMAND, "down", "command")
    keys.drag_path([first, second, first], "command")
    keys.click(second, "command")
    keys.key(COMMAND, "up")
    time.sleep(SETTLE)
    opened = [int(fields[2]) for _, fields in keys.runner.matching("runner", "opened", since=since)]
    want = [len(LINKS[0]), len(LINKS[1])]
    if opened != want:
        raise Violation(f"keys: links: opened URIs of lengths {opened or 'none'} instead of {want}: only a "
                        "Command press and release on the same link opens it")
    print(f"keys: links: opened {want}; a plain click and a drag off the link and back opened nothing")
    expect("links bytes", keys.stop("links"), "")


def sgr(button, cell, final):
    return f"\x1b[<{button};{cell[0] + 1};{cell[1] + 1}{final}".encode().hex()


def case_tracking(keys):
    keys.start("tracking", "mouse", before="clear; ")
    row = 1
    start, end = (3, row), (9, row)
    keys.click(start)
    keys.drag(start, end)
    keys.drag(start, end, "loption")
    middle = ((start[0] + end[0]) // 2, row)
    want = (sgr(0, start, "M") + sgr(0, start, "m") + sgr(0, start, "M") + sgr(32, middle, "M")
            + sgr(32, end, "M") + sgr(0, end, "m"))
    expect("tracking", keys.stop("tracking"), want)


CASES = (case_plain, case_deckm, case_kitty, case_focus, case_command, case_option, case_links,
         case_tracking)


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
