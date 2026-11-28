#!/usr/bin/env python3
"""U4 typed-shell check on the terminal driver, run by check-gate.py as macos-shell.

usage: shell.py BINARY [EVIDENCE_DIR]

Starts `SUMIE_MACOS_CI_RUNNER=terminal` with SUMIE_MACOS_CI_READBACK set, a throwaway HOME and
no SHELL, so the pane runs the account's own login shell, and posts real `CGEvent` keys to
the runner's PID. The runner reads back every presented frame (`capture DIR`). Exit 0 on a
pass, 1 on a violation (including missing permission to post events), 2 if it cannot check.
At the window's first size, and again after a native resize to a second one:
  size   `sh t` and Return, typed as keys, run a fixture in HOME: it saves its parent's name,
         which must be a login shell's (`-zsh`), and `stty size`,
         clears the screen, paints a green bar exactly as wide as the child's columns on
         row 1 and a magenta cell on the child's last row, then waits for a line. The
         child's size must equal the focused pane's grid from the runner's `report`; in the
         frame presented with the output the bar spans exactly that many cells on row 1 only,
         the magenta cell sits on the grid's last row, and the grid fills the frame to within
         one cell. The first frame after the resize already has the new size.
  wide   The fixture also prints 骨直かなカナ한글 from three columns before the right edge:
         骨 takes two cells, 直 wraps (a spacer ends the row) and the other six follow on the
         next row, each two cells wide, none U+FFFD at the VT boundary; in the frame every
         one has ink in both of its cells, and the log has no missing-glyph warning.
Before the checks, a reopen must still capture a PNG. Then `exit` and Return, typed: the window
leaves the screen and the runner exits 0. Without the runner, an immediately exiting shell
must fail a third-frame screenshot request without writing a PNG.
"""
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import threading
import time

SCRIPTS = Path(__file__).resolve().parent
CONFIG = '[font]\nfamily = "Menlo"\n\n[cursor]\nblink = false\n'
# AppKit virtual key codes: the same positions on QWERTY, QWERTZ and AZERTY layouts.
KEYS = {"s": 0x01, "h": 0x04, "t": 0x11, " ": 0x31, "e": 0x0E, "x": 0x07, "i": 0x22, "\r": 0x24}
SECOND_SIZE = (900, 560)
WIDE = "骨直かなカナ한글"
GREEN, MAGENTA = (0, 255, 0), (255, 0, 255)
FIXTURE = f"""ps -o comm= -p $PPID >"$HOME/shell"
stty size >"$HOME/size.new" && mv "$HOME/size.new" "$HOME/size"
set -- $(stty size)
printf "\\033[H\\033[2J\\033[48;2;0;255;0m%${{2}}s\\033[0m" ''
printf '\\033[%d;1H\\033[48;2;255;0;255m \\033[0m' "$1"
printf '\\033[3;%dH{WIDE}\\033[5;1H' $(($2 - 2))
read -r line
"""
MISSING = "could not draw terminal glyphs"
WAIT = 10.0


class CannotCheck(Exception):
    pass


class Violation(Exception):
    pass


