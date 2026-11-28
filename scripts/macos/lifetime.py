#!/usr/bin/env python3
"""U10 close and Quit checks on the terminal driver, run by check-gate.py.

usage: lifetime.py CASE BINARY [EVIDENCE_DIR]

Each case starts a fresh `SUMIE_MACOS_CI_RUNNER=terminal` process with a throwaway HOME,
/bin/sh as the login shell and RUST_LOG=debug, and makes the shell ignore SIGHUP
(`trap '' HUP; sleep 1000`). Exit 0 on a pass, 1 on a violation, 2 if it cannot check.
  close         The red button: the window leaves CGWindowList's on-screen set before the
                reap; then SIGHUP, SIGKILL at the 3 s grace period, the reap with its exit
                watch removed, exit 0.
  repeat        Quit twice: one grant, one veto, SIGKILL 3 s after the first Quit.
  fatal         An injected error with the child alive: it is killed at the grace period and
                reaped, the process exits 1 with the original diagnostic, and no Quit runs.
  fatal-quit    The same error while an ordinary Quit drains: exit 1 and no grant (no
                second `terminate:`).
  system        A quit Apple event with a log-out reason: NSTerminateLater, then the reply
                within 4 s of the request, exit 0.
  system-fatal  The injected error while that reply is owed: logged, then the reply, exit 0.
  stuck         The main thread stalls holding stderr's lock: exit 70 within 10 s of Quit.
  stuck-system  The same after a system quit: exit 0 within 6 s.
  exit          The last shell runs `exit`: the window closes and the process exits 0.
  cycle-quit    Quit while a cycle reaps: AppKit grants Quit and the runner exits 0.
  cycles        200 window open/close cycles in one process. Within 6 s of each close the
                window drains, then each count settles at or below its peak in the first 10 cycles.
                Resource checks are recorded only under SUMIE_TIMING=record.
SUMIE_LIFETIME_TRIGGER picks how Close or Quit enters AppKit: `runner` (the default:
performClose:, terminate:), or, on the owner's Mac with Accessibility, `red` (a CG click on
the close button), `cmdq` (Cmd+Q through the HID tap), `ax` (AX press on the Quit item) and
`dock` (Quit from the Dock item's menu through AX).
"""
import json
import os
from pathlib import Path
import re
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

SCRIPTS = Path(__file__).resolve().parent
CONFIG = '[font]\nfamily = "Menlo"\n\n[cursor]\nblink = false\n'
GRACE = 3.0
HARD = {"ordinary": 10.0, "system": 6.0}
REPLY = 4.0
# Log lines land on stderr a few ms after the event; timers fire within a few ms.
SLACK = 0.5
# The design book's cleanup bound: a closed window's resources are back within 6 s.
CLEANUP = 6.0
CYCLES, WARMUP = int(os.environ.get("SUMIE_LIFETIME_CYCLES", "200")), 10
RECORD = os.environ.get("SUMIE_TIMING") == "record"
TRIGGER = os.environ.get("SUMIE_LIFETIME_TRIGGER", "runner")
REGIONS = {"iosurface_regions": "IOSurface", "ioaccelerator_regions": "IOAccelerator (graphics)"}
COUNTS = ("fds", "threads", "children", "zombies", *REGIONS)
MARKS = {
    "hup": "sent SIGHUP",
    "kill": "sending SIGKILL",
    "reaped": "reaped a closing child and removed its exit watch",
    "draining": "draining reason=",
    "after-fatal": "replying to the system quit after a fatal error",
}
PHASE = re.compile(r"winit::quit: (\w+)")


class CannotCheck(Exception):
    pass


