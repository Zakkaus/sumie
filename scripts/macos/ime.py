#!/usr/bin/env python3
"""M1b-2 input-method check on the terminal driver, run by check-gate.py.

usage: ime.py BINARY [EVIDENCE_DIR]

Starts the terminal runner like keys.py with SUMIE_MACOS_CI_PASTEBOARD naming a fresh board,
runs keyrec.py in its shell and records the bytes that reach the PTY. Exit 0 on a pass, 1 on a
violation, 2 if it cannot check. Text never appears in its output; the runner reports lengths.

Protocol cases: input methods stay off, and the runner injects the events winit would report
(`ime preedit`, `ime commit`) into the window's composition, with real `CGEvent` keys around
them. They prove the bytes and their recipient, not that a native composition was cancelled.
  commit     Preedit, empty preedit, commit: the PTY gets the commit's bytes once and nothing
             else; the overlay shows the preedit and its caret, then clears. A second commit
             without a composition sends nothing.
  late       A composition in pane 1 is followed by a switch to pane 2, and by search opening
             in pane 2: its late commit reaches neither PTY. pane 2's own composition commits
             there.
  kitty      Kitty flags 11: a is pressed, then a composition starts. a's release reaches the
             PTY at that moment, once; neither the next preedit nor a's real key-up sends
             another. The commit follows.
  command    During a composition, Cmd+V pastes the board once and Cmd+C (nothing selected)
             sends nothing, with no key bytes under Kitty flags 11; the composition then
             commits after the paste.
Native cases need the selected input source to be Traditional or Simplified Chinese Pinyin;
with any other source they are skipped and the check says so. Input methods are on, and the
real input method composes from posted keys; its preedits are reported by length.
  pinyin     n i preedits 1 and 2 bytes and sends nothing; Space commits one 3-byte character
             once, without a Return. n i Escape cancels: nothing arrives.
  takeover   Kitty flags 11: 1 is pressed and reported; n starts a composition, which sends 1's
             release once; 1's key-up and Escape send nothing more.
  shortcuts  During n i, Cmd+V pastes once and Cmd+C sends nothing; the composition keeps its
             2 bytes, so the input method saw neither chord, and a continues it.
  cancel     During n i, focus loss, a pane split and opening search each cancel the native
             composition: the overlay clears, and the next a starts a 1-byte composition
             instead of continuing ni. Nothing reaches the PTY.
After the check the general board's change count must be unchanged (it is never read).
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from frames import CannotCheck, Runner, build_tools  # noqa: E402
from keys import COMMAND, KITTY_A, A, Keys, Violation  # noqa: E402
from pasteboard import Board, framed  # noqa: E402

SCRIPTS = Path(__file__).resolve().parent
C, E, F, I, N, V, W = 0x08, 0x0E, 0x03, 0x22, 0x2D, 0x09, 0x0D
DIGIT1, SPACE, ESCAPE = 0x12, 0x31, 0x35
LEFT, RIGHT, DOWN, UP = 0x7B, 0x7C, 0x7D, 0x7E
PINYIN = ("com.apple.inputmethod.TCIM.Pinyin", "com.apple.inputmethod.SCIM.ITABC")
CONTENT = "sumie 墨 paste"
BO, BA, PO, PU = "ㄅ", "把", "ㄆ", "葡"
PRESS_1, RELEASE_1 = "1b5b343975", "1b5b34393b313a3375"
SETTLE = 0.3


def utf8(text):
    return text.encode().hex()


def expect(name, got, want):
    if got != want:
        raise Violation(f"ime: {name}: recorded {got or 'nothing'} instead of {want or 'nothing'}")
    print(f"ime: {name}: {want or 'nothing'}")


class Ime:
    def __init__(self, runner):
        self.runner = runner
        self.keys = Keys(runner)

    def command(self, line, reply):
        asked = time.monotonic()
        self.runner.send(line)
        found = self.runner.wait_for("runner", *reply, since=asked)
        time.sleep(0.1)
        return found[1]

    def preedit(self, text=""):
        self.command(f"ime preedit {text}".rstrip(), ("injected", "preedit"))

    def commit(self, text):
        self.command(f"ime commit {text}", ("injected", "commit"))

    def overlay(self):
        time.sleep(0.1)
        fields = self.command("ime overlay", ("overlay",))
        return tuple(fields[2:5])

    def expect_overlay(self, name, text, caret=None):
        length = len(text.encode())
        want = (str(length), *(("-", "-") if not length else map(str, caret or (length, length))))
        got = self.overlay()
        if got != want:
            raise Violation(f"ime: {name}: the overlay shows {' '.join(got)} instead of {' '.join(want)} "
                            "(bytes, caret)")

    def chord(self, code, *more):
        self.keys.key(COMMAND, "down", "command")
        for key in (code, *more):
            self.keys.tap(key, "command")
        self.keys.key(COMMAND, "up")

    def source(self):
        return self.command("ime source", ("source",))[2]

    def preedits(self, since):
        return [int(fields[3]) for _, fields in self.runner.matching("runner", "ime", "preedit", since=since)]

    def await_preedit(self, name, length, since, timeout=5.0):
        """Wait for the native input method's preedit of `length` bytes since `since`."""
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if length in self.preedits(since):
                return
            time.sleep(0.02)
        raise Violation(f"ime: {name}: the input method reported preedits of {self.preedits(since) or 'none'} "
                        f"bytes, not one of {length}")


