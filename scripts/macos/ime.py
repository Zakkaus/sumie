#!/usr/bin/env python3
"""M1b-2 input-method check on the terminal driver, run by check-gate.py.

usage: ime.py BINARY [EVIDENCE_DIR]

Starts the terminal runner like keys.py with SUMIE_MACOS_CI_PASTEBOARD naming a fresh board,
runs keyrec.py in its shell and records the bytes that reach the PTY. Exit 0 on a pass, 1 on a
violation, 2 if it cannot check. Text never appears in its output; the runner reports lengths.

Protocol cases: input methods stay off, and the runner injects the events winit would report
(`ime preedit`, `ime commit`, `ime disabled`) into the window's batch, with real `CGEvent` keys
around them; it reports an injection once its batch was delivered. They prove the bytes and
their recipient, not that a native composition was cancelled.
  commit     Preedit, empty preedit, commit: the PTY gets the commit's bytes once and nothing
             else; the overlay shows the preedit and its caret, then clears. A second commit
             without a composition, as the input method inserts punctuation, arrives too. After
             a composition and focus loss, its late commit sends nothing, and the next commit
             arrives.
  late       A composition in pane 1 is followed by a switch to pane 2; the queued commit
             reaches pane 1, and the old preedit stays out of pane 2's overlay. Pane 2 composes,
             then search opens: the late preedit stays out of the overlay until `disabled`,
             after which search composes and commits. Pane 2's next composition commits there.
  batch      Cmd+F and a preedit in one batch, as when the key after the chord composes: the
             preedit belongs to search, and its commit reaches no PTY.
  kitty      Kitty flags 11: a is pressed, then a composition starts. a's release reaches the
             PTY at that moment, once; neither the next preedit nor a's real key-up sends
             another. The commit follows.
  switch     Kitty flags 11 in both panes: a is pressed in pane 1, then pane 2 takes the focus
             and composes. a's release reaches pane 1, once; pane 2 gets only its commit. Then
             a is pressed in pane 2, search opens and composes: the release reaches pane 2.
  command    Row 0 is selected. During a composition, Cmd+C copies it to the board and Cmd+V
             pastes it once, with no key bytes under Kitty flags 11; the composition then
             commits after the paste.
Native cases need the selected input source to be Traditional or Simplified Chinese Pinyin;
with any other source the check exits 2 after the protocol cases: the native half was not
checked. Input methods are on, and the real input method composes from posted keys; its
preedits are reported by length.
  direct     The comma key, before any composition, then n i and the comma key: the input
             method inserts its comma without marked text, after the candidate in the second
             case. The PTY gets each commit once: a comma, half- or full-width as the input
             method's punctuation setting chooses (not the layout's key text), and before it
             one 3-byte character.
  pinyin     n i preedits 1 and 2 bytes and sends nothing; Space commits one 3-byte character
             once, without a Return. n i Escape cancels: nothing arrives.
  takeover   Kitty flags 11: 1 is pressed and reported; n starts a composition, which sends 1's
             release once; 1's key-up and Escape send nothing more.
  shortcuts  Row 0 is selected. During n i, Cmd+C copies it and Cmd+V pastes it once; the
             composition keeps its 2 bytes, so the input method saw neither chord, and a
             continues it.
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
COMMA, DIGIT1, SPACE, ESCAPE = 0x2B, 0x12, 0x31, 0x35
LEFT, RIGHT, DOWN, UP = 0x7B, 0x7C, 0x7D, 0x7E
PINYIN = ("com.apple.inputmethod.TCIM.Pinyin", "com.apple.inputmethod.SCIM.ITABC")
CONTENT = "sumie 墨 paste"
BO, BA, PO, PU = "ㄅ", "把", "ㄆ", "葡"
COMMAS = (",", "\uff0c")
PRESS_1, RELEASE_1 = "1b5b343975", "1b5b34393b313a3375"
# Erasing the display pushes the rows to history, where the command line before it would join
# row 0's logical line; erase the history too.
FIXTURE, SELECTED = "printf '\\033[3J\\033[2J\\033[Hsumie-copy-fixture\\n'; ", "sumie-copy"
WAIT = 5.0


def utf8(text):
    return text.encode().hex()


def expect(name, got, want):
    if got != want:
        raise Violation(f"ime: {name}: recorded {got or 'nothing'} instead of {want or 'nothing'}")
    print(f"ime: {name}: {want or 'nothing'}")


class Ime:
    def __init__(self, runner, board):
        self.runner = runner
        self.board = board
        self.keys = Keys(runner)

    def command(self, line, reply):
        asked = time.monotonic()
        self.runner.send(line)
        return self.runner.wait_for("runner", *reply, since=asked)[1]

    def inject(self, line, kind):
        """Inject an event; the runner replies once the batch that carries it was delivered."""
        self.command(f"ime {line}".rstrip(), ("injected", kind))

    def preedit(self, text=""):
        self.inject(f"preedit {text}", "preedit")

    def commit(self, text):
        self.inject(f"commit {text}", "commit")

    def disabled(self):
        self.inject("disabled", "disabled")

    def overlay(self):
        """The overlay's bytes and caret, and the input generation."""
        return tuple(self.command("ime overlay", ("overlay",))[2:6])

    def generation(self):
        return self.overlay()[3]

    def expect_overlay(self, name, text, caret=None):
        length = len(text.encode())
        want = (str(length), *(("-", "-") if not length else map(str, caret or (length, length))))
        end = time.monotonic() + WAIT
        while (got := self.overlay()[:3]) != want and time.monotonic() < end:
            time.sleep(0.02)
        if got != want:
            raise Violation(f"ime: {name}: the overlay shows {' '.join(got)} instead of {' '.join(want)} "
                            "(bytes, caret)")

    def chord(self, code, *more):
        self.keys.key(COMMAND, "down", "command")
        for key in (code, *more):
            self.keys.tap(key, "command")
        self.keys.key(COMMAND, "up")

    def switch(self, act):
        """Run `act`, then wait until the session changed the input recipient."""
        before = self.generation()
        act()
        end = time.monotonic() + WAIT
        while self.generation() == before:
            if time.monotonic() >= end:
                raise Violation("ime: the input recipient did not change")
            time.sleep(0.02)

    def switch_chord(self, code, *more):
        self.switch(lambda: self.chord(code, *more))

    def focus_other(self, back):
        """Move pane focus across the split, whichever way the split runs."""
        self.switch_chord(*((LEFT, UP) if back else (RIGHT, DOWN)))

    def select_fixture(self, name, modes):
        """Start the recorder below a fixture row and select that row's first cells."""
        self.board.set("sentinel")
        out = self.keys.start(name, modes, before=FIXTURE)
        self.keys.drag((0, 0), (len(SELECTED) - 1, 0))
        return out

    def await_board(self, name, want):
        end = time.monotonic() + WAIT
        while (got := self.board.get()) != want and time.monotonic() < end:
            time.sleep(0.05)
        if got != want:
            state = "kept the sentinel" if got == "sentinel" else f"holds {len(got or '')} other bytes"
            raise Violation(f"ime: {name}: after Cmd+C the board {state}")
        print(f"ime: {name}: Cmd+C copied the selection")

    def copy_paste(self, name, out):
        """Cmd+C, then Cmd+V: the selection reaches the board, then the PTY, once."""
        self.chord(C)
        self.await_board(name, SELECTED)
        self.chord(V)
        pasted = framed(SELECTED)
        expect(f"{name}: Cmd+V pastes the copy once", self.keys.await_recorded(out, len(pasted)), pasted)
        return pasted

    def source(self):
        return self.command("ime source", ("source",))[2]

    def preedits(self, since):
        return [int(fields[3]) for _, fields in self.runner.matching("runner", "ime", "preedit", since=since)]

    def await_preedit(self, name, length, since):
        """Wait for the native input method's preedit of `length` bytes since `since`."""
        end = time.monotonic() + WAIT
        while time.monotonic() < end:
            if length in self.preedits(since):
                return
            time.sleep(0.02)
        raise Violation(f"ime: {name}: the input method reported preedits of {self.preedits(since) or 'none'} "
                        f"bytes, not one of {length}")

    def first_preedits(self, since):
        """The preedits the native input method reported since `since`, once there is one."""
        end = time.monotonic() + WAIT
        while not self.preedits(since) and time.monotonic() < end:
            time.sleep(0.02)
        return self.preedits(since)


