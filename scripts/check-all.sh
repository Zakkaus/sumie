#!/bin/sh
set -u

case ${1-}:$# in
    :0) fast=false ;;
    --fast:1) fast=true ;;
    *) printf 'usage: %s [--fast]\n' "$0" >&2; exit 2 ;;
esac

scripts=$(CDPATH='' cd "$(dirname "$0")" && pwd -P) || exit 2
command -v python3 >/dev/null || { echo 'check-all: python3 not found' >&2; exit 2; }
# The macos-ci feature links AppKit, so only a Mac can lint it.
macos_ci=
if [ "$(uname -s)" = Darwin ]; then
    macos_ci=clippy-macos-ci
fi
for gate in structure terminfo shell fmt clippy $macos_ci test; do
    python3 "$scripts/check-gate.py" "$gate" || exit "$?"
done

if [ "$fast" = false ]; then
    exec "$scripts/negative-controls.sh"
fi
