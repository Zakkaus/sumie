#!/usr/bin/env python3
"""Key recorder for the keyboard check; run it in the shell inside sumie.

usage: keyrec.py MODES OUT

MODES is a comma-separated list of: plain; deckm (DECCKM, mode 1); kitty (Kitty keyboard
flags 11: disambiguate, event types, every key as an escape); focus (mode 1004); paste
(bracketed paste, mode 2004); osc52 (no mode: an OSC 52 clipboard query once recording
starts, so its reply is recorded). The recorder puts its tty in raw mode, sets the modes and
waits for a cursor position report, so the terminal has applied every mode before the first
key. Then it creates OUT.ready and writes each read as one hex line to OUT, never to the
terminal, until OUT.stop exists. On exit it restores the modes and the tty, then creates
OUT.done.
"""
import os
from pathlib import Path
import select
import sys
import termios
import tty

ENABLE = {"plain": "", "deckm": "\x1b[?1h", "kitty": "\x1b[>11u", "focus": "\x1b[?1004h",
          "paste": "\x1b[?2004h", "osc52": ""}
DISABLE = {"plain": "", "deckm": "\x1b[?1l", "kitty": "\x1b[<u", "focus": "\x1b[?1004l",
           "paste": "\x1b[?2004l", "osc52": ""}
QUERY = {"osc52": "\x1b]52;c;?\x07"}
POLL = 0.02


def write(text):
    os.write(sys.stdout.fileno(), text.encode())


def await_position_report(fd):
    reply = b""
    while not reply.endswith(b"R"):
        ready, _, _ = select.select([fd], [], [], 5.0)
        if not ready:
            raise SystemExit("keyrec: no cursor position report")
        reply += os.read(fd, 64)


def main():
    if len(sys.argv) != 3 or not set(sys.argv[1].split(",")) <= ENABLE.keys():
        print(__doc__, file=sys.stderr)
        return 2
    modes, out = sys.argv[1].split(","), Path(sys.argv[2])
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    tty.setraw(fd)
    try:
        write("".join(ENABLE[mode] for mode in modes) + "\x1b[6n")
        await_position_report(fd)
        with out.open("w") as record:
            out.with_name(out.name + ".ready").touch()
            write("".join(QUERY.get(mode, "") for mode in modes))
            stop = out.with_name(out.name + ".stop")
            while not stop.exists():
                ready, _, _ = select.select([fd], [], [], POLL)
                if ready:
                    record.write(os.read(fd, 65536).hex() + "\n")
                    record.flush()
    finally:
        write("".join(DISABLE[mode] for mode in reversed(modes)))
        termios.tcsetattr(fd, termios.TCSAFLUSH, saved)
        out.with_name(out.name + ".done").touch()
    return 0


if __name__ == "__main__":
    sys.exit(main())
