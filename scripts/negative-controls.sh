#!/bin/sh
set -u

if [ "$#" -ne 0 ]; then
    printf 'usage: %s\n' "$0" >&2
    exit 2
fi
scripts=$(CDPATH='' cd "$(dirname "$0")" && pwd -P) || exit 2
command -v python3 >/dev/null || { echo 'negative controls: python3 not found' >&2; exit 2; }
exec python3 "$scripts/negative-controls.py"
