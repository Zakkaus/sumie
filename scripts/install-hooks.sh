#!/bin/sh
set -u

root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P) || exit 2
git -C "$root" config --local core.hooksPath scripts/hooks || exit 2
printf 'Installed scripts/hooks for %s\n' "$root"
