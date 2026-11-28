#!/bin/bash
# usage: quit.sh EVIDENCE_DIR CASE (see CASES in check.py)
# Builds the Quit fixture against the winit fork that the workspace pins in [patch.crates-io].
set -eu -o pipefail
root="$(cd "$(dirname "$0")/../.." && pwd -P)"
fixture="$root/scripts/macos-spike/quit"
evidence=${1:?supply an absolute evidence directory}
mode=${2:?select a case from check.py}
: "${CARGO_TARGET_DIR:?supply an absolute target directory outside the checkout}"
case $mode in
    menu | key | cg | nested | nested-queued) control= ;;
    logout | shutdown | menu-during-later | quit-all | unknown-reason | upgrade) control= ;;
    synchronous) control=synchronous-control.patch ;;
    system-cancel) control=system-cancel-control.patch ;;
    *) exit 2 ;;
esac
# The workspace lock records the fork commit; Cargo's git database supplies it, and git checks
# the tree against that commit.
rev=$(sed -n '/^name = "winit"$/{n;/^version = "0.30.13"$/{n;s/^source = "git+.*#\([0-9a-f]\{40\}\)"$/\1/p;};}' "$root/Cargo.lock")
[[ $rev =~ ^[0-9a-f]{40}$ ]] || { echo "quit: winit 0.30.13 is not pinned to a git commit" >&2; exit 2; }
cargo fetch --locked --manifest-path "$root/Cargo.toml" --target aarch64-apple-darwin
db=
for candidate in "${CARGO_HOME:-$HOME/.cargo}"/git/db/*/; do
    if git -C "$candidate" cat-file -e "$rev^{commit}" 2>/dev/null; then db=$candidate; break; fi
done
[[ -n $db ]] || { echo "quit: commit $rev is not in Cargo's git database" >&2; exit 2; }
# The reviewed patch is the fork's whole diff from upstream winit v0.30.13.
upstream=e9809ef54b18499bb4f2cac945719ecc2a61061b
git -C "$db" -c diff.noprefix=false diff --no-color --no-ext-diff "$upstream" "$rev" |
    cmp -s - "$root/third_party/winit/0001-macos.patch" ||
    { echo "quit: third_party/winit/0001-macos.patch is not the diff from v0.30.13 to $rev" >&2; exit 2; }
fork="$fixture/vendor/winit-0.30.13"
mkdir -p "$evidence"
# Leave any pre-existing fork alone. Only this invocation owns the fresh extraction.
mkdir "$fork"
trap 'rm -rf "$fork"' EXIT
# Fresh mtimes: Cargo judges freshness by mtime, and a control build may be newer than the commit.
git -C "$db" archive --format=tar "$rev" | tar -xmf - -C "$fork"
if [[ -n $control ]]; then
    patch -s -d "$fork" -p1 -F 0 < "$fixture/vendor/$control"
fi
ulimit -c 0
cargo build --locked --release --manifest-path "$fixture/Cargo.toml"
python3 -I "$fixture/check.py" "$mode" "$CARGO_TARGET_DIR/release/sumie-quit-probe" "$evidence"
