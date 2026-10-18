#!/bin/sh
# Builds Editage.app and a disk image for Apple silicon Macs.
#
# Output in target/dist/:
#   Editage.app                          the application bundle
#   Editage-<version>-arm64.dmg          disk image (drag Editage to Applications)
#   Editage-<version>-arm64.dmg.sha256   its SHA-256 checksum
#
# The bundle is signed ad hoc (no Apple Developer ID) and is not notarized.
# See README.md › Installing for what that means when opening it.
#
# Environment:
#   BUILD_NUMBER  CFBundleVersion (default 1; CI passes the run number)
set -eu

cd "$(dirname "$0")/../.."

TARGET=aarch64-apple-darwin
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
BUILD=${BUILD_NUMBER:-1}
DIST=target/dist
APP=$DIST/Editage.app
DMG=$DIST/Editage-$VERSION-arm64.dmg

if [ -z "$VERSION" ]; then
    echo "error: could not read the workspace version from Cargo.toml" >&2
    exit 1
fi

cargo build --release --locked --target "$TARGET" -p editage-macos

rm -rf "$DIST"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/$TARGET/release/Editage" "$APP/Contents/MacOS/Editage"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@BUILD@/$BUILD/g" \
    packaging/macos/Info.plist > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' > "$APP/Contents/PkgInfo"
cp LICENSE-APACHE LICENSE-MIT "$APP/Contents/Resources/"

# The icon: every size macOS asks for, from the 1024 px master.
ICONSET=$DIST/AppIcon.iconset
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" packaging/macos/AppIcon.png \
        --out "$ICONSET/icon_${size}x${size}.png" > /dev/null
    double=$((size * 2))
    sips -z "$double" "$double" packaging/macos/AppIcon.png \
        --out "$ICONSET/icon_${size}x${size}@2x.png" > /dev/null
done
iconutil --convert icns --output "$APP/Contents/Resources/AppIcon.icns" "$ICONSET"
rm -rf "$ICONSET"

# Ad hoc signature with the hardened runtime. Apple silicon refuses to run
# unsigned code, and the signature seals the bundle's contents.
codesign --force --sign - --options runtime --timestamp=none "$APP"
codesign --verify --strict --verbose=2 "$APP"

STAGE=$DIST/dmg
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "Editage $VERSION" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
rm -rf "$STAGE"

(cd "$DIST" && shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256")
echo "Built $DMG"
