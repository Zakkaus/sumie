#!/usr/bin/env python3
"""U4 native close and Quit routes on the terminal driver, run by check-gate.py as macos-routes.

usage: routes.py CASE BINARY [EVIDENCE_DIR]

Each case starts a fresh terminal runner like lifetime.py, whose shell ignores SIGHUP and runs
a long sleep, then closes or quits it the way a person does. Every case needs: the window
off screen before the reap; SIGHUP, SIGKILL at the 3 s grace period, then the reap; exit 0;
and nothing left in the child's process group afterwards. Quit cases also need exactly one
intercept and one grant, close none. Exit 0 on a pass, 1 on a violation, 2 if it cannot
check (no Accessibility permission, no Dock item, or a window over the close button).
  close-button  A CG click on the window's close button (AX finds it; the click is posted
                only while the runner is frontmost and its window is topmost there).
  cmd-q         Cmd+Q as `CGEvent` keys posted to the runner, through AppKit's key equivalent.
  menu-quit     An AX press on the application menu's Quit item.
  dock-quit     Quit from the runner's Dock item menu, through AX.
  minimized     Minimize, then Quit from the menu and again from the Dock: one intercept, a
                veto and one grant.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from lifetime import (HARD, CannotCheck, Runner, Screen, cgtool, drained, group_alive,  # noqa: E402
                      reap_errors, start_child)

COMMAND, Q = 0x37, 0x0C


def press_quit(runner, tool, route):
    """Quit through `route`; returns when the request was made."""
    pid = str(runner.process.pid)
    if route == "cmd-q":
        sent = time.monotonic()
        for line in (f"key {COMMAND} down command", f"key {Q} down command", f"key {Q} up command",
                     f"key {COMMAND} up -"):
            runner.send(line)
        return sent
    sent = time.monotonic()
    result = subprocess.run([tool, {"menu": "axquit", "dock": "dockquit"}[route], pid],
                            capture_output=True, text=True)
    if result.returncode:
        raise CannotCheck(f"{route}: {result.stdout.strip() or 'AX refused the press'}")
    return sent


def click_close(runner, tool):
    pid = str(runner.process.pid)
    point = subprocess.run([tool, "closebutton", pid], capture_output=True, text=True).stdout.split()
    if len(point) != 2:
        raise CannotCheck("no close button through AX (no Accessibility permission?)")
    if subprocess.run([tool, "front", pid], capture_output=True, text=True).stdout.strip() != pid:
        raise CannotCheck("the runner did not become frontmost")
    sent = time.monotonic()
    if subprocess.run([tool, "clickown", pid, *point], capture_output=True).returncode:
        raise CannotCheck("another window covers the close button")
    return sent


def common_errors(name, runner, screen, code, marks, result):
    errors = reap_errors(name, marks)
    if screen.offscreen is None or marks["reaped"] is None or screen.offscreen > marks["reaped"]:
        errors.append(f"{name}: the window was still on screen when the child was reaped")
    if code != 0:
        errors.append(f"{name}: exited {code}")
    if left := group_alive(runner.child):
        errors.append(f"{name}: {len(left)} processes of the child's group outlived sumie")
        os.killpg(runner.child, signal.SIGKILL)
    result["left"] = len(left)
    return errors


def phase_errors(name, phases, vetoes):
    if (phases.count("intercept"), phases.count("grant"), phases.count("veto")) != (1, 1, vetoes):
        return [f"{name}: phases {phases}, expected one intercept, {vetoes} vetoes and one grant"]
    return []


def case(name, runner, tool, result):
    start_child(runner)
    screen = Screen(tool, runner.process.pid)
    if name == "close-button":
        sent = click_close(runner, tool)
    elif name == "minimized":
        asked = time.monotonic()
        runner.send("minimize")
        if runner.wait_for("out", "runner occluded true", 5, asked) is None:
            raise CannotCheck("the window did not minimize")
        sent = press_quit(runner, tool, "menu")
        if runner.wait_for("err", "winit::quit: intercept", 5, sent) is None:
            return [f"{name}: the menu's Quit was not intercepted"]
        time.sleep(1.0)
        if runner.process.poll() is None:
            press_quit(runner, tool, "dock")
    else:
        sent = press_quit(runner, tool, name.removesuffix("-quit"))
    code, marks = drained(runner, result, sent, HARD["ordinary"])
    screen.polling = False
    result["offscreen"] = None if screen.offscreen is None else round(screen.offscreen - sent, 3)
    errors = common_errors(name, runner, screen, code, marks, result)
    phases = result["phases"]
    if name == "close-button" and phases:
        errors.append(f"{name}: the close ran Quit phases {phases}")
    elif name != "close-button":
        errors += phase_errors(name, phases, 1 if name == "minimized" else 0)
    if not errors:
        print(f"{name}: off screen {result['offscreen']:.3f} s, SIGKILL {marks['kill'] - sent:.3f} s, "
              f"reaped {marks['reaped'] - sent:.3f} s, exit 0 {runner.exited - sent:.3f} s after the request; "
              f"phases {phases}; no process left")
    return errors


CASES = ("close-button", "cmd-q", "menu-quit", "dock-quit", "minimized")


def main():
    if len(sys.argv) not in (3, 4) or sys.argv[1] not in CASES:
        print(__doc__, file=sys.stderr)
        return 2
    name, binary = sys.argv[1], sys.argv[2]
    evidence = Path(sys.argv[3]) if len(sys.argv) == 4 else None
    result = {"case": name, "load": os.getloadavg()}
    with tempfile.TemporaryDirectory(prefix="sumie-routes.") as scratch:
        try:
            tool = cgtool(scratch)
            runner = Runner(binary, str(Path(scratch) / "home"))
        except (OSError, subprocess.CalledProcessError) as error:
            print(f"routes: could not start: {error}", file=sys.stderr)
            return 2
        try:
            if runner.wait_for("out", "runner first", 30) is None:
                raise CannotCheck("no first frame")
            errors = case(name, runner, tool, result)
        except CannotCheck as error:
            print(f"routes: {name}: could not check: {error}", file=sys.stderr)
            return 2
        finally:
            runner.stop()
            result["stderr"] = [line for _, line in runner.err][-60:]
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / f"routes-{name}.json").write_text(json.dumps(result, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
