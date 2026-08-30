#!/usr/bin/env bash
# Build a Reel AppImage from the release binary.
#
# The AppImage carries the reel binary, desktop entry and icon; like the
# tarball it expects ffmpeg (and optionally libmpv) from the system — Reel
# dlopens libmpv at runtime and shells out to ffmpeg, and bundling either
# would triple the download for no gain.
#
# Usage: scripts/appimage.sh [output-dir]
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${1:-target/appimage}"
BIN=target/release/reel
[ -x "$BIN" ] || { echo "build first: cargo build --release" >&2; exit 1; }

TOOL="$HOME/.cache/reel-build/appimagetool"
if [ ! -x "$TOOL" ]; then
  mkdir -p "$(dirname "$TOOL")"
  curl -fsSL -o "$TOOL" \
    https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
  chmod +x "$TOOL"
fi

DIR="$(mktemp -d)"
trap 'rm -rf "$DIR"' EXIT
APP="$DIR/Reel.AppDir"
mkdir -p "$APP/usr/bin" "$APP/usr/share/applications" "$APP/usr/share/icons/hicolor/scalable/apps"
cp "$BIN" "$APP/usr/bin/reel"
cp assets/reel.desktop "$APP/reel.desktop"
cp assets/reel.desktop "$APP/usr/share/applications/reel.desktop"
cp assets/reel-icon.svg "$APP/reel.svg"
cp assets/reel-icon.svg "$APP/usr/share/icons/hicolor/scalable/apps/reel.svg"
sed -i 's/^Icon=.*/Icon=reel/' "$APP/reel.desktop" "$APP/usr/share/applications/reel.desktop"
cat > "$APP/AppRun" <<'RUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
exec "$HERE/usr/bin/reel" "$@"
RUN
chmod +x "$APP/AppRun"

mkdir -p "$OUT"
ARCH=x86_64 "$TOOL" "$APP" "$OUT/reel-x86_64.AppImage" >/dev/null
echo "$OUT/reel-x86_64.AppImage"
