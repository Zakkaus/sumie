#!/usr/bin/env python3
"""M1b-2 pasteboard check on the terminal driver, run by check-gate.py.

usage: pasteboard.py BINARY [EVIDENCE_DIR]

Starts the terminal runner like keys.py with SUMIE_MACOS_CI_PASTEBOARD naming a fresh board,
so its pasteboard helpers serve that board instead of the general one. The check reaches the
board through its own tool (pbfixture.m), runs keyrec.py in the shell and posts real `CGEvent`
keys: Cmd+V pastes and Cmd+C copies through the shared actions. Exit 0 on a pass, 1 on a
violation, 2 if it cannot check. Contents never appear in its output.
  paste      The board's string arrives as is, and framed whole in mode 2004.
  empty      A board without a string, or with an empty one, sends nothing; a later paste
             still arrives.
  oversized  A string over 1 MiB sends nothing; in mode 2004, one whose framing makes it
             1 MiB + 1 sends nothing, and one that fills the 1 MiB FIFO exactly arrives whole.
  stale      A paste whose provider answers after search opened and closed, and after the
             next paste was requested, sends nothing; that next paste, promised with other
             contents, arrives once and alone.
  stalled    A provider that never answers: nothing arrives, pings stay <= 100 ms, and within
             6.2 s of the request no helper is left; then a paste arrives again.
  copy       A mouse drag over row 0 selects it and its release leaves the sentinel; Cmd+C
             replaces the sentinel with the dragged cells.
  osc52      A focused OSC 52 write sets the board and an empty one clears it; without focus
             the board keeps its sentinel; a query gets no contents back.
After every case the general board's change count must be unchanged (it is never read).
"""
import base64
import json
import os
from pathlib import Path
import select
import shlex
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from frames import RECORD_TIMING, CannotCheck, Runner, build_tools  # noqa: E402
from keys import COMMAND, Keys, Violation  # noqa: E402

SCRIPTS = Path(__file__).resolve().parent
LIMIT = 1 << 20
C, V, F, B, ESCAPE = 0x08, 0x09, 0x03, 0x0B, 0x35
CONTENT = "sumie 墨 paste\tend"
START, END = "1b5b3230307e", "1b5b3230317e"
HELPER_TIMEOUT = 5.0
REAPED_BY = 6.2
PING_LIMIT_MS = 100.0


def framed(text):
    return START + text.encode().hex() + END


