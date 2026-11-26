#!/bin/sh
set -u

case ${1-}:$# in
    :0) fast=false ;;
    --fast:1) fast=true ;;
    *) printf 'usage: %s [--fast]\n' "$0" >&2; exit 2 ;;
esac

scripts=$(CDPATH='' cd "$(dirname "$0")" && pwd -P) || exit 2
command -v python3 >/dev/null || { echo 'check-all: python3 not found' >&2; exit 2; }
# The macos-ci feature links AppKit, so only a Mac can lint it or run its window scenarios.
macos_ci=
macos_window=
if [ "$(uname -s)" = Darwin ]; then
    macos_ci=clippy-macos-ci
    macos_window='macos-wake macos-keys macos-pasteboard macos-resize macos-idle macos-flood macos-timeout macos-throughput macos-loading
        macos-close macos-repeat macos-fatal macos-system macos-stuck macos-exit'
fi
# The pane cycle gate drives a private KWin, so only a Linux host with KWin can run it.
linux_panes=
if [ "$(uname -s)" = Linux ] && command -v kwin_wayland >/dev/null; then
    linux_panes=linux-panes
fi
# The pane budget runs in a private nested KWin for about a minute, so only the full gate
# on a Linux host with KWin runs it.
panes=
if [ "$fast" = false ] && [ "$(uname -s)" = Linux ] && command -v kwin_wayland >/dev/null; then
    panes=panes
elif [ "$fast" = false ] && [ "$(uname -s)" = Linux ]; then
    echo 'check-all: panes skipped (no kwin_wayland)' >&2
fi
for gate in structure terminfo shell fmt clippy $macos_ci test $macos_window $panes; do
    python3 "$scripts/check-gate.py" "$gate" || exit "$?"
done

if [ "$fast" = false ]; then
    for gate in $linux_panes; do
        python3 "$scripts/check-gate.py" "$gate" || exit "$?"
    done
    exec "$scripts/negative-controls.sh"
fi
