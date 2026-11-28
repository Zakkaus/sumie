import os
from pathlib import Path
import re
import subprocess
import sys
import time

# Phases are the fixture's `quit: <phase>` lines and winit's `winit::quit: <phase>` decisions.
FIRST = ["intercept", "first request returned", "draining"]
# winit forwards the repeated ordinary Quit too, so the application could ask again.
SYNC_REPEAT = ["veto", "repeat request returned", "asked again"]
GRANT = ["grant", "exiting"]
# A system quit is answered NSTerminateLater and drained inside AppKit's modal-panel loop. The
# self-sent Apple event is dispatched inside the send, so the send returns only after the reply.
SYSTEM = ["request", "later", "draining in later", "reaped", "reply", "request returned", "exiting"]
# mode: (SUMIE_QUIT_TRIGGER, expected phases in order)
CASES = {
    "menu": ("menu", FIRST + SYNC_REPEAT + ["reaped"] + GRANT),
    "key": ("key", ["first request returned", "intercept", "draining", "repeat request returned",
                    "veto", "asked again", "reaped"] + GRANT),
    "cg": ("cg", ["first request returned", "intercept", "draining", "repeat request returned",
                  "veto", "asked again", "reaped"] + GRANT),
    "nested": ("nested", FIRST + SYNC_REPEAT + ["reaped", "handler busy",
                                                "nested run loop returned"] + GRANT),
    "logout": ("logout", SYSTEM),
    "shutdown": ("shutdown", SYSTEM),
    # Any keyAEQuitReason is a system quit, including kAEQuitAll and codes winit does not know.
    "quit-all": ("quit-all", SYSTEM),
    "unknown-reason": ("unknown-reason", SYSTEM),
    # A system quit while an ordinary one drains is answered Later and requested again.
    "upgrade": ("upgrade", FIRST + ["request", "later"] + SYSTEM[2:]),
    # AppKit does not ask the delegate again while a reply is pending: no veto, no second request.
    "menu-during-later": ("menu-during-later", SYSTEM[:3] + ["menu returned"] + SYSTEM[3:]),
    "synchronous": ("menu", FIRST + SYNC_REPEAT + ["reaped", "grant"]),
    "nested-queued": ("nested-queued", FIRST + SYNC_REPEAT + ["reaped"]),
    "system-cancel": ("logout", ["request", "later", "request returned", "veto during system quit"]),
}
# The negative controls must fail at winit's re-entrancy check from this frame, nothing else.
ORIGIN = {"synchronous": "app_will_terminate", "nested-queued": "maybe_queue_event"}
PANIC = re.compile(r"src/platform_impl/macos/event_handler\.rs:\d+:\d+:\n"
                   r"tried to handle event while another event is currently being handled")
TIMEOUT = 20
PHASE = re.compile(r"^(?:winit::(?:quit|run_after_handler)|quit): (.*)$", re.MULTILINE)

mode, binary, evidence = sys.argv[1:]
trigger, expected = CASES[mode]
log = Path(evidence) / f"quit-{mode}.log"
started = time.monotonic()
with log.open("wb") as stream:
    try:
        status = subprocess.run([binary], stdout=stream, stderr=subprocess.STDOUT,
                                timeout=TIMEOUT, env={**os.environ, "RUST_BACKTRACE": "0",
                                                      "SUMIE_QUIT_TRIGGER": trigger}).returncode
    except subprocess.TimeoutExpired:
        status = None
elapsed = time.monotonic() - started
text = log.read_text(errors="replace")
phases = PHASE.findall(text)
if status is None:
    # Name the first expected phase that never came, if the run followed the script until then.
    if phases == expected[:len(phases)] and len(phases) < len(expected):
        after = phases[-1] if phases else "launch"
        sys.exit(f"{mode}: no {expected[len(phases)]} after {after} within {TIMEOUT} s; see {log}")
    sys.exit(f"{mode}: no exit within {TIMEOUT} s, phases {phases}; see {log}")
if mode in ORIGIN:
    # Reject build errors, arbitrary crashes, and watchdog kills. Match the panic's origin too.
    passed = status != 0 and PANIC.search(text) is not None and ORIGIN[mode] in text
elif mode == "system-cancel":
    # The fixture exits 1 itself when a system quit drains outside NSTerminateLater.
    passed = status == 1 and "panicked" not in text
else:
    passed = status == 0 and "panicked" not in text
    if "veto during system quit" in phases:
        sys.exit(f"{mode}: veto during system quit; see {log}")
if mode == "cg" and status == 3 and phases == ["no post-event access"]:
    # Preflight only: never request access. The owner runs this case with a physical Cmd+Q.
    sys.exit(f"{mode}: unsupported, no existing post-event access")
if not passed or phases != expected:
    sys.exit(f"{mode}: exit {status}, phases {phases}; see {log}")
print(f"{mode}: exit {status} in {elapsed:.2f} s, {' -> '.join(phases)}")
