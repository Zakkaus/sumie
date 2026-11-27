#!/bin/sh
# Assemble an ad-hoc signed sumie.app from a release binary, and optionally a .dmg.
#
#   scripts/macos/bundle.sh [--dmg] BINARY OUTPUT_DIR [VERSION]
#
# BINARY is the arm64 `sumie` executable (cargo build --release). OUTPUT_DIR receives
# sumie.app and, with --dmg, sumie-VERSION-aarch64-macos.dmg. VERSION defaults to the
# workspace version in Cargo.toml. SUMIE_REQUIRE_ICON=1 turns a missing icon toolchain
# (rsvg-convert, iconutil) into an error; the release workflow sets it.
set -eu

make_dmg=false
if [ "${1-}" = --dmg ]; then
    make_dmg=true
    shift
fi
if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
    printf 'usage: %s [--dmg] BINARY OUTPUT_DIR [VERSION]\n' "$0" >&2
    exit 2
fi
binary=$1
out=$2
root=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd -P)
version=${3:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)}
version=${version#v}

[ -f "$binary" ] && [ -x "$binary" ] || { echo "bundle: $binary is not an executable file" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo 'bundle: codesign and hdiutil exist only on macOS' >&2; exit 2; }
case $version in
    '' | *[!0-9A-Za-z.+-]*) echo "bundle: unusable version '$version'" >&2; exit 2 ;;
esac

mkdir -p "$out"
out=$(CDPATH='' cd "$out" && pwd -P)
app=$out/sumie.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/sumie"
chmod 755 "$app/Contents/MacOS/sumie"
cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"

# CFBundleVersion must be dotted integers; a pre-release suffix stays in the short string.
build=${version%%[-+]*}
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>sumie</string>
    <key>CFBundleIconFile</key>
    <string>sumie</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.zakkaus.sumie</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>sumie</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$build</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key>
    <string>13.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" >/dev/null

# The icon master is assets/icons/macos/sumie.svg on a 1024 px grid. Only an SVG renderer
# turns it into the iconset sizes, so a Mac without rsvg-convert (brew install librsvg)
# builds an icon-less bundle unless SUMIE_REQUIRE_ICON=1.
if command -v rsvg-convert >/dev/null && command -v iconutil >/dev/null; then
    iconset=$(mktemp -d)/sumie.iconset
    mkdir "$iconset"
    for size in 16 32 128 256 512; do
        double=$((size * 2))
        rsvg-convert -w "$size" -h "$size" "$root/assets/icons/macos/sumie.svg" \
            -o "$iconset/icon_${size}x${size}.png"
        rsvg-convert -w "$double" -h "$double" "$root/assets/icons/macos/sumie.svg" \
            -o "$iconset/icon_${size}x${size}@2x.png"
    done
    iconutil -c icns "$iconset" -o "$app/Contents/Resources/sumie.icns"
    rm -rf "$(dirname "$iconset")"
elif [ "${SUMIE_REQUIRE_ICON-}" = 1 ]; then
    echo 'bundle: rsvg-convert and iconutil are required to build the icon' >&2
    exit 1
else
    echo 'bundle: no rsvg-convert; building without an icon' >&2
fi

# Ad-hoc signature: it satisfies Apple silicon's requirement for a signed executable, but
# it is not a Developer ID, so Gatekeeper still quarantines a downloaded copy.
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"

if [ "$make_dmg" = true ]; then
    dmg=$out/sumie-$version-aarch64-macos.dmg
    rm -f "$dmg"
    stage=$(mktemp -d)
    cp -R "$app" "$stage/sumie.app"
    ln -s /Applications "$stage/Applications"
    hdiutil create -volname sumie -srcfolder "$stage" -ov -format UDZO "$dmg" >&2
    rm -rf "$stage"
    printf '%s\n' "$dmg"
else
    printf '%s\n' "$app"
fi
