import os
from pathlib import Path
import re
import subprocess
import sys
import time

FIRST = ["first request returned", "draining"]
SYNC_REPEAT = ["veto", "repeat request returned"]
GRANT = ["grant", "exiting"]
# mode: (SUMIE_QUIT_TRIGGER, expected phases in order)
CASES = {
    "menu": ("menu", FIRST + SYNC_REPEAT + ["reaped"] + GRANT),
    "key": ("key", FIRST + ["repeat request returned", "veto", "reaped"] + GRANT),
    "cg": ("cg", FIRST + ["repeat request returned", "veto", "reaped"] + GRANT),
    "nested": ("nested", FIRST + SYNC_REPEAT + ["reaped", "handler busy",
                                                "nested run loop returned"] + GRANT),
    "synchronous": ("menu", FIRST + SYNC_REPEAT + ["reaped", "grant"]),
    "nested-queued": ("nested-queued", FIRST + SYNC_REPEAT + ["reaped"]),
}
# The negative controls must fail at winit's re-entrancy check from this frame, nothing else.
ORIGIN = {"synchronous": "app_will_terminate", "nested-queued": "queue_closure"}
PANIC = re.compile(r"src/platform_impl/macos/event_handler\.rs:\d+:\d+:\n"
                   r"tried to handle event while another event is currently being handled")

mode, binary, evidence = sys.argv[1:]
trigger, expected = CASES[mode]
log = Path(evidence) / f"quit-{mode}.log"
started = time.monotonic()
with log.open("wb") as stream:
    status = subprocess.run([binary], stdout=stream, stderr=subprocess.STDOUT, timeout=20,
                            env={**os.environ, "RUST_BACKTRACE": "0",
                                 "SUMIE_QUIT_TRIGGER": trigger}).returncode
elapsed = time.monotonic() - started
text = log.read_text(errors="replace")
if mode in ORIGIN:
    # Reject build errors, arbitrary crashes, and watchdog kills. Match the panic's origin too.
    passed = status != 0 and PANIC.search(text) is not None and ORIGIN[mode] in text
else:
    passed = status == 0 and "panicked" not in text
phases = [s.removeprefix("quit: ") for s in text.splitlines() if s.startswith("quit: ")]
if mode == "cg" and status == 3 and phases == ["no post-event access"]:
    # Preflight only: never request access. The owner runs this case with a physical Cmd+Q.
    sys.exit(f"{mode}: unsupported, no existing post-event access")
if not passed or phases != expected:
    sys.exit(f"{mode}: exit {status}, phases {phases}; see {log}")
print(f"{mode}: exit {status} in {elapsed:.2f} s, {' -> '.join(phases)}")
