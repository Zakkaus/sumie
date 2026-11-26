"""The probe runner every negative control goes through: it edits one file, runs one gate,
checks the exit and the marker, and restores the file."""
from contextlib import ExitStack
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "scripts/check-gate.py"


class GateUnavailable(Exception):
    pass


def git_status():
    return subprocess.check_output(
        ["git", "-C", ROOT, "status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )


def append(text):
    return lambda original: original + "\n" + text


def replace_once(old, new):
    def change(original):
        if original.count(old) != 1:
            raise RuntimeError(f"expected one anchor {old!r}")
        return original.replace(old, new, 1)
    return change


def restore(path, original):
    path.write_bytes(original)
    if path.read_bytes() != original:
        raise RuntimeError(f"could not restore {path}")


def probe(name, gate, file, change, marker, expected=1, *, new=False):
    path = ROOT / file
    original = b"" if new else path.read_bytes()
    edited = change(original.decode()).encode()
    if edited == original:
        raise RuntimeError(f"{name}: probe did not change {file}")
    print(f"\n== probe {name}: {file} ({gate})", flush=True)
    with ExitStack() as cleanup:
        if new:
            if not path.parent.is_dir():
                path.parent.mkdir()
                cleanup.callback(path.parent.rmdir)
            with path.open("xb") as handle:
                cleanup.callback(path.unlink)
                handle.write(edited)
        else:
            cleanup.callback(restore, path, original)
            path.write_bytes(edited)
        if path.read_bytes() != edited:
            raise RuntimeError(f"{name}: probe did not land in {file}")
        result = subprocess.run([sys.executable, GATE, gate], cwd=ROOT, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(result.stdout, end="")
        if result.returncode == 2:
            raise GateUnavailable(f"{name}: {gate} could not run (exit 2)")
        if result.returncode != expected or marker not in result.stdout:
            raise RuntimeError(f"{name}: expected exit {expected} and {marker!r}, got exit {result.returncode}")
    print(f"{name}: {'RED' if expected else 'GREEN'} as expected (exit {result.returncode}); restored {file}")
