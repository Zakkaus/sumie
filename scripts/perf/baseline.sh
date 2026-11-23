#!/bin/sh
set -eu

scripts=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
if [ "$#" -ne 1 ]; then
    printf 'usage: %s ARTIFACT_DIR\n' "$0" >&2
    exit 2
fi

unset WAYLAND_DEBUG
exec "$scripts/nested/run.sh" 1.25 1800 -- python3 "$scripts/perf/baseline.py" "$1"
