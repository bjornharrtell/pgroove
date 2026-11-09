#!/bin/sh
# Builds an AppImage from target/release/pgroove.
# Usage: packaging/build-appimage.sh <output-file>
# Set APPIMAGETOOL to a local appimagetool binary to skip the download (used on CI otherwise).
set -eu
out="${1:?usage: build-appimage.sh <output-file>}"
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

appdir="$work/AppDir"
mkdir -p "$appdir/usr/bin"
cp "$root/target/release/pgroove" "$appdir/usr/bin/pgroove"
cp "$root/packaging/pgroove.desktop" "$appdir/pgroove.desktop"
cp "$root/packaging/pgroove.svg" "$appdir/pgroove.svg"
cat > "$appdir/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/pgroove" "$@"
RUN
chmod +x "$appdir/AppRun"

tool="${APPIMAGETOOL:-$work/appimagetool}"
if [ ! -x "$tool" ]; then
  curl -fsSL -o "$tool" \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$tool"
fi
# --appimage-extract-and-run avoids needing FUSE on CI runners.
ARCH=x86_64 "$tool" --appimage-extract-and-run "$appdir" "$out"