def case_commit(ime):
    out = ime.keys.start("commit", "plain")
    ime.preedit(BO)
    ime.expect_overlay("commit: preedit", BO)
    ime.preedit()
    ime.expect_overlay("commit: empty preedit", "")
    expect("commit: composing", ime.keys.recorded(out), "")
    ime.commit(BA)
    ime.commit(BA)
    expect("commit: once, then a commit without a composition", ime.keys.recorded(out), utf8(BA) * 2)
    ime.preedit(BO)
    ime.keys.focus(False)
    ime.keys.focus(True)
    ime.commit(BA)
    ime.commit(PU)
    expect("commit: not the late commit after focus loss, then the next", ime.keys.stop("commit"),
           utf8(BA) * 2 + utf8(PU))


def case_late(ime):
    first = ime.keys.start("late-first", "plain")
    ime.switch_chord(E)
    second = ime.keys.start("late-second", "plain")
    ime.focus_other(back=True)
    ime.preedit(BO)
    ime.focus_other(back=False)
    # The old composition goes on before the cancel reaches it: a non-empty preedit, then its
    # commit.
    ime.preedit(BO + PO)
    ime.commit(BA)
    expect("late: queued commit reaches the original pane",
           ime.keys.await_recorded(first, len(utf8(BA))), utf8(BA))
    ime.expect_overlay("late: after the switch", "")
    ime.preedit(PO)
    ime.expect_overlay("late: pane 2's composition", PO)
    ime.switch_chord(F)
    # Without a commit only the cancel's report ends the old composition; search's own
    # composition follows it.
    ime.preedit(PO + BO)
    ime.expect_overlay("late: a late preedit after search opened", "")
    ime.disabled()
    ime.preedit(BO)
    ime.expect_overlay("late: search's composition after the cancel", BO)
    ime.commit(BA)
    ime.switch(lambda: ime.keys.tap(ESCAPE))
    expect("late: search sends nothing to the second pane", ime.keys.recorded(second), "")
    ime.preedit(PO)
    ime.commit(PU)
    expect("late: the second pane's own commit", ime.keys.stop("late-second"), utf8(PU))
    expect("late: only the queued commit reached the first pane", ime.keys.stop("late-first"), utf8(BA))
    # Close the second pane; the next case starts at the first pane's prompt.
    ime.switch_chord(W)


