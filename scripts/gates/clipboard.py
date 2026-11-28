"""Exercise both Wayland selections on a private KWin socket."""
import shutil
import subprocess
import sys
from pathlib import Path

from gates.build import ROOT, CannotCheck, built_binary
from gates.panes import PANES_TOOLS


def check_linux_clipboard():
    if sys.platform != "linux":
        raise CannotCheck("the clipboard gate needs Linux and KWin")
    missing = [tool for tool in PANES_TOOLS if shutil.which(tool) is None]
    if missing:
        raise CannotCheck(f"the clipboard gate needs {', '.join(missing)}")
    pkgdata = subprocess.check_output(
        ["pkg-config", "--variable=pkgdatadir", "wayland-protocols"], text=True,
    ).strip()
    protocol = Path(pkgdata) / "staging/ext-data-control/ext-data-control-v1.xml"
    if not protocol.is_file():
        raise CannotCheck("ext-data-control-v1 protocol XML is missing")
    nested = ROOT / "scripts/nested"
    result = subprocess.run(
        ["sh", nested / "run.sh", "--no-session-bus", "1", "25", "--injector", "--",
         sys.executable, nested / "clipboard.py", built_binary(), protocol],
        cwd=ROOT, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=45,
    )
    print(result.stdout, end="")
    if result.returncode == 0:
        return 0
    print(result.stderr[-6000:], end="", file=sys.stderr)
    if result.returncode == 1 and "FAIL: " in result.stdout:
        return 1
    raise CannotCheck(f"the clipboard scenario exited {result.returncode} without a verdict")
