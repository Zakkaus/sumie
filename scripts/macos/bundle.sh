#!/bin/sh
# Assemble an ad-hoc signed sumie.app from a release binary, and optionally a .dmg.
#
#   scripts/macos/bundle.sh [--dmg] BINARY OUTPUT_DIR LICENSES [VERSION]
#
# BINARY is the arm64 `sumie` executable (cargo build --release). OUTPUT_DIR receives
# sumie.app and, with --dmg, sumie-VERSION-aarch64-macos.dmg. VERSION defaults to the
# workspace version in Cargo.toml. SUMIE_REQUIRE_ICON=1 turns a missing icon toolchain
# (rsvg-convert, iconutil) into an error; the release workflow sets it.
#
# The bundle carries JetBrains Mono, the default font.family, because stock macOS lacks it.
# The release zip is downloaded and checked against FONT_SHA256; SUMIE_FONT_ZIP points at an
# already-downloaded copy of the same file for offline builds (the hash is still enforced).
set -eu

# JetBrains Mono, SIL Open Font License 1.1. Bump the three values together.
FONT_VERSION=2.304
FONT_URL=https://github.com/JetBrains/JetBrainsMono/releases/download/v$FONT_VERSION/JetBrainsMono-$FONT_VERSION.zip
FONT_SHA256=6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf

make_dmg=false
if [ "${1-}" = --dmg ]; then
    make_dmg=true
    shift
fi
if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then
    printf 'usage: %s [--dmg] BINARY OUTPUT_DIR LICENSES [VERSION]\n' "$0" >&2
    exit 2
fi
binary=$1
out=$2
licenses=$3
root=$(CDPATH='' cd "$(dirname "$0")/../.." && pwd -P)
version=${4:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)}
version=${version#v}

[ -f "$binary" ] && [ -x "$binary" ] || { echo "bundle: $binary is not an executable file" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo 'bundle: codesign and hdiutil exist only on macOS' >&2; exit 2; }
[ -s "$licenses" ] || { echo "bundle: missing or empty license report: $licenses" >&2; exit 2; }
case $version in
    '' | *[!0-9A-Za-z.+-]*) echo "bundle: unusable version '$version'" >&2; exit 2 ;;
esac

# Keep prerelease and build metadata in the archive name, not in the plist.
build=${version%%[-+]*}
if ! printf '%s\n' "$build" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "bundle: unusable bundle version '$version' (expected major.minor.patch)" >&2
    exit 2
fi

mkdir -p "$out"
out=$(CDPATH='' cd "$out" && pwd -P)
app=$out/sumie.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/sumie"
chmod 755 "$app/Contents/MacOS/sumie"
cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"
cp "$licenses" "$app/Contents/Resources/THIRD-PARTY-LICENSES"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>ATSApplicationFontsPath</key>
    <string>Fonts</string>
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
    <string>$build</string>
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

# JetBrains Mono: the static TTFs (Core Text names the family "JetBrains Mono" for all four
# faces, which the variable files would not give a bold or italic request) and its license.
# ATSApplicationFontsPath in Info.plist registers Resources/Fonts for this app only.
fonts=$app/Contents/Resources/Fonts
work=$(mktemp -d)
zip=${SUMIE_FONT_ZIP-}
if [ -z "$zip" ]; then
    zip=$work/JetBrainsMono-$FONT_VERSION.zip
    curl -fsSL --retry 3 -o "$zip" "$FONT_URL" || { echo "bundle: cannot download $FONT_URL" >&2; exit 1; }
fi
[ -f "$zip" ] || { echo "bundle: SUMIE_FONT_ZIP $zip is not a file" >&2; exit 2; }
actual=$(shasum -a 256 "$zip" | cut -d ' ' -f 1)
if [ "$actual" != "$FONT_SHA256" ]; then
    echo "bundle: $zip has SHA-256 $actual, expected $FONT_SHA256 (JetBrains Mono $FONT_VERSION)" >&2
    exit 1
fi
unzip -q "$zip" -d "$work/x"
mkdir -p "$fonts"
for face in Regular Bold Italic BoldItalic; do
    cp "$work/x/fonts/ttf/JetBrainsMono-$face.ttf" "$fonts/"
done
cp "$work/x/OFL.txt" "$fonts/OFL.txt"
rm -rf "$work"

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