def case_batch(ime):
    out = ime.keys.start("batch", "plain")
    ime.switch(lambda: ime.inject(f"search {PO}", "search"))
    ime.expect_overlay("batch: the preedit right after Cmd+F, in search", PO)
    ime.commit(PU)
    ime.switch(lambda: ime.keys.tap(ESCAPE))
    expect("batch: the commit went to search", ime.keys.stop("batch"), "")


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


def case_switch(ime):
    press, release = KITTY_A[:10], KITTY_A[10:]
    first = ime.keys.start("switch-first", "kitty")
    ime.switch_chord(E)
    second = ime.keys.start("switch-second", "kitty")
    ime.focus_other(back=True)
    ime.keys.key(A, "down")
    expect("switch: press in pane 1", ime.keys.await_recorded(first, len(press)), press)
    ime.focus_other(back=False)
    ime.preedit(BO)
    expect("switch: the takeover release after a pane switch reaches pane 1", ime.keys.recorded(first),
           press + release)
    ime.keys.key(A, "up")
    ime.commit(PU)
    expect("switch: pane 2 gets only its commit", ime.keys.recorded(second), utf8(PU))
    ime.keys.key(A, "down")
    expect("switch: press in pane 2", ime.keys.await_recorded(second, len(utf8(PU) + press)), utf8(PU) + press)
    ime.switch_chord(F)
    ime.preedit(BO)
    ime.keys.key(A, "up")
    ime.commit(PU)
    ime.switch(lambda: ime.keys.tap(ESCAPE))
    expect("switch: the takeover release after search opened", ime.keys.stop("switch-second"),
           utf8(PU) + press + release)
    expect("switch: pane 1, once", ime.keys.stop("switch-first"), press + release)
    ime.switch_chord(W)


def case_command(ime):
    out = ime.select_fixture("command", "paste,kitty")
    ime.preedit(BO)
    pasted = ime.copy_paste("command: during a composition", out)
    ime.expect_overlay("command: the composition after the chords", BO)
    ime.commit(BA)
    expect("command: the commit after the paste", ime.keys.stop("command"), pasted + utf8(BA))


