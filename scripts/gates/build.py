"""The cargo helpers the gate modules share."""
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
CARGO_RECORDS = ("compiler-artifact", "compiler-message", "build-script-executed", "build-finished")


class CannotCheck(Exception):
    pass


def run(*args):
    return subprocess.run(args, cwd=ROOT, capture_output=True)


def cargo_record(line):
    if not line.startswith("{"):
        return None
    try:
        record = json.loads(line)
    except json.JSONDecodeError:
        return None
    if isinstance(record, dict) and record.get("reason") in CARGO_RECORDS:
        return record
    return None


def built_binary(*features):
    result = run("cargo", "build", "--locked", *features, "--message-format=json")
    if result.returncode:
        print(result.stderr.decode(errors="replace"), end="", file=sys.stderr)
        raise CannotCheck(f"cargo build exited {result.returncode}")
    for line in result.stdout.decode(errors="replace").splitlines():
        record = cargo_record(line)
        if (record and record["reason"] == "compiler-artifact" and record.get("executable")
                and record["target"]["name"] == "sumie"):
            return record["executable"]
    raise CannotCheck("cargo build did not report the sumie executable")


def runner_binary():
    return built_binary("--features", "macos-ci")
