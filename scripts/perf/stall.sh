#!/bin/sh
set -eu

perf=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
if [ "$#" -lt 2 ]; then
    printf 'usage: %s ARTIFACT_DIR SECONDS [--panes N] [--refresh-hz HZ] [--present-log]\n' "$0" >&2
    exit 2
fi
duration=$2
case $duration in
    ''|0*|*[!0-9]*) echo 'stall: SECONDS must be a positive integer' >&2; exit 2 ;;
esac
# Startup, building the layout and the final frame log flush take up to a minute.
exec sh "$perf/run.sh" "$((duration + 90))" --injector -- python3 -B "$perf/stall.py" "$@"
