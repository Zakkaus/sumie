#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 1 ]]; then
    echo 'usage: bash scripts/macos-spike/runner.sh EVIDENCE_DIR' >&2
    exit 2
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo 'the runner probe requires native arm64 macOS' >&2
    exit 2
fi

scripts=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
bin=${SUMIE_MACOS_SPIKE_BIN:-${CARGO_TARGET_DIR:-target}/release/sumie}
if [[ ! -x $bin || $(xcrun lipo -archs "$bin") != arm64 ]]; then
    echo 'build the native arm64 release binary with --features macos-ci before running the probe' >&2
    exit 2
fi

scratch=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sumie-runner.XXXXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
xcrun --sdk macosx swiftc -O -target arm64-apple-macosx13.0 \
    "$scripts/runner-pixel.swift" -o "$scratch/check-pixel"
python3 "$scripts/runner.py" "$bin" "$1" "$scratch"
