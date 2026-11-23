#!/bin/sh
set -eu

scripts=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    printf 'usage: %s ARTIFACT_DIR [--startup-only]\n' "$0" >&2
    exit 2
fi

exec sh "$scripts/perf/run.sh" 1800 -- python3 -B "$scripts/perf/baseline.py" "$@"
