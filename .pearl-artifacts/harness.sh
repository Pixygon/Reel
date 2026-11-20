#!/usr/bin/env bash
# Stand-in Reel.app (real reel binary, toy libmpv + sibling dylib, toy
# ffmpeg/ffprobe), run through scripts/mac.sh's signing block verbatim.
# Usage: harness.sh [runtime]   runtime = also force hardened-runtime ad-hoc
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
A="$ROOT/.pearl-artifacts/harness"
rm -rf "$A"; mkdir -p "$A/src"; cd "$A"
echo 'int dep_value(void){return 2;}' > src/dep.c
printf 'int dep_value(void);\nunsigned long mpv_client_api_version(void){return ((unsigned long)dep_value()<<16)|5;}\n' > src/mpv.c
printf '#include <stdio.h>\nint main(void){puts("tool ok");return 0;}\n' > src/tool.c
APP="$A/Reel.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources/bin"
F="$APP/Contents/Frameworks"
clang -dynamiclib -o "$F/libdep.dylib" -install_name @rpath/libdep.dylib src/dep.c
clang -dynamiclib -o "$F/libmpv.dylib" -install_name @rpath/libmpv.dylib src/mpv.c -L"$F" -ldep -Wl,-rpath,/nix/store/fake
ln -s libmpv.dylib "$F/libmpv.2.dylib"
clang -o "$APP/Contents/Resources/bin/ffmpeg" src/tool.c
clang -o "$APP/Contents/Resources/bin/ffprobe" src/tool.c
cp "$ROOT/target/release/reel" "$APP/Contents/MacOS/reel"
install_name_tool -add_rpath @executable_path/../Frameworks "$APP/Contents/MacOS/reel"
for d in "$F"/*.dylib; do
  [ -L "$d" ] && continue
  for r in $(otool -l "$d" | awk '/LC_RPATH/{getline; getline; print $2}'); do
    install_name_tool -delete_rpath "$r" "$d" 2>/dev/null || true
  done
  install_name_tool -add_rpath @loader_path "$d" 2>/dev/null || true
done
cat > "$APP/Contents/Info.plist" <<'P'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.pixygon.reel</string>
<key>CFBundleExecutable</key><string>reel</string>
<key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
P
sed -n '/^# Signing\. install_name_tool/,/^# Notarization/p' "$ROOT/scripts/mac.sh" | sed '$d' > "$A/sign-block.sh"
if [ "${1:-}" = runtime ]; then
  # Simulate the hardened runtime on ad-hoc, to see library validation bite.
  sed -i '' 's/SIGN=(--force --sign -)/SIGN=(--force --sign - --options runtime)/' "$A/sign-block.sh"
fi
cd "$ROOT"
SIGN_IDENTITY="${PIXYGON_MAC_SIGN_IDENTITY:-Developer ID Application: Anders Horne (78FMJHZ97T)}"
# shellcheck disable=SC1090
source "$A/sign-block.sh"
echo "--- signatures"
codesign -dv "$APP" 2>&1 | grep -E "Identifier|flags|TeamIdentifier|Signature"
codesign -d --entitlements - --xml "$APP" 2>/dev/null | plutil -p - || true
codesign -dv "$F/libmpv.dylib" 2>&1 | grep -E "flags|Signature"
codesign -dv "$APP/Contents/Resources/bin/ffmpeg" 2>&1 | grep -E "flags|Signature"
echo "harness ok"
