#!/usr/bin/env python3
"""Run the runner probe N times on this machine and summarize medians and spread."""

import json
import os
from pathlib import Path
import statistics
import subprocess
import sys

MS = 1_000_000
# report field: (probe file, path to the nanosecond value)
FIELDS = {
    "launch_to_first_present_ms": ("timing", ("launch_to_first_submission_notice_ns",)),
    "max_main_thread_ping_rtt_ms": ("timing", ("max_heartbeat_rtt_ns",)),
    "max_submission_gap_60s_ms": ("timing", ("max_submission_interval_ns",)),
    "close_to_reaped_exit_ms": ("timing", ("close", "request_to_reaped_exit_ns")),
}


def value(report, path):
    for key in path:
        report = report[key]
    return report / MS


def main():
    runs, evidence = int(sys.argv[1]), Path(sys.argv[2]).resolve()
    script = Path(__file__).resolve().parent / "runner.sh"
    samples, capabilities, failures = {name: [] for name in FIELDS}, [], 0
    for run in range(1, runs + 1):
        directory = evidence / f"run-{run}"
        failures += subprocess.call(["bash", script, directory]) != 0
        reports = {name: json.loads((directory / f"runner-{name}.json").read_text())
                   for name in ("timing", "capabilities", "environment")}
        for name, (probe, path) in FIELDS.items():
            if reports[probe].get("status") == "measured":
                samples[name].append(value(reports[probe], path))
        capabilities.append(reports["capabilities"].get("capabilities", {}))
    summary = {
        "runs": runs,
        "failed_runs": failures,
        "metrics": {name: {"median": statistics.median(values), "min": min(values),
                           "max": max(values), "samples": values}
                    for name, values in samples.items() if values},
        "capabilities_in_every_run": {
            key: sorted({run.get(key, "missing") for run in capabilities})
            for key in sorted(set().union(*capabilities))
        },
        "load_average_at_end": os.getloadavg(),
    }
    (evidence / "baseline.json").write_text(json.dumps(summary, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(summary, indent=2))
    return int(failures != 0 or any(len(v) != runs for v in samples.values()))


if __name__ == "__main__":
    sys.exit(main())