def case_commit(ime):
    out = ime.keys.start("commit", "plain")
    ime.preedit(BO)
    ime.expect_overlay("commit: preedit", BO)
    ime.preedit()
    ime.expect_overlay("commit: empty preedit", "")
    expect("commit: composing", ime.keys.recorded(out), "")
    ime.commit(BA)
    ime.commit(BA)
    expect("commit: once, then not again without a composition", ime.keys.stop("commit"), utf8(BA))


def focus_other(ime, back):
    """Move pane focus across the split, whichever way the split runs."""
    ime.chord(*((LEFT, UP) if back else (RIGHT, DOWN)))
    time.sleep(SETTLE)


def case_late(ime):
    first = ime.keys.start("late-first", "plain")
    ime.chord(E)
    time.sleep(1.0)
    second = ime.keys.start("late-second", "plain")
    focus_other(ime, back=True)
    ime.preedit(BO)
    focus_other(ime, back=False)
    ime.commit(BA)
    ime.expect_overlay("late: after the switch", "")
    ime.preedit(PO)
    ime.chord(F)
    time.sleep(SETTLE)
    ime.commit(PU)
    ime.keys.tap(ESCAPE)
    time.sleep(SETTLE)
    expect("late: commits after a switch and after search opened", ime.keys.recorded(first)
           + ime.keys.recorded(second), "")
    ime.preedit(PO)
    ime.commit(PU)
    expect("late: the second pane's own commit", ime.keys.stop("late-second"), utf8(PU))
    expect("late: the first pane", ime.keys.stop("late-first"), "")
    # Close the second pane; the next case starts at the first pane's prompt.
    ime.chord(W)
    time.sleep(1.0)


def case_kitty(ime):
    out = ime.keys.start("kitty", "kitty")
    press, release = KITTY_A[:10], KITTY_A[10:]
    ime.keys.key(A, "down")
    expect("kitty: press", ime.keys.await_recorded(out, len(press)), press)
    ime.preedit(BO)
    expect("kitty: release when the composition takes over", ime.keys.recorded(out), press + release)
    ime.preedit(BO + PO)
    ime.keys.key(A, "up")
    ime.commit(PU)
    expect("kitty: one release, then the commit", ime.keys.stop("kitty"), press + release + utf8(PU))


