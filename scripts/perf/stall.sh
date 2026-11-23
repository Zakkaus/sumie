#!/bin/sh
set -eu

perf=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    printf 'usage: %s ARTIFACT_DIR [SECONDS (default: 60)]\n' "$0" >&2
    exit 2
fi
duration=${2-60}
case $duration in
    ''|0*|*[!0-9]*) echo 'stall: SECONDS must be a positive integer' >&2; exit 2 ;;
esac
exec sh "$perf/run.sh" "$((duration + 30))" -- python3 -B "$perf/stall.py" "$1" "$duration"
