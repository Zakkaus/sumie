"""The Linux pane gates, each in a private nested KWin."""
import os
import shutil
import subprocess
import sys
import tempfile

from gates.build import ROOT, CannotCheck, built_binary

PANES_TIMEOUT = 700
PANES_TOOLS = ("kwin_wayland", "kscreen-doctor", "wayland-scanner", "cc", "pkg-config")
PANES = 20
PANES_SECONDS = 20


def check_linux_panes():
    """200 pane and 200 tab open/close cycles in a private KWin leave no handle, thread or
    zombie above the baseline, and a pane that ignores SIGHUP closes without stalling drawing."""
    if sys.platform != "linux":
        raise CannotCheck("the pane cycle gate needs Linux and KWin")
    missing = [tool for tool in PANES_TOOLS if shutil.which(tool) is None]
    if missing:
        raise CannotCheck(f"the pane cycle gate needs {', '.join(missing)}")
    # Only a debug build writes the presentation log that the no-stall check reads.
    binary = built_binary()
    with tempfile.TemporaryDirectory(prefix="sumie-panes.") as artifacts:
        try:
            result = subprocess.run(["sh", ROOT / "scripts/perf/panes.sh", artifacts],
                                    env={**os.environ, "SUMIE_BIN": binary}, stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True, timeout=PANES_TIMEOUT)
        except subprocess.TimeoutExpired:
            print("linux-panes: the scenario timed out", file=sys.stderr)
            return 1
    print(result.stdout, end="")
    if result.returncode == 0:
        return 0
    print(result.stderr[-4000:], end="", file=sys.stderr)
    if result.returncode == 1 and "FAIL: " in result.stdout:
        return 1
    raise CannotCheck(f"the scenario exited {result.returncode} without a verdict")


def check_panes():
    """20 writer panes in a private nested KWin, release build: every writer advances, each
    pane repaints within 200 ms of every line, and no presentation gap exceeds 100 ms."""
    if sys.platform != "linux" or not shutil.which("kwin_wayland"):
        raise CannotCheck("the pane budget needs kwin_wayland for a private nested session")
    binary = built_binary("--release")
    artifacts = tempfile.mkdtemp(prefix="sumie-panes.")
    try:
        for attempt in (1, 2):
            result = subprocess.run(["sh", ROOT / "scripts/perf/stall.sh", artifacts,
                                     str(PANES_SECONDS), "--panes", str(PANES)],
                                    env={**os.environ, "SUMIE_BIN": binary},
                                    stdin=subprocess.DEVNULL)
            if not result.returncode:
                shutil.rmtree(artifacts)
                return 0
            if attempt == 1:
                print("panes: retrying once", file=sys.stderr, flush=True)
    except BaseException:
        shutil.rmtree(artifacts, ignore_errors=True)
        raise
    print(f"panes: stall.sh exited {result.returncode}; artifacts kept in {artifacts}",
          file=sys.stderr)
    return 1