def case_command(ime, board):
    board.set(CONTENT)
    out = ime.keys.start("command", "paste,kitty")
    ime.preedit(BO)
    ime.chord(V)
    pasted = framed(CONTENT)
    got = ime.keys.await_recorded(out, len(pasted))
    ime.chord(C)
    expect("command: Cmd+V and Cmd+C during a composition", ime.keys.recorded(out), pasted)
    ime.expect_overlay("command: the composition after the chords", BO)
    ime.commit(BA)
    expect("command: the commit after the paste", ime.keys.stop("command"), got + utf8(BA))


def native_pinyin(ime):
    out = ime.keys.start("pinyin", "plain")
    since = time.monotonic()
    ime.keys.tap(N)
    ime.keys.tap(I)
    ime.await_preedit("pinyin: n i", 2, since)
    ime.expect_overlay("pinyin: n i", "ni")
    expect("pinyin: composing", ime.keys.recorded(out), "")
    since = time.monotonic()
    ime.keys.tap(SPACE)
    committed = ime.runner.wait_for("runner", "ime", "commit", since=since)[1]
    got = ime.keys.await_recorded(out, 2 * int(committed[3]))
    try:
        text = bytes.fromhex(got).decode()
    except ValueError:
        text = ""
    if len(text) != 1 or len(text.encode()) != 3 or len(ime.runner.matching("runner", "ime", "commit",
                                                                              since=since)) != 1:
        raise Violation(f"ime: pinyin: Space recorded {got or 'nothing'} instead of one committed character")
    print("ime: pinyin: one 3-byte character committed, no Return")
    ime.expect_overlay("pinyin: after the commit", "")
    since = time.monotonic()
    ime.keys.tap(N)
    ime.keys.tap(I)
    ime.await_preedit("pinyin: n i again", 2, since)
    ime.keys.tap(ESCAPE)
    time.sleep(SETTLE)
    ime.expect_overlay("pinyin: after Escape", "")
    expect("pinyin: Escape cancels", ime.keys.stop("pinyin")[len(got):], "")


def native_takeover(ime):
    out = ime.keys.start("takeover", "kitty")
    ime.keys.key(DIGIT1, "down")
    expect("takeover: 1 pressed", ime.keys.await_recorded(out, len(PRESS_1)), PRESS_1)
    since = time.monotonic()
    ime.keys.tap(N)
    ime.await_preedit("takeover: n", 1, since)
    expect("takeover: 1's release when n composes", ime.keys.recorded(out), PRESS_1 + RELEASE_1)
    ime.keys.key(DIGIT1, "up")
    ime.keys.tap(ESCAPE)
    expect("takeover: one release", ime.keys.stop("takeover"), PRESS_1 + RELEASE_1)


def native_shortcuts(ime, board):
    board.set(CONTENT)
    out = ime.keys.start("shortcuts", "paste,kitty")
    since = time.monotonic()
    ime.keys.tap(N)
    ime.keys.tap(I)
    ime.await_preedit("shortcuts: n i", 2, since)
    ime.chord(V)
    pasted = framed(CONTENT)
    ime.keys.await_recorded(out, len(pasted))
    ime.chord(C)
    expect("shortcuts: Cmd+V and Cmd+C during n i", ime.keys.recorded(out), pasted)
    ime.expect_overlay("shortcuts: the composition after the chords", "ni")
    since = time.monotonic()
    ime.keys.tap(A)
    end = time.monotonic() + 5
    while not ime.preedits(since) and time.monotonic() < end:
        time.sleep(0.02)
    if not any(length > 2 for length in ime.preedits(since)):
        raise Violation(f"ime: shortcuts: a after the chords made preedits of {ime.preedits(since) or 'none'} "
                        "bytes instead of continuing ni")
    ime.keys.tap(ESCAPE)
    expect("shortcuts: Escape", ime.keys.stop("shortcuts"), pasted)