def native_direct(ime):
    out = ime.keys.start("direct", "plain")
    got = ""
    for name, keys, count in (("the comma key", (COMMA,), 1), ("n i and the comma key", (N, I, COMMA), 2)):
        since = time.monotonic()
        for key in keys:
            ime.keys.tap(key)
        end = time.monotonic() + WAIT
        while len(commits := ime.runner.matching("runner", "ime", "commit", since=since)) < count:
            if time.monotonic() >= end:
                raise Violation(f"ime: direct: {name} made {len(commits)} input-method commits instead of "
                                f"{count}: the text the input method inserted was dropped")
            time.sleep(0.02)
        length = sum(int(fields[3]) for _, fields in commits)
        new = ime.keys.await_recorded(out, len(got) + 2 * length)[len(got):]
        try:
            text = bytes.fromhex(new).decode()
        except ValueError:
            text = ""
        if (len(new) != 2 * length or len(text) != count or text[-1] not in COMMAS
                or any(len(char.encode()) != 3 for char in text[:-1])):
            raise Violation(f"ime: direct: {name} recorded {new or 'nothing'} instead of the input method's "
                            f"{count} commits of {length} bytes, once, ending in a comma")
        print(f"ime: direct: {name}: {new}")
        got += new
    expect("direct: nothing more", ime.keys.stop("direct"), got)


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
    ime.expect_overlay("takeover: after Escape", "")
    expect("takeover: one release", ime.keys.stop("takeover"), PRESS_1 + RELEASE_1)


def native_shortcuts(ime):
    out = ime.select_fixture("shortcuts", "paste,kitty")
    since = time.monotonic()
    ime.keys.tap(N)
    ime.keys.tap(I)
    ime.await_preedit("shortcuts: n i", 2, since)
    pasted = ime.copy_paste("shortcuts: during n i", out)
    ime.expect_overlay("shortcuts: the composition after the chords", "ni")
    since = time.monotonic()
    ime.keys.tap(A)
    if not any(length > 2 for length in ime.first_preedits(since)):
        raise Violation(f"ime: shortcuts: a after the chords made preedits of {ime.preedits(since) or 'none'} "
                        "bytes instead of continuing ni")
    ime.keys.tap(ESCAPE)
    ime.expect_overlay("shortcuts: after Escape", "")
    expect("shortcuts: Escape", ime.keys.stop("shortcuts"), pasted)


def native_cancel(ime):
    out = ime.keys.start("cancel", "plain")

    def cancelled(name, change, resume, cleanup):
        since = time.monotonic()
        ime.keys.tap(N)
        ime.keys.tap(I)
        ime.await_preedit(f"cancel: {name}: n i", 2, since)
        change()
        ime.expect_overlay(f"cancel: {name}: the old composition", "")
        resume()
        since = time.monotonic()
        ime.keys.tap(A)
        preedits = ime.first_preedits(since)
        if not preedits:
            raise Violation(f"ime: cancel: {name}: a made no preedit: the key was lost")
        if preedits != [1]:
            raise Violation(f"ime: cancel: {name}: a continued the old composition (preedits of {preedits} "
                            "bytes, not one of 1): the native composition survived")
        ime.keys.tap(ESCAPE)
        ime.expect_overlay(f"cancel: {name}: after Escape", "")
        cleanup()
        print(f"ime: cancel: {name}: the native composition ended")

    def nothing():
        pass

    cancelled("focus loss", lambda: ime.keys.focus(False), lambda: ime.keys.focus(True), nothing)
    cancelled("a pane split", lambda: ime.switch_chord(E), nothing, lambda: ime.switch_chord(W))
    cancelled("opening search", lambda: ime.switch_chord(F), nothing,
              lambda: ime.switch(lambda: ime.keys.tap(ESCAPE)))
    expect("cancel: bytes", ime.keys.stop("cancel"), "")


def protocol_cases(ime):
    case_commit(ime)
    case_late(ime)
    case_batch(ime)
    case_kitty(ime)
    case_switch(ime)
    case_command(ime)


def native_cases(ime):
    source = ime.source()
    if source not in PINYIN and os.environ.get("SUMIE_IME_NATIVE") == "skip":
        print(f"ime: native: NOT CHECKED (SUMIE_IME_NATIVE=skip, input source {source}); "
              "only the protocol cases ran")
        return
    if source not in PINYIN:
        raise CannotCheck(f"native cases: the selected input source {source} is not Chinese Pinyin; "
                          "the protocol cases passed")
    print(f"ime: native: input source {source}")
    ime.runner.send("ime on")
    # Commands run in order: the reply to the next one follows `ime on`.
    ime.source()
    # First: no composition has run yet, so the commit has no owner.
    native_direct(ime)
    native_pinyin(ime)
    native_takeover(ime)
    native_shortcuts(ime)
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
            ime = Ime(runner, board)
            protocol_cases(ime)
            native_cases(ime)
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
