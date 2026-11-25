#!/bin/bash
# usage: quit.sh EVIDENCE_DIR {menu|key|cg|nested|nested-queued|synchronous}
# Builds the Quit fixture against winit 0.30.13 from the workspace lock plus the reviewed patch.
set -eu -o pipefail
root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture="$root/scripts/macos-spike/quit"
evidence=${1:?supply an absolute evidence directory}
mode=${2:?select a case from check.py}
: "${CARGO_TARGET_DIR:?supply an absolute target directory outside the checkout}"
case $mode in
    menu | key | cg | nested | nested-queued | synchronous) ;;
    *) exit 2 ;;
esac
# The workspace lock records the crates.io checksum; Cargo's own cache supplies the archive.
checksum=$(sed -n '/^name = "winit"$/{n;/^version = "0.30.13"$/{n;n;s/^checksum = "\(.*\)"$/\1/p;};}' "$root/Cargo.lock")
[[ $checksum =~ ^[0-9a-f]{64}$ ]] || { echo "quit: winit 0.30.13 is not locked" >&2; exit 2; }
cargo fetch --locked --manifest-path "$root/Cargo.toml" --target aarch64-apple-darwin
archive=$(ls "${CARGO_HOME:-$HOME/.cargo}"/registry/cache/*/winit-0.30.13.crate | head -n 1)
printf '%s  %s\n' "$checksum" "$archive" | shasum -a 256 -c -
fork="$fixture/vendor/winit-0.30.13"
mkdir -p "$evidence"
# Leave any pre-existing fork alone. Only this invocation owns the fresh extraction.
mkdir "$fork"
trap 'rm -rf "$fork"' EXIT
tar -xzf "$archive" -C "$fork" --strip-components=1
patch -s -d "$fork" -p1 -F 0 < "$fixture/vendor/ordinary-quit.patch"
if [[ $mode == synchronous ]]; then
    patch -s -d "$fork" -p1 -F 0 < "$fixture/vendor/synchronous-control.patch"
fi
ulimit -c 0
cargo build --locked --release --manifest-path "$fixture/Cargo.toml"
python3 -I "$fixture/check.py" "$mode" "$CARGO_TARGET_DIR/release/sumie-quit-probe" "$evidence"