class Board:
    def __init__(self, tool, name):
        self.tool, self.name = str(tool), name

    def run(self, *args, text=None, board=None):
        return subprocess.run([self.tool, args[0], board or self.name, *args[1:]], capture_output=True,
                              input=None if text is None else text.encode(), timeout=10)

    def set(self, text):
        if self.run("set", text=text).returncode:
            raise CannotCheck("pbfixture could not set the board")

    def clear(self):
        self.run("clear")

    def get(self):
        result = self.run("get")
        return None if result.returncode == 3 else result.stdout.decode()

    def general_count(self):
        return int(self.run("count", board="general").stdout)

    def provide(self, wait, *texts):
        """Promise `texts` in turn; each request waits `wait` ms, forever for -1, or for SIGUSR1
        for `signal`."""
        # Unbuffered, so `await_asked` can wait on the pipe for each line.
        process = subprocess.Popen([self.tool, "provide", self.name, str(wait)], bufsize=0,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        process.stdin.write("\0".join(texts).encode())
        process.stdin.close()
        if process.stdout.readline().strip() != b"ready":
            raise CannotCheck("pbfixture could not promise the board")
        return process


def await_asked(provider, which, timeout=5.0):
    ready = select.select([provider.stdout], [], [], timeout)[0]
    if not ready or provider.stdout.readline().strip() != b"asked":
        raise CannotCheck(f"the provider was not asked for the {which} paste")


class Check:
    def __init__(self, runner, board):
        self.runner, self.board, self.keys = runner, board, Keys(runner)
        self.general = board.general_count()

    def chord(self, code):
        self.keys.key(COMMAND, "down", "command")
        self.keys.tap(code, "command")
        self.keys.key(COMMAND, "up")

    def paste(self):
        self.chord(V)

    def shell(self, command):
        self.runner.send(f"type {command}")
        time.sleep(0.5)

    def await_board(self, want, timeout=5.0):
        end = time.monotonic() + timeout
        got = self.board.get()
        while got != want and time.monotonic() < end:
            time.sleep(0.05)
            got = self.board.get()
        return got

    def general_unchanged(self, case):
        if self.board.general_count() != self.general:
            raise Violation(f"pasteboard: {case}: the general pasteboard changed (a copy elsewhere during "
                            "the check also counts)")


def expect(case, got, want, what):
    if got != want:
        raise Violation(f"pasteboard: {case}: recorded {len(got) // 2} bytes instead of {what} "
                        f"({len(want) // 2} bytes)")
    print(f"pasteboard: {case}: {what}")


def case_paste(check):
    check.board.set(CONTENT)
    for name, modes, want in (("paste", "plain", CONTENT.encode().hex()), ("framed", "paste", framed(CONTENT))):
        out = check.keys.start(name, modes)
        check.paste()
        check.keys.await_recorded(out, len(want))
        got = check.keys.stop(name)
        expect(name, got, want, "the board's string only" if modes == "plain" else "the string framed once")


def case_empty(check):
    out = check.keys.start("empty", "paste")
    for fill in (check.board.clear, lambda: check.board.set("")):
        fill()
        check.paste()
        time.sleep(1.0)
    check.board.set("after empty")
    check.paste()
    check.keys.await_recorded(out, len(framed("after empty")))
    got = check.keys.stop("empty")
    expect("empty", got, framed("after empty"), "nothing for no string and an empty one, then the next paste")


def case_oversized(check):
    out = check.keys.start("oversized", "paste")
    for length in (LIMIT + 1, LIMIT - 11):
        check.board.set("x" * length)
        check.paste()
        time.sleep(2.0)
    fits = "x" * (LIMIT - 12)
    check.board.set(fits)
    check.paste()
    check.keys.await_recorded(out, len(framed(fits)), timeout=20.0)
    got = check.keys.stop("oversized")
    expect("oversized", got, framed(fits), "nothing for 1 MiB + 1 and a framed 1 MiB + 1, then a whole 1 MiB")


def case_stale(check):
    out = check.keys.start("stale", "paste")
    stale, fresh = framed("stale paste"), framed("fresh paste")
    provider = check.board.provide("signal", "stale paste", "fresh paste")
    try:
        check.paste()
        await_asked(provider, "first")
        # A new recipient generation: search opens and closes, then the next paste is requested.
        check.chord(F)
        check.keys.tap(ESCAPE)
        check.paste()
        # The session handles keys in order, so once b reaches the shell the paste before it has
        # been requested for the new recipient; only then does the provider answer the first.
        check.keys.tap(B)
        if check.keys.await_recorded(out, len("62")) != "62":
            raise CannotCheck("b did not reach the shell after the second paste")
        provider.send_signal(signal.SIGUSR1)
        # The session reads one paste at a time, so the next is asked for only after the first
        # was answered, and then from its own promise.
        await_asked(provider, "next")
        provider.send_signal(signal.SIGUSR1)
        check.keys.await_recorded(out, len("62") + len(stale) + len(fresh), timeout=4.0)
    finally:
        provider.kill()
        provider.wait()
    got = check.keys.stop("stale").removeprefix("62")
    if got != fresh:
        raise Violation(f"pasteboard: stale: {got.count(stale)} stale and {got.count(fresh)} fresh pastes "
                        f"({len(got) // 2} bytes) instead of the fresh one alone")
    print("pasteboard: stale: the paste requested before the recipient changed was dropped and the next "
          "arrived alone")


def helpers(runner):
    listing = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,stat=,command="], capture_output=True,
                             text=True).stdout
    parent = str(runner.process.pid)
    return [fields[0] for fields in (line.split(None, 3) for line in listing.splitlines())
            if len(fields) == 4 and fields[1] == parent
            and ("--internal-pasteboard" in fields[3] or fields[2].startswith("Z"))]


def case_stalled(check):
    out = check.keys.start("stalled", "paste")
    provider = check.board.provide(-1, "never")
    runner = check.runner
    try:
        runner.start_pings()
        asked = time.monotonic()
        check.paste()
        await_asked(provider, "stalled")
        time.sleep(max(0.0, asked + REAPED_BY - time.monotonic()))
        runner.pinging = False
        left = helpers(runner)
        early = check.keys.recorded(out)
    finally:
        runner.pinging = False
        provider.kill()
        provider.wait()
    worst, count = runner.worst_ping_ms(asked, asked + HELPER_TIMEOUT)
    errors = []
    if early:
        errors.append(f"pasteboard: stalled: recorded {len(early) // 2} bytes from a provider that never answered")
    if left:
        errors.append(f"pasteboard: stalled: the helper outlived its {HELPER_TIMEOUT:.0f} s timeout "
                      f"({len(left)} left {REAPED_BY} s after the request)")
    print(f"pasteboard: stalled: worst ping {worst:.1f} ms over {count} pings")
    if worst > PING_LIMIT_MS:
        message = f"pasteboard: stalled: a ping took {worst:.1f} ms (limit {PING_LIMIT_MS:.0f} ms)"
        if RECORD_TIMING:
            print(f"{message} (recorded only)")
        else:
            errors.append(message)
    check.board.set("after stall")
    check.paste()
    check.keys.await_recorded(out, len(early) + len(framed("after stall")))
    got = check.keys.stop("stalled")
    if errors:
        raise Violation("\n".join(errors))
    expect("stalled", got, framed("after stall"), "nothing from the stalled provider, then the next paste")


