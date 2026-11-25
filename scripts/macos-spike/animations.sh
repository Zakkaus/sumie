#!/usr/bin/env bash
# Pings the idle runner during minimize, restore, live resize, a held menu and fullscreen.
# Posts CGEvents and reads Accessibility state with the existing grants; requests none.
set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo 'usage: bash scripts/macos-spike/animations.sh EVIDENCE_DIR [--sample]' >&2
    exit 2
fi
scripts=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
bin=${SUMIE_MACOS_SPIKE_BIN:-${CARGO_TARGET_DIR:-target}/release/sumie}
if [[ ! -x $bin ]]; then
    echo 'build the release binary with --features macos-ci first' >&2
    exit 2
fi
scratch=$(mktemp -d "${TMPDIR:-/tmp}/sumie-animations.XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
xcrun --sdk macosx swiftc -O "$scripts/cgtool.swift" -o "$scratch/cgtool"
SUMIE_CGTOOL=$scratch/cgtool python3 "$scripts/animations.py" "$bin" "$@"