class Runner:
    def __init__(self, binary, home):
        self.home = Path(home)
        config = self.home / "config"
        (config / "sumie").mkdir(parents=True)
        state = Path(home) / "Library/Application Support/sumie"  # the first-run hint takes a key
        state.mkdir(parents=True)
        (state / "first-run").touch()
        (config / "sumie/sumie.toml").write_text(CONFIG)
        env = {**os.environ, "SUMIE_MACOS_CI_RUNNER": "terminal", "HOME": home, "XDG_CONFIG_HOME": str(config),
               "SHELL": "/bin/sh", "RUST_LOG": "debug"}
        self.process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, bufsize=0, env=env)
        self.lock = threading.Lock()
        self.out, self.err = [], []
        self.exited = None
        self.child = None
        for stream, lines in ((self.process.stdout, self.out), (self.process.stderr, self.err)):
            threading.Thread(target=self.read, args=(stream, lines), daemon=True).start()
        self.waiter = threading.Thread(target=self.wait, daemon=True)
        self.waiter.start()

    def wait(self):
        self.process.wait()
        self.exited = time.monotonic()

    def read(self, stream, lines):
        for raw in stream:
            with self.lock:
                lines.append((time.monotonic(), raw.decode(errors="replace").rstrip("\n")))

    def send(self, command):
        try:
            self.process.stdin.write((command + "\n").encode())
        except BrokenPipeError:
            pass

    def lines(self, which, needle, since=0.0):
        with self.lock:
            return [(at, line) for at, line in (self.out if which == "out" else self.err)
                    if at >= since and needle in line]

    def first(self, which, needle, since=0.0):
        found = self.lines(which, needle, since)
        return found[0][0] if found else None

    def wait_for(self, which, needle, timeout, since=0.0):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            at = self.first(which, needle, since)
            if at is not None:
                return at
            if self.process.poll() is not None:
                time.sleep(0.05)
                return self.first(which, needle, since)
            time.sleep(0.01)
        return None

    def phases(self):
        with self.lock:
            return [match[1] for _, line in self.err if (match := PHASE.search(line))]

    def wait_exit(self, timeout):
        self.waiter.join(max(timeout, 0.1))
        # Let the readers take the last lines.
        time.sleep(0.1)
        return None if self.waiter.is_alive() else self.process.returncode

    def stop(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        if self.child is not None:
            kill_group(self.child)


def cgtool(scratch):
    tool = Path(scratch) / "cgtool"
    subprocess.run(["xcrun", "--sdk", "macosx", "swiftc", "-O", SCRIPTS.parent / "macos-spike/cgtool.swift",
                    "-o", tool], check=True, capture_output=True)
    return tool


class Screen:
    """Polls CGWindowList until the runner window is off screen."""

    def __init__(self, tool, pid):
        self.tool, self.pid = tool, pid
        self.offscreen = None
        self.polling = True
        threading.Thread(target=self.poll, daemon=True).start()

    def state(self):
        out = subprocess.run([self.tool, "bounds", str(self.pid)], capture_output=True, text=True).stdout.split()
        return len(out) == 5 and out[4] == "1"

    def poll(self):
        # A query that sees the window gone may have started before it left, so the time it
        # returned is the earliest the window is known to be off screen.
        while self.polling and self.offscreen is None:
            if not self.state():
                self.offscreen = time.monotonic()
            time.sleep(0.02)


def trigger(runner, tool, action):
    """Close or Quit through the chosen route; returns when the action started."""
    pid = str(runner.process.pid)
    route = TRIGGER
    if route == "runner":
        sent = time.monotonic()
        runner.send("close" if action == "close" else "quit")
    elif action == "close" and route == "red":
        point = subprocess.run([tool, "closebutton", pid], capture_output=True, text=True).stdout.split()
        if len(point) != 2:
            raise CannotCheck("no close button through AX")
        if subprocess.run([tool, "front", pid], capture_output=True, text=True).stdout.strip() != pid:
            raise CannotCheck("the runner did not become frontmost")
        sent = time.monotonic()
        if subprocess.run([tool, "clickown", pid, *point], capture_output=True).returncode:
            raise CannotCheck("another window covers the close button")
    elif action == "quit" and route in ("cmdq", "ax", "dock"):
        if route == "cmdq" and subprocess.run([tool, "front", pid], capture_output=True, text=True).stdout.strip() != pid:
            raise CannotCheck("the runner did not become frontmost")
        sent = time.monotonic()
        result = subprocess.run([tool, {"cmdq": "cmdq", "ax": "axquit", "dock": "dockquit"}[route], pid],
                                capture_output=True, text=True)
        if result.returncode:
            raise CannotCheck(f"{route}: {result.stdout.strip()}")
    else:
        raise CannotCheck(f"SUMIE_LIFETIME_TRIGGER={route} cannot {action}")
    return sent


def start_child(runner):
    """The shell ignores SIGHUP and runs a long sleep in its own process group."""
    pidfile = runner.home / "child.pid"
    runner.send(f"type trap '' HUP; echo $$ >{pidfile}; sleep 1000")
    end = time.monotonic() + 10
    while not pidfile.exists() or not pidfile.read_text().strip():
        if time.monotonic() > end:
            raise CannotCheck("the shell did not start the HUP-ignoring child")
        time.sleep(0.02)
    time.sleep(0.3)
    runner.child = int(pidfile.read_text())
    return runner.child


def group_alive(pgid):
    out = subprocess.run(["ps", "-A", "-o", "pgid=,pid="], capture_output=True, text=True).stdout.split("\n")
    return [int(f[1]) for line in out if len(f := line.split()) == 2 and int(f[0]) == pgid]


def kill_group(pgid):
    if group_alive(pgid):
        os.killpg(pgid, signal.SIGKILL)


def drained(runner, result, start, limit):
    """Wait for the exit and gather the marks the cases compare."""
    code = runner.wait_exit(limit + SLACK - (time.monotonic() - start))
    marks = {name: runner.first("err", needle, start) for name, needle in MARKS.items()}
    result.update(exit=code, marks={k: None if v is None else round(v - start, 3) for k, v in marks.items()},
                  phases=runner.phases(), exited=None if code is None else round(runner.exited - start, 3))
    return code, marks


def reap_errors(name, marks):
    """SIGHUP when draining starts, SIGKILL at the grace period, then the reap."""
    errors = []
    hup, kill, reaped = marks["hup"], marks["kill"], marks["reaped"]
    if hup is None:
        errors.append(f"{name}: no SIGHUP was sent to the closing child")
    elif kill is None:
        errors.append(f"{name}: no SIGKILL at the {GRACE:.0f} s grace period")
    elif not GRACE - 0.1 <= kill - hup <= GRACE + SLACK:
        errors.append(f"{name}: SIGKILL {kill - hup:.2f} s after SIGHUP (grace {GRACE:.0f} s)")
    if kill is not None and (reaped is None or reaped < kill):
        errors.append(f"{name}: the child was not reaped after SIGKILL")
    return errors


def case_close(runner, tool, result):
    start_child(runner)
    screen = Screen(tool, runner.process.pid)
    sent = trigger(runner, tool, "close")
    code, marks = drained(runner, result, sent, HARD["ordinary"])
    screen.polling = False
    errors = reap_errors("close", marks)
    offscreen = screen.offscreen
    result["offscreen"] = None if offscreen is None else round(offscreen - sent, 3)
    if marks["reaped"] is not None and (offscreen is None or offscreen > marks["reaped"]):
        errors.append("close: the window was still on screen when the child was reaped")
    if code != 0:
        errors.append(f"close: exited {code}")
    if offscreen is not None and marks["reaped"] is not None:
        print(f"close: off screen {offscreen - sent:.3f} s, SIGKILL {marks['kill'] - sent:.3f} s, "
              f"reaped {marks['reaped'] - sent:.3f} s, exit {runner.exited - sent:.3f} s after the close")
    return errors


def case_repeat(runner, tool, result):
    start_child(runner)
    screen = Screen(tool, runner.process.pid)
    first = trigger(runner, tool, "quit")
    if runner.wait_for("err", "winit::quit: intercept", 5, first) is None:
        return ["repeat: the first Quit was not intercepted"]
    time.sleep(1.5)
    if runner.process.poll() is None:
        trigger(runner, tool, "quit")
    code, marks = drained(runner, result, first, HARD["ordinary"])
    screen.polling = False
    phases = result["phases"]
    errors = reap_errors("repeat", marks)
    if (phases.count("intercept"), phases.count("grant")) != (1, 1) or "veto" not in phases:
        errors.append(f"repeat: phases {phases}, expected one intercept, a veto and one grant")
    if screen.offscreen is None or marks["reaped"] is None or screen.offscreen > marks["reaped"]:
        errors.append("repeat: the window was still on screen when the child was reaped")
    if code != 0:
        errors.append(f"repeat: exited {code}")
    if not errors:
        print(f"repeat: phases {phases}; SIGKILL {marks['kill'] - marks['hup']:.3f} s after SIGHUP, "
              f"exit {runner.exited - first:.3f} s after the first Quit")
    return errors


def fatal_errors(name, runner, code, marks, result):
    errors = reap_errors(name, marks)
    if code != 1:
        errors.append(f"{name}: exited {code}, expected 1")
    if not runner.lines("err", "sumie: injected fault"):
        errors.append(f"{name}: the original diagnostic was not printed")
    if not errors:
        print(f"{name}: exit 1 with `sumie: injected fault` {result['exited']:.3f} s after the error; "
              f"SIGKILL {marks['kill'] - marks['hup']:.3f} s after SIGHUP; phases {result['phases']}")
    return errors


def case_fatal(runner, _tool, result):
    start_child(runner)
    sent = time.monotonic()
    runner.send("fail")
    code, marks = drained(runner, result, sent, HARD["ordinary"])
    errors = fatal_errors("fatal", runner, code, marks, result)
    if result["phases"]:
        errors.append(f"fatal: a Quit ran after the error: {result['phases']}")
    return errors


def case_fatal_quit(runner, tool, result):
    start_child(runner)
    sent = trigger(runner, tool, "quit")
    if runner.wait_for("err", MARKS["hup"], 5, sent) is None:
        return ["fatal-quit: the Quit did not start draining"]
    runner.send("fail")
    code, marks = drained(runner, result, sent, HARD["ordinary"])
    errors = fatal_errors("fatal-quit", runner, code, marks, result)
    if "grant" in result["phases"]:
        errors.append(f"fatal-quit: terminate: was granted after the error: {result['phases']}")
    return errors


def system_errors(name, runner, code, marks, sent, result):
    errors = []
    phases = result["phases"]
    reply = runner.first("err", "winit::quit: reply", sent)
    result["reply"] = None if reply is None else round(reply - sent, 3)
    if "later" not in phases:
        errors.append(f"{name}: the system quit was not answered NSTerminateLater: {phases}")
    if reply is None or reply - sent > REPLY:
        errors.append(f"{name}: no reply within {REPLY:.0f} s of the system quit")
    if code != 0:
        errors.append(f"{name}: exited {code}")
    if code is not None and runner.exited - sent > REPLY + SLACK:
        errors.append(f"{name}: exited {runner.exited - sent:.2f} s after the system quit (reply limit {REPLY:.0f} s)")
    return errors


def case_system(runner, _tool, result):
    start_child(runner)
    sent = time.monotonic()
    runner.send("system-quit")
    code, marks = drained(runner, result, sent, HARD["system"])
    errors = system_errors("system", runner, code, marks, sent, result) + reap_errors("system", marks)
    if not errors:
        print(f"system: phases {result['phases']}; SIGKILL {marks['kill'] - sent:.3f} s, reply {result['reply']:.3f} s, "
              f"exit {runner.exited - sent:.3f} s after the request")
    return errors


def case_system_fatal(runner, _tool, result):
    start_child(runner)
    sent = time.monotonic()
    runner.send("system-quit")
    if runner.wait_for("err", MARKS["hup"], 3, sent) is None:
        return ["system-fatal: the system quit did not start draining"]
    runner.send("fail")
    code, marks = drained(runner, result, sent, HARD["system"])
    errors = system_errors("system-fatal", runner, code, marks, sent, result)
    if marks["after-fatal"] is None:
        errors.append("system-fatal: the error was not logged before the reply")
    if not errors:
        print(f"system-fatal: the error logged, then the reply {result['reply']:.3f} s after the request, exit 0")
    return errors


def stuck(name, runner, tool, result, system):
    start_child(runner)
    sent = time.monotonic()
    if system:
        runner.send("system-quit")
    else:
        sent = trigger(runner, tool, "quit")
    # The drain's clock starts when it logs `draining`, after a Dock or AX trigger's own delay.
    began = runner.wait_for("err", MARKS["draining"], 5, sent)
    if began is None or runner.wait_for("err", 'stage="reaping"', 3, sent) is None:
        return [f"{name}: draining did not reach the reaping stage"]
    runner.send("stall")
    if runner.wait_for("out", "runner stalling", 1, began) is None:
        return [f"{name}: the runner did not stall holding stderr"]
    limit = HARD["system" if system else "ordinary"]
    code, marks = drained(runner, result, began, limit)
    if code is None:
        return [f"{name}: still running {limit + SLACK:.1f} s after the quit"]
    errors = []
    expected = 0 if system else 70
    if code != expected:
        errors.append(f"{name}: exited {code}, expected {expected}")
    if marks["reaped"] is not None:
        errors.append(f"{name}: a reap was reported while the main thread was stuck")
    if not errors:
        print(f"{name}: exit {code} {runner.exited - began:.3f} s after draining began with stderr locked")
    return errors


def case_stuck(runner, tool, result):
    return stuck("stuck", runner, tool, result, False)


def case_stuck_system(runner, tool, result):
    return stuck("stuck-system", runner, tool, result, True)


def case_exit(runner, tool, result):
    screen = Screen(tool, runner.process.pid)
    time.sleep(0.5)
    sent = time.monotonic()
    runner.send("type exit")
    code, marks = drained(runner, result, sent, HARD["ordinary"])
    screen.polling = False
    errors = []
    if code != 0:
        errors.append(f"exit: exited {code}")
    if screen.offscreen is None:
        errors.append("exit: the window never left the screen")
    if marks["kill"] is not None or marks["draining"] is None:
        errors.append(f"exit: unexpected drain marks {result['marks']}")
    if not errors:
        print(f"exit: off screen {screen.offscreen - sent:.3f} s, exit {runner.exited - sent:.3f} s after `exit`")
    return errors


def resources(pid, pending, deadline):
    """Sample only the counts that have not settled; a failed probe yields None."""
    def read(*command):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return None
        try:
            result = subprocess.run(command, capture_output=True, text=True, timeout=remaining)
        except (OSError, subprocess.TimeoutExpired):
            return None
        return result.stdout.splitlines() if result.returncode == 0 else None

    if "fds" in pending:
        rows = read("lsof", "-n", "-P", "-p", str(pid), "-Ff")
        yield "fds", None if rows is None else {int(row[1:]) for row in rows if re.fullmatch(r"f[0-9]+", row)}
    if "threads" in pending:
        rows = read("ps", "-M", "-p", str(pid))
        yield "threads", len(rows) - 1 if rows and len(rows) > 1 else None
    if pending & {"children", "zombies"}:
        rows = read("ps", "-A", "-o", "ppid=,stat=")
        children = None if not rows else [stat for ppid, stat in map(str.split, rows) if int(ppid) == pid]
        if "children" in pending:
            yield "children", None if children is None else len(children)
        if "zombies" in pending:
            yield "zombies", None if children is None else sum(stat.startswith("Z") for stat in children)
    if pending & REGIONS.keys():
        rows = read("vmmap", "-summary", str(pid))
        for key, name in REGIONS.items():
            if key in pending:
                # A type with no regions has no row.
                yield key, None if not rows else next((int(row.split()[-1]) for row in rows
                                                       if re.match(rf"{re.escape(name)}\s+\d", row)), 0)


def settled(pid, baseline, closed):
    """Freeze each count after two valid samples agree, or both meet its baseline."""
    deadline = closed + CLEANUP
    pending = set(COUNTS)
    previous, sample, regions, fds = {}, {}, [], None
    while pending and time.monotonic() < deadline:
        for key, value in resources(pid, pending, deadline):
            at = time.monotonic()
            if key in REGIONS:
                regions.append((round(at - closed, 3), key, value))
            if value is None or at > deadline:
                previous.pop(key, None)
                continue
            if key == "fds":
                fds, value = value, len(value)
            sample[key] = value
            prior = previous.get(key)
            if prior is not None and (value == prior if baseline is None else max(prior, value) <= baseline[key]):
                pending.remove(key)
            previous[key] = value
        if pending:
            time.sleep(max(0, min(0.25, deadline - time.monotonic())))
    return sample, pending, fds, regions


def case_cycle_quit(runner, _tool, result):
    start_child(runner)
    closed = time.monotonic()
    runner.send("cycle")
    if runner.wait_for("err", 'stage="reaping"', 2, closed) is None:
        return ["cycle-quit: the cycle did not start reaping"]
    runner.send("quit")
    code, marks = drained(runner, result, closed, HARD["ordinary"])
    if code != 0 or "grant" not in result["phases"] or runner.first("out", "runner cycled ", closed):
        return ["cycle-quit: Quit did not finish the drain through AppKit"]
    print("cycle-quit: AppKit granted Quit during the drain; exit 0")
    return reap_errors("cycle-quit", marks)


def case_cycles(runner, _tool, result):
    """Compare each drained cycle with the warm-up cycles using one deadline from its close."""
    baseline, baseline_fds, worst, seconds, errors = None, None, {}, [], []
    for number in range(1, CYCLES + 1):
        if number > 1:
            asked = time.monotonic()
            runner.send("open")
            if runner.wait_for("out", "runner first", 15, asked) is None:
                return [f"cycles: cycle {number} did not open and present"]
        closed = time.monotonic()
        runner.send("cycle")
        reaped = runner.wait_for("out", f"runner cycled {number}", closed + CLEANUP - time.monotonic(), closed)
        if reaped is None:
            return [f"cycles: cycle {number} was not drained within {CLEANUP:.0f} s of its close"]
        seconds.append(reaped - closed)
        sample, pending, fds, regions = settled(runner.process.pid, None if number <= WARMUP else baseline, closed)
        if pending:
            for key in sorted(pending):
                value = sample.get(key)
                if number > WARMUP and value is not None and value > baseline[key]:
                    errors.append(f"cycles: {key} {baseline[key]} -> {value} after cycle {number}")
                else:
                    errors.append(f"cycles: could not settle {key} after cycle {number}")
            added = removed = None
            if baseline_fds is not None and fds is not None:
                added, removed = sorted(fds - baseline_fds), sorted(baseline_fds - fds)
            print(f"cycles: cycle {number} timeout: FD difference added={added}, removed={removed}; "
                  f"regions (seconds since close, metric, count)={regions}", file=sys.stderr)
            result.update(failed=sample, cycle=number, fd_added=added, fd_removed=removed, regions=regions)
            break
        if number <= WARMUP:
            baseline, baseline_fds = {key: max(value, (baseline or {}).get(key, value)) for key, value in sample.items()}, fds
        else:
            for key, value in sample.items():
                worst[key] = max(worst.get(key, value), value)
    result.update(baseline=baseline, worst=worst, drain_max=max(seconds), drain_median=statistics.median(seconds))
    print(f"cycles: {len(seconds)} of {CYCLES} drained, longest {max(seconds):.3f} s, "
          f"median {statistics.median(seconds):.3f} s from close to reaped")
    print(f"{'':24}{'warm-up max':>14}{'worst later':>14}")
    for key in baseline or {}:
        print(f"{key:24}{baseline[key]!s:>14}{worst.get(key, '-')!s:>14}")
    if RECORD:
        for error in errors:
            print(f"{error} (recorded only)")
        return []
    return errors


CASES = {"close": case_close, "repeat": case_repeat, "fatal": case_fatal, "fatal-quit": case_fatal_quit,
         "system": case_system, "system-fatal": case_system_fatal, "stuck": case_stuck,
         "stuck-system": case_stuck_system, "exit": case_exit, "cycle-quit": case_cycle_quit, "cycles": case_cycles}


def main():
    if len(sys.argv) not in (3, 4) or sys.argv[1] not in CASES:
        print(__doc__, file=sys.stderr)
        return 2
    name, binary = sys.argv[1], sys.argv[2]
    evidence = Path(sys.argv[3]) if len(sys.argv) == 4 else None
    result = {"case": name, "trigger": TRIGGER, "load": os.getloadavg()}
    with tempfile.TemporaryDirectory(prefix="sumie-lifetime.") as scratch:
        try:
            tool = cgtool(scratch)
            runner = Runner(binary, str(Path(scratch) / "home"))
        except (OSError, subprocess.CalledProcessError) as error:
            print(f"lifetime: could not start: {error}", file=sys.stderr)
            return 2
        try:
            if runner.wait_for("out", "runner first", 30) is None:
                raise CannotCheck("no first frame")
            errors = CASES[name](runner, tool, result)
        except CannotCheck as error:
            print(f"lifetime: {name}: could not check: {error}", file=sys.stderr)
            return 2
        finally:
            runner.stop()
            result["stderr"] = [line for _, line in runner.err][-60:]
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / f"lifetime-{name}-{TRIGGER}.json").write_text(json.dumps(result, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