def case_copy(check):
    check.board.set("sentinel")
    # Erasing the display pushes the rows to history, where the wrapped command line before it
    # would join row 0's logical line; erase the history too.
    printed = check.runner.home / "copy.printed"
    check.runner.send(f"type printf '\\033[3J\\033[2J\\033[Hsumie-copy-fixture\\n'; : > {shlex.quote(str(printed))}")
    check.runner.wait_for_file(printed)
    time.sleep(0.3)
    dragged = "sumie-copy"
    check.keys.drag((0, 0), (len(dragged) - 1, 0))
    time.sleep(1.0)
    if check.board.get() != "sentinel":
        raise Violation("pasteboard: copy: the drag release changed the sentinel; macOS copies only on Cmd+C")
    check.chord(C)
    got = check.await_board(dragged)
    if got != dragged:
        state = "kept the sentinel" if got == "sentinel" else f"holds {len(got or '')} other bytes"
        raise Violation(f"pasteboard: copy: after Cmd+C the board {state}")
    print("pasteboard: copy: the sentinel stays after a drag and Cmd+C replaces it with the dragged cells")


def osc52(text):
    return f"printf '\\033]52;c;%s\\007' {base64.b64encode(text.encode()).decode()}"


def case_osc52(check):
    check.board.set("sentinel")
    check.shell(osc52("osc52 write 墨"))
    if check.await_board("osc52 write 墨") != "osc52 write 墨":
        raise Violation("pasteboard: osc52: a focused write did not reach the board")
    check.shell("printf '\\033]52;c;\\007'")
    if check.await_board(None) is not None:
        raise Violation("pasteboard: osc52: a focused clear left a string on the board")
    check.board.set("sentinel")
    check.keys.focus(False)
    check.shell(osc52("osc52 unfocused"))
    time.sleep(1.0)
    check.keys.focus(True)
    if check.board.get() != "sentinel":
        raise Violation("pasteboard: osc52: a write without focus changed the board")
    check.keys.start("osc52", "osc52")
    time.sleep(1.0)
    got = check.keys.stop("osc52")
    if "sentinel".encode().hex() in got or base64.b64encode(b"sentinel").hex() in got:
        raise Violation("pasteboard: osc52: a query got the board's contents back")
    print(f"pasteboard: osc52: focused write and clear apply, unfocused write dropped, query answered with "
          f"{len(got) // 2} bytes and no contents")


CASES = (case_paste, case_empty, case_oversized, case_stale, case_stalled, case_copy, case_osc52)


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    binary = sys.argv[1]
    evidence = Path(sys.argv[2]) if len(sys.argv) == 3 else None
    errors = []
    with tempfile.TemporaryDirectory(prefix="sumie-pasteboard.") as scratch:
        board = Board(Path(scratch) / "pbfixture", f"org.sumie.check.{os.getpid()}.{time.monotonic_ns()}")
        try:
            subprocess.run(["xcrun", "clang", "-arch", "arm64", "-fobjc-arc", "-Wall", "-Wextra", "-Werror",
                            "-framework", "AppKit", SCRIPTS / "pbfixture.m", "-o", board.tool], check=True)
            hook, _ = build_tools(Path(scratch), False)
            board.set("sentinel")
            os.environ["SUMIE_MACOS_CI_PASTEBOARD"] = board.name
            runner = Runner(binary, hook, str(Path(scratch) / "home"))
        except (OSError, subprocess.CalledProcessError, CannotCheck) as error:
            print(f"pasteboard: could not start: {error}", file=sys.stderr)
            return 2
        try:
            if not runner.first.wait(30):
                raise CannotCheck("no first frame")
            runner.wait_for("runner", "focused", "true", timeout=10)
            check = Check(runner, board)
            for case in CASES:
                case(check)
                check.general_unchanged(case.__name__.removeprefix("case_"))
        except CannotCheck as error:
            print(f"pasteboard: could not check: {error}", file=sys.stderr)
            return 2
        except Violation as error:
            errors += str(error).splitlines()
        finally:
            code = runner.close()
            board.clear()
    if code != 0:
        errors.append(f"pasteboard: the runner exited {code}")
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / "pasteboard.json").write_text(json.dumps({"errors": errors, "load": os.getloadavg()}, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
