#!/bin/sh
set -eu

scripts=$(CDPATH='' cd "$(dirname "$0")" && pwd -P)
case ${1-} in
    -h|--help) exec python3 "$scripts/run.py" --help ;;
esac

base=${TMPDIR:-/tmp}
[ ! -d /scratch/ram ] || base=/scratch/ram
base=$(CDPATH='' cd -- "$base" && pwd -P)
runtime=$(mktemp -d "$base/sumie-rt.XXXXXX")
trap 'rm -rf "$runtime"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

unset DISPLAY WAYLAND_SOCKET QT_QPA_PLATFORM QT_QPA_PLATFORMTHEME KWIN_WAYLAND_NO_PERMISSION_CHECKS
export XDG_RUNTIME_DIR="$runtime" WAYLAND_DISPLAY=wl-sumie
export XDG_CONFIG_HOME="$runtime/config" XDG_DATA_HOME="$runtime/data"
export XDG_STATE_HOME="$runtime/state" XDG_CACHE_HOME="$runtime/cache"
mkdir "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME" "$XDG_CACHE_HOME"

dbus-run-session --config-file="$scripts/session.conf" -- python3 "$scripts/run.py" "$@"
