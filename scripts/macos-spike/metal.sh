#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "usage: $0 EVIDENCE_DIR [SCENARIO...]" >&2
    exit 2
fi
if [[ $(uname -s) != Darwin || $(uname -m) != arm64 ]]; then
    echo 'metal: requires an arm64 macOS runner with WindowServer' >&2
    exit 2
fi

scripts=$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
evidence=$(python3 - "$scripts" "$1" <<'PY'
from pathlib import Path
import sys
repo = Path(sys.argv[1]).parents[1]
evidence = Path(sys.argv[2]).resolve()
if evidence.is_relative_to(repo):
    sys.exit("metal: evidence must be outside the repository")
print(evidence)
PY
)
mkdir -p "$evidence"
build=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/sumie-metal.XXXXXX")
trap 'rm -rf "$build"' EXIT
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}
export CARGO_TARGET_DIR="$build/target"
target=aarch64-apple-darwin

{
    sw_vers
    uname -m
    printf 'ImageOS=%s\nImageVersion=%s\n' "${ImageOS:-unknown}" "${ImageVersion:-unknown}"
    cargo --version
    cp "$scripts/metal.Cargo.toml" "$build/Cargo.toml"
    cp "$scripts/metal.rs" "$build/metal.rs"
    cp "$scripts/../../Cargo.lock" "$build/Cargo.lock"
    cargo metadata --manifest-path "$build/Cargo.toml" --format-version 1 \
        --filter-platform "$target" > "$build/metadata.json"
    python3 - "$build/metadata.json" <<'PY'
import json
from pathlib import Path
import sys
packages = json.loads(Path(sys.argv[1]).read_text())["packages"]
for name, version in {"wgpu": "30.0.2", "wgpu-core": "30.0.2", "wgpu-hal": "30.0.2", "winit": "0.30.13"}.items():
    resolved = [pkg["version"] for pkg in packages if pkg["name"] == name]
    if resolved != [version]:
        sys.exit(f"metal: expected {name} {version}, resolved {resolved}")
    print(f"{name} {version}")
PY
    xcrun clang -arch arm64 -fobjc-arc -fblocks -Wall -Wextra -c "$scripts/metal.m" -o "$build/metal.o"
    cargo rustc --locked --manifest-path "$build/Cargo.toml" --target "$target" --bin metal-spike -- \
        -C "link-arg=$build/metal.o" -l framework=AppKit -l framework=Metal -l framework=QuartzCore
} 2>&1 | tee "$evidence/metal-build.log"

python3 -B "$scripts/metal.py" "$CARGO_TARGET_DIR/$target/debug/metal-spike" "$evidence" "${@:2}"