class Runner:
    def __init__(self, binary, home):
        self.home = Path(home)
        config = self.home / "config"
        (config / "sumie").mkdir(parents=True)
        (config / "sumie/sumie.toml").write_text(CONFIG)
        (self.home / "t").write_text(FIXTURE)
        self.frames = self.home / "frames"
        self.frames.mkdir()
        self.shell = ""
        # The fixture frames each check compared, for the evidence.
        self.checked = []
        env = {key: value for key, value in os.environ.items() if key not in ("SHELL", "ZDOTDIR")}
        env.update(SUMIE_MACOS_CI_RUNNER="terminal", SUMIE_MACOS_CI_READBACK="1", HOME=str(self.home),
                   XDG_CONFIG_HOME=str(config))
        self.process = subprocess.Popen([binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                        stderr=subprocess.PIPE, bufsize=0, env=env, cwd=self.home)
        self.lock = threading.Lock()
        self.out, self.err = [], []
        for stream, lines in ((self.process.stdout, self.out), (self.process.stderr, self.err)):
            threading.Thread(target=self.read, args=(stream, lines), daemon=True).start()

    def read(self, stream, lines):
        for raw in stream:
            with self.lock:
                lines.append((time.monotonic(), raw.decode(errors="replace").rstrip("\n")))

    def send(self, command):
        self.process.stdin.write((command + "\n").encode())

    def wait_for(self, *prefix, since=0.0, timeout=WAIT):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            with self.lock:
                found = [(at, line.split()) for at, line in self.out
                         if at >= since and line.split()[:len(prefix)] == list(prefix)]
            if found:
                return found[0]
            if self.process.poll() is not None:
                break
            time.sleep(0.01)
        raise CannotCheck(f"no `{' '.join(prefix)}` line within {timeout} s")

    def ask(self, command, *prefix):
        asked = time.monotonic()
        self.send(command)
        return self.wait_for("runner", *prefix, since=asked)[1][1 + len(prefix):]

    def report(self):
        fields = self.ask("report", "report")
        return {"columns": int(fields[5]), "rows": int(fields[6]), "prompt": fields[8] == "1"}

    def geometry(self):
        width, height, *rest = self.ask("geometry", "geometry")
        x, y, cell_w, cell_h = map(float, rest)
        return {"size": (int(width), int(height)), "origin": (x, y), "cell": (cell_w, cell_h)}

    def presents(self):
        with self.lock:
            return [(at, int(line.split()[2])) for at, line in self.out if line.startswith("runner present ")]

    def type(self, text):
        for char in text:
            for phase in ("down", "up"):
                if self.ask(f"key {KEYS[char]} {phase} -", "posted", "key") != ["true"]:
                    raise Violation("shell: could not post a key: no permission to post events, "
                                    "or the window is not key")
            time.sleep(0.02)

    def logged(self, needle):
        with self.lock:
            return [line for _, line in self.err if needle in line]


def pixels(png):
    """The frame as (width, height, rows of RGB tuples), through sips' 32-bit BMP."""
    bmp = png.with_suffix(".bmp")
    subprocess.run(["sips", "-s", "format", "bmp", png, "--out", bmp], check=True, capture_output=True)
    data = bmp.read_bytes()
    offset, = struct.unpack_from("<I", data, 10)
    width, height, _, bits = struct.unpack_from("<iiHH", data, 18)
    masks = struct.unpack_from("<III", data, 54)
    if bits != 32:
        raise CannotCheck(f"sips wrote a {bits}-bit BMP")
    shifts = [(mask & -mask).bit_length() - 1 for mask in masks]
    words = struct.unpack_from(f"<{abs(width * height)}I", data, offset)
    rows = [[tuple((word >> shift) & 0xFF for shift in shifts) for word in words[row * width:(row + 1) * width]]
            for row in range(abs(height))]
    if height > 0:
        rows.reverse()
    return width, abs(height), rows


def near(color, target, tolerance=24):
    return all(abs(a - b) <= tolerance for a, b in zip(color, target))


def settle(runner, since):
    """The last present once none followed for 0.5 s."""
    end = time.monotonic() + WAIT
    while time.monotonic() < end:
        presents = [entry for entry in runner.presents() if entry[0] >= since]
        if presents and time.monotonic() - presents[-1][0] > 0.5:
            return presents[-1][1]
        time.sleep(0.05)
    raise CannotCheck("the window did not settle after the output")


def run_fixture(runner, name):
    """Type `sh t`; returns the child's rows and columns once the VT shows the fixture."""
    size = runner.home / "size"
    if size.exists():
        size.unlink()
    end = time.monotonic() + WAIT
    while not runner.report()["prompt"]:
        if time.monotonic() > end:
            raise CannotCheck(f"{name}: the shell never showed a prompt")
        time.sleep(0.1)
    runner.type("sh t\r")
    while not size.exists() or not size.read_text().strip():
        if time.monotonic() > end:
            raise Violation(f"{name}: the typed command did not run")
        time.sleep(0.02)
    rows, columns = map(int, size.read_text().split())
    shell = (runner.home / "shell").read_text().strip() if (runner.home / "shell").exists() else ""
    runner.shell = shell
    if not shell.startswith("-"):
        raise Violation(f"{name}: the pane's shell {shell!r} is not a login shell")
    # 骨 on row 3 shows the fixture's output reached the VT, wherever the child placed it.
    while "2" not in runner.ask("cells 2", "cells")[1]:
        if time.monotonic() > end:
            raise Violation(f"{name}: the fixture's output never reached the VT")
        time.sleep(0.05)
    return rows, columns


def wide_errors(name, runner, columns):
    errors = []
    edge = runner.ask("cells 2", "cells")[1]
    wrapped = runner.ask("cells 3", "cells")[1]
    if edge[columns - 3:] != "20s":
        errors.append(f"{name}: row 3 ends {edge[columns - 3:]!r}, expected a wide cell and a wrap spacer")
    if wrapped[:2 * (len(WIDE) - 1)] != "20" * (len(WIDE) - 1):
        errors.append(f"{name}: row 4 starts {wrapped[:2 * (len(WIDE) - 1)]!r}, expected seven wide cells")
    if "f" in edge + wrapped:
        errors.append(f"{name}: U+FFFD reached the VT")
    return errors


def frame_errors(name, frame, geometry, grid, columns, rows):
    """Compare a presented frame with the child's size and the VT grid."""
    width, height, image = frame
    (x0, y0), (cell_w, cell_h) = geometry["origin"], geometry["cell"]
    errors = []
    if (width, height) != geometry["size"]:
        return [f"{name}: the frame is {width}x{height}, the window {geometry['size'][0]}x{geometry['size'][1]}"]

    def at(column, row, fx=0.5, fy=0.5):
        x, y = int(x0 + (column + fx) * cell_w), int(y0 + (row + fy) * cell_h)
        return image[y][x] if 0 <= x < width and 0 <= y < height else None

    def ink(column, row):
        """Pixels of a cell that differ from the background."""
        left, top = int(x0 + column * cell_w), int(y0 + row * cell_h)
        return sum(sum(abs(a - b) for a, b in zip(image[y][x], background)) > 60
                   for y in range(top, min(top + int(cell_h), height))
                   for x in range(left, min(left + int(cell_w), width)))

    bar = [column for column in range(grid[0] + 1) if (color := at(column, 0)) and near(color, GREEN)]
    if bar != list(range(columns)):
        errors.append(f"{name}: the green bar covers {len(bar)} cells of row 1, the child has {columns} columns")
    if any(near(at(column, 1) or (0, 0, 0), GREEN) for column in range(grid[0])):
        errors.append(f"{name}: the green bar wrapped onto row 2")
    marks = [row + 1 for row in range(grid[1]) if near(at(0, row) or (0, 0, 0), MAGENTA)]
    if marks != [grid[1]]:
        errors.append(f"{name}: the child's last row ({rows}) shows on rows {marks} of a {grid[1]}-row grid")
    right, bottom = width - (x0 + grid[0] * cell_w), height - (y0 + grid[1] * cell_h)
    if not (0 <= right < cell_w + x0 and 0 <= bottom < cell_h + y0):
        errors.append(f"{name}: a {grid[0]}x{grid[1]} grid leaves {right:.0f} px and {bottom:.0f} px of the frame")
    background = at(grid[0] // 2, grid[1] - 3)
    cells = [(columns - 3, 2)] + [(2 * index, 3) for index in range(len(WIDE) - 1)]
    for column, row in cells:
        halves = [ink(column, row), ink(column + 1, row)]
        if 0 in halves:
            errors.append(f"{name}: the wide cell at column {column + 1}, row {row + 1} has ink {halves} in its halves")
    return errors


def check_size(runner, name, result):
    since = time.monotonic()
    rows, columns = run_fixture(runner, name)
    grid = runner.report()
    grid = (grid["columns"], grid["rows"])
    errors = wide_errors(name, runner, columns)
    if (columns, rows) != grid:
        errors.append(f"{name}: stty size says {columns}x{rows}, the VT grid is {grid[0]}x{grid[1]}")
    count = settle(runner, since)
    geometry = runner.geometry()
    frame = pixels(runner.frames / f"{count}.png")
    errors += frame_errors(name, frame, geometry, grid, columns, rows)
    if runner.logged(MISSING):
        errors.append(f"{name}: a glyph was missing: {runner.logged(MISSING)[0]}")
    result[name] = {"shell": runner.shell, "stty": [rows, columns], "grid": grid, "frame": count,
                    "geometry": geometry}
    runner.checked.append((name, count))
    if not errors:
        print(f"{name}: {runner.shell}: stty, VT and frame {count} agree on {columns}x{rows} at "
              f"{geometry['size'][0]}x{geometry['size'][1]} px; {len(WIDE)} wide cells drawn")
    runner.type("\r")
    return errors


def check_resize(runner, result):
    """The first present after `Resized` already has the window's new size."""
    settle(runner, 0.0)
    asked = time.monotonic()
    runner.send(f"resize {SECOND_SIZE[0]} {SECOND_SIZE[1]}")
    resized, _ = runner.wait_for("runner", "resized", since=asked)
    first = settle(runner, resized) and next(count for at, count in runner.presents() if at > resized)
    geometry = runner.geometry()
    width, height, _ = pixels(runner.frames / f"{first}.png")
    result["resize"] = {"first": first, "frame": [width, height], "window": geometry["size"]}
    if (width, height) != geometry["size"]:
        return [f"resize: the first frame after the resize is {width}x{height}, "
                f"the window {geometry['size'][0]}x{geometry['size'][1]}"]
    print(f"resize: frame {first}, the first after it, is {width}x{height}")
    return []


def offscreen(tool, pid):
    out = subprocess.run([tool, "bounds", str(pid)], capture_output=True, text=True).stdout.split()
    return len(out) != 5 or out[4] != "1"


def check_exit(runner, tool, result):
    runner.type("exit\r")
    sent = time.monotonic()
    gone = None
    while time.monotonic() - sent < WAIT and runner.process.poll() is None:
        if gone is None and offscreen(tool, runner.process.pid):
            gone = time.monotonic() - sent
        time.sleep(0.02)
    code = runner.process.poll()
    result["exit"] = {"code": code, "offscreen": gone}
    if code != 0:
        return [f"exit: the typed exit left the process {'running' if code is None else f'exiting {code}'}"]
    print(f"exit: exit 0 {time.monotonic() - sent:.3f} s after the typed exit")
    return []


def check_reopened_capture(runner):
    runner.send("cycle")
    runner.wait_for("runner", "cycled", "1")
    asked = time.monotonic()
    runner.send("open")
    runner.wait_for("runner", "first", since=asked)
    runner.wait_for("runner", "focused", "true", since=asked)
    count, = runner.ask(f"capture {runner.frames}", "capture")
    frame = str(int(count) + 1)
    runner.send("type printf 'cycle capture'")
    try:
        runner.wait_for("runner", "present", frame, since=asked)
    except CannotCheck as error:
        raise Violation("cycle capture: the reopened window did not present a PNG") from error
    png = runner.frames / f"{frame}.png"
    if not png.is_file() or not png.stat().st_size:
        raise Violation("cycle capture: the reopened window did not present a PNG")
    print(f"cycle capture: wrote {frame}.png after reopening")


def check_early_screenshot(binary, home):
    png = home / "early.png"
    env = {**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / "config"), "SHELL": "/usr/bin/true"}
    env.pop("SUMIE_MACOS_CI_RUNNER", None)
    result = subprocess.run([binary, "--exit-after-frames", "3", "--screenshot", png], env=env,
                            capture_output=True, text=True, timeout=30)
    if (result.returncode != 1 or png.exists()
            or not re.search(r"window closed after [012] of 3 presentations", result.stderr)):
        return [f"exit: early screenshot: expected exit 1 and no PNG, got {result.returncode}: {result.stderr.strip()}"]
    print("exit: early screenshot: exit 1 with no PNG")
    return []


def main():
    if len(sys.argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    binary = sys.argv[1]
    evidence = Path(sys.argv[2]) if len(sys.argv) == 3 else None
    result = {"load": os.getloadavg()}
    with tempfile.TemporaryDirectory(prefix="sumie-shell.") as scratch:
        tool = Path(scratch) / "cgtool"
        try:
            subprocess.run(["xcrun", "--sdk", "macosx", "swiftc", "-O", SCRIPTS.parent / "macos-spike/cgtool.swift",
                            "-o", tool], check=True, capture_output=True)
            runner = Runner(binary, str(Path(scratch) / "home"))
        except (OSError, subprocess.CalledProcessError) as error:
            print(f"shell: could not start: {error}", file=sys.stderr)
            return 2
        try:
            runner.wait_for("runner", "first", timeout=30)
            check_reopened_capture(runner)
            errors = check_size(runner, "size 1", result)
            errors += check_resize(runner, result)
            errors += check_size(runner, "size 2", result)
            if not errors:
                errors += check_exit(runner, tool, result)
            if not errors:
                errors += check_early_screenshot(binary, runner.home)
        except Violation as violation:
            errors = [str(violation)]
        except (CannotCheck, OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            print(f"shell: could not check: {error}", file=sys.stderr)
            return 2
        finally:
            if runner.process.poll() is None:
                runner.process.kill()
            runner.process.wait()
            if evidence:
                evidence.mkdir(parents=True, exist_ok=True)
                for name, count in runner.checked:
                    shutil.copyfile(runner.frames / f"{count}.png", evidence / f"shell-{name.replace(' ', '-')}.png")
    if evidence:
        (evidence / "shell.json").write_text(json.dumps(result, indent=1))
    for error in errors:
        print(error, file=sys.stderr)
    return int(bool(errors))


if __name__ == "__main__":
    sys.exit(main())