def native_cancel(ime):
    out = ime.keys.start("cancel", "plain")

    def cancelled(name, change, resume, cleanup):
        since = time.monotonic()
        ime.keys.tap(N)
        ime.keys.tap(I)
        ime.await_preedit(f"cancel: {name}: n i", 2, since)
        change()
        time.sleep(SETTLE)
        ime.expect_overlay(f"cancel: {name}: the old composition", "")
        resume()
        since = time.monotonic()
        ime.keys.tap(A)
        end = time.monotonic() + 5
        while not ime.preedits(since) and time.monotonic() < end:
            time.sleep(0.02)
        if ime.preedits(since) != [1]:
            raise Violation(f"ime: cancel: {name}: a made preedits of {ime.preedits(since) or 'none'} bytes "
                            "instead of starting a new 1-byte composition: the native composition survived")
        ime.keys.tap(ESCAPE)
        time.sleep(SETTLE)
        cleanup()
        print(f"ime: cancel: {name}: the native composition ended")

    def wait(seconds):
        return lambda: time.sleep(seconds)

    def chord(code):
        return lambda: ime.chord(code)

    cancelled("focus loss", lambda: ime.keys.focus(False), lambda: ime.keys.focus(True), wait(0))
    cancelled("a pane split", chord(E), wait(1.0), lambda: (ime.chord(W), time.sleep(1.0)))
    cancelled("opening search", chord(F), wait(0), lambda: (ime.keys.tap(ESCAPE), time.sleep(SETTLE)))
    expect("cancel: bytes", ime.keys.stop("cancel"), "")


def protocol_cases(ime, board):
    case_commit(ime)
    case_late(ime)
    case_kitty(ime)
    case_command(ime, board)


def native_cases(ime, board):
    source = ime.source()
    if source not in PINYIN:
        print(f"ime: native: skipped: the selected input source {source} is not Chinese Pinyin")
        return
    print(f"ime: native: input source {source}")
    ime.runner.send("ime on")
    time.sleep(SETTLE)
    native_pinyin(ime)
    native_takeover(ime)
    native_shortcuts(ime, board)
    native_cancel(ime)


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    binary = sys.argv[1]
    evidence = Path(sys.argv[2]) if len(sys.argv) == 3 else None
    errors = []
    with tempfile.TemporaryDirectory(prefix="sumie-ime.") as scratch:
        board = Board(Path(scratch) / "pbfixture", f"org.sumie.check.{os.getpid()}.{time.monotonic_ns()}")
        try:
            subprocess.run(["xcrun", "clang", "-arch", "arm64", "-fobjc-arc", "-Wall", "-Wextra", "-Werror",
                            "-framework", "AppKit", SCRIPTS / "pbfixture.m", "-o", board.tool], check=True)
            hook, _ = build_tools(Path(scratch), False)
            board.set("sentinel")
            general = board.general_count()
            os.environ["SUMIE_MACOS_CI_PASTEBOARD"] = board.name
            runner = Runner(binary, hook, str(Path(scratch) / "home"))
        except (OSError, subprocess.CalledProcessError, CannotCheck) as error:
            print(f"ime: could not start: {error}", file=sys.stderr)
            return 2
        try:
            if not runner.first.wait(30):
                raise CannotCheck("no first frame")
            runner.wait_for("runner", "focused", "true", timeout=10)
            ime = Ime(runner)
            protocol_cases(ime, board)
            native_cases(ime, board)
        except CannotCheck as error:
            print(f"ime: could not check: {error}", file=sys.stderr)
            return 2
        except Violation as error:
            errors += str(error).splitlines()
        finally:
            code = runner.close()
            board.clear()
        if board.general_count() != general:
            errors.append("ime: the general pasteboard changed (a copy elsewhere during the check also counts)")
    if code != 0:
        errors.append(f"ime: the runner exited {code}")
    if evidence:
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / "ime.json").write_text(json.dumps({"errors": errors, "load": os.getloadavg()}, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
