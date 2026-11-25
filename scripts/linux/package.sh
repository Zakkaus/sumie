#!/bin/sh
# Pack a release binary and its data files into sumie-VERSION-ARCH-linux.tar.gz.
#
#   scripts/linux/package.sh BINARY OUTPUT_DIR VERSION ARCH
#
# ARCH is x86_64 or aarch64. The tarball unpacks into one directory holding the stripped
# binary, LICENSE, README.md, terminfo/, shell-integration/ and a share/ tree for the
# desktop entry and icons. The binary installs its own terminfo and shell integration on
# first launch, so the copies in the tarball are for packagers and for reading.
set -eu

if [ "$#" -ne 4 ]; then
    printf 'usage: %s BINARY OUTPUT_DIR VERSION ARCH\n' "$0" >&2
    exit 2
fi
binary=$1
out=$2
version=${3#v}
arch=$4
root=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd -P)

[ -f "$binary" ] && [ -x "$binary" ] || { echo "package: $binary is not an executable file" >&2; exit 2; }
case $version in
    '' | *[!0-9A-Za-z.+-]*) echo "package: unusable version '$version'" >&2; exit 2 ;;
esac
case $arch in
    x86_64 | aarch64) ;;
    *) echo "package: unsupported architecture '$arch'" >&2; exit 2 ;;
esac

mkdir -p "$out"
out=$(CDPATH='' cd "$out" && pwd -P)
name=sumie-$version-$arch-linux
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
dir=$stage/$name
mkdir -p "$dir/share/applications" "$dir/share/icons/hicolor/scalable/apps" \
    "$dir/share/icons/hicolor/symbolic/apps"

cp "$binary" "$dir/sumie"
strip "$dir/sumie"
cp "$root/LICENSE" "$root/README.md" "$dir/"
cp -R "$root/terminfo" "$root/shell-integration" "$dir/"
cp "$root/assets/sumie.desktop" "$dir/share/applications/"
cp "$root/assets/icons/hicolor/scalable/apps/sumie.svg" "$dir/share/icons/hicolor/scalable/apps/"
cp "$root/assets/icons/hicolor/symbolic/apps/sumie-symbolic.svg" \
    "$dir/share/icons/hicolor/symbolic/apps/"

# Fixed owner, sorted names and a zero mtime keep the archive the same for the same inputs.
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='@0' -C "$stage" -cf - "$name" |
    gzip -9n > "$out/$name.tar.gz"
printf '%s\n' "$out/$name.tar.gz"
