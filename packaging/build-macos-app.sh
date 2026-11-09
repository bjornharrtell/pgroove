#!/bin/sh
# Builds pgroove.app from target/release/pgroove, then a .dmg and a .tar.gz of the app.
# Usage: packaging/build-macos-app.sh <version, e.g. 0.5.1> <output-prefix>
# Writes <output-prefix>.dmg (for people) and <output-prefix>.tar.gz (for the in-app updater).
# Needs macOS (sips, iconutil, hdiutil). The app is not signed: the binary keeps the ad-hoc
# signature the linker gives it, which is what lets the updater swap the binary in place.
set -eu
version="${1:?usage: build-macos-app.sh <version> <output-prefix>}"
prefix="${2:?usage: build-macos-app.sh <version> <output-prefix>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

app="$work/dist/pgroove.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$root/target/release/pgroove" "$app/Contents/MacOS/pgroove"
sed "s/@VERSION@/$version/g" "$root/packaging/Info.plist" > "$app/Contents/Info.plist"

# Icon: an .icns made from the 256px PNG (the larger sizes are scaled up from it).
iconset="$work/pgroove.iconset"
mkdir "$iconset"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$root/packaging/pgroove.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
done
for s in 16 32 128 256; do
  d=$((s * 2))
  sips -z "$d" "$d" "$root/packaging/pgroove.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/pgroove.icns"

# Tarball for the updater: it takes the pgroove binary out of it by name.
tar -C "$work/dist" -czf "$prefix.tar.gz" pgroove.app

# Disk image with an Applications shortcut to drag the app onto.
ln -s /Applications "$work/dist/Applications"
cp "$root/README.md" "$work/dist/README.md"
hdiutil create -volname pgroove -srcfolder "$work/dist" -ov -format UDZO "$prefix.dmg"
