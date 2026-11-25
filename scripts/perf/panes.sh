#!/bin/sh
set -eu

perf=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
if [ "$#" -ne 1 ]; then
    printf 'usage: %s ARTIFACT_DIR\n' "$0" >&2
    exit 2
fi
# 400 cycles take about two minutes; the limit covers a loaded host.
exec sh "$perf/run.sh" 600 --input -- python3 -B "$perf/panes.py" "$1"
