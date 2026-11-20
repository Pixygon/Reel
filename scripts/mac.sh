#!/usr/bin/env bash
# Build Reel.app for Apple silicon and install it into ~/Applications.
#
# Unlike the Linux tarball/AppImage, the Mac app carries its own media
# engine: a Mac has no package manager to pull mpv and ffmpeg in, so the
# bundle ships
#   Contents/Frameworks/   libmpv + its dylibs (media-kit's LGPL build)
#   Contents/Resources/bin ffmpeg + ffprobe (Martin Riedl's signed static build)
# and Reel finds both on its own (main.rs mac_path, mpv.rs lib_candidates).
# Both downloads are pinned and sha256-checked, cached in ~/.cache/reel-mac.
#
# Usage: scripts/mac.sh [--no-install] [--zip]
#   --no-install  build target/mac/Reel.app only
#   --zip         also write target/mac/Reel-<version>-macos-arm64.zip
#
# Signing: PIXYGON_MAC_SIGN_IDENTITY (default "Developer ID Application:
# Anders Horne (78FMJHZ97T)") with the hardened runtime and
# scripts/mac/reel.entitlements; ad-hoc when that identity isn't in the
# keychain. Set PIXYGON_NOTARY_KEY_ID + PIXYGON_NOTARY_ISSUER to also
# notarize and staple (writes the zip).
set -euo pipefail
cd "$(dirname "$0")/.."

INSTALL=1
ZIP=0
for a in "$@"; do
  case "$a" in
    --no-install) INSTALL=0 ;;
    --zip) ZIP=1 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

[ "$(uname -s)" = Darwin ] || { echo "macOS only" >&2; exit 1; }
[ "$(uname -m)" = arm64 ] || { echo "Apple silicon only (arm64)" >&2; exit 1; }

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
CACHE="$HOME/.cache/reel-mac"
OUT=target/mac
APP="$OUT/Reel.app"

SIGN_IDENTITY="${PIXYGON_MAC_SIGN_IDENTITY:-Developer ID Application: Anders Horne (78FMJHZ97T)}"

MPV_TAG=v0.7.3
MPV_ASSET="libmpv-libs_${MPV_TAG}_macos-arm64-video-full.tar.gz"
FF_BASE=https://ffmpeg.martin-riedl.de/download/macos/arm64/1789931890_9.0.2
FF_SHA_ffmpeg=c8ed4c4e6978a03c485edbfe4e0a5dc2380f8a30bba5150531b31b094492d924
FF_SHA_ffprobe=fcbe839537485eaee7a7a8bc5cbc0f90d53617e80943e8a5b2e31cb851197ea6
MPV_SHA=9bb168ec908b4801f4231f411e3278a0aea644a2b03ce375c880d2813ab7f949

check() { # file sha
  local got
  got="$(shasum -a 256 "$1" | cut -d' ' -f1)"
  [ "$got" = "$2" ] || { echo "sha256 mismatch for $1: $got" >&2; rm -f "$1"; exit 1; }
}

fetch() {
  mkdir -p "$CACHE"
  for t in ffmpeg ffprobe; do
    local z="$CACHE/$t-9.0.2.zip" sha_var="FF_SHA_$t"
    [ -f "$z" ] || curl -fsSL -o "$z" "$FF_BASE/$t.zip"
    check "$z" "${!sha_var}"
  done
  local m="$CACHE/$MPV_ASSET"
  [ -f "$m" ] || curl -fsSL -o "$m" \
    "https://github.com/media-kit/libmpv-darwin-build/releases/download/$MPV_TAG/$MPV_ASSET"
  check "$m" "$MPV_SHA"
}

icon() { # svg -> icns
  local svg="$1" icns="$2" tmp
  tmp="$(mktemp -d)"
  qlmanage -t -s 1024 -o "$tmp" "$svg" >/dev/null 2>&1
  local png="$tmp/$(basename "$svg").png"
  [ -f "$png" ] || { echo "icon render failed" >&2; exit 1; }
  local set="$tmp/reel.iconset"
  mkdir -p "$set"
  for s in 16 32 128 256 512; do
    sips -z $s $s "$png" --out "$set/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) "$png" --out "$set/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$set" -o "$icns"
  rm -rf "$tmp"
}

echo "• cargo build --release"
cargo build --release
fetch

echo "• assembling $APP ($VERSION)"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Frameworks" "$APP/Contents/Resources/bin"
cp target/release/reel "$APP/Contents/MacOS/reel"
install_name_tool -add_rpath @executable_path/../Frameworks "$APP/Contents/MacOS/reel"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
tar -xzf "$CACHE/$MPV_ASSET" -C "$tmp"
cp "$tmp"/libmpv-libs_*/*.dylib "$APP/Contents/Frameworks/"
# Reel asks for libmpv.2.dylib first; media-kit names it libmpv.dylib.
ln -s libmpv.dylib "$APP/Contents/Frameworks/libmpv.2.dylib"
for d in "$APP"/Contents/Frameworks/*.dylib; do
  [ -L "$d" ] && continue
  # Drop the builder's /nix/store rpaths; resolve siblings next to each other.
  for r in $(otool -l "$d" | awk '/LC_RPATH/{getline; getline; print $2}'); do
    install_name_tool -delete_rpath "$r" "$d" 2>/dev/null || true
  done
  install_name_tool -add_rpath @loader_path "$d" 2>/dev/null || true
done

for t in ffmpeg ffprobe; do
  unzip -oq "$CACHE/$t-9.0.2.zip" -d "$APP/Contents/Resources/bin"
done
chmod +x "$APP"/Contents/Resources/bin/*

icon assets/reel-icon.svg "$APP/Contents/Resources/reel.icns"

cat > "$APP/Contents/Resources/THIRD-PARTY.txt" <<NOTICE
Reel is MIT-licensed (https://github.com/Pixygon/Reel). It ships, unmodified:

Contents/Resources/bin/ffmpeg, ffprobe — FFmpeg 9.0.2, static build by
  Martin Riedl (https://ffmpeg.martin-riedl.de), configured --enable-gpl
  --enable-version3: licensed GPL v3. Source: https://ffmpeg.org/releases/
  and the build scripts at https://git.martin-riedl.de/ffmpeg/build-script

Contents/Frameworks/*.dylib — libmpv and its libraries (FFmpeg 6.0, libass,
  freetype, harfbuzz, fribidi, dav1d, uchardet, libxml2, libpng, mbedtls),
  built by media-kit ($MPV_TAG, https://github.com/media-kit/libmpv-darwin-build),
  under their own licenses (LGPL 2.1+ for libmpv and this FFmpeg). Sources
  are listed in that repository.
NOTICE

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Reel</string>
  <key>CFBundleDisplayName</key><string>Reel</string>
  <key>CFBundleIdentifier</key><string>io.pixygon.reel</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>reel</string>
  <key>CFBundleIconFile</key><string>reel</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.video</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>© Pixygon — MIT</string>
  <key>NSMicrophoneUsageDescription</key><string>Reel records voice-over and webcam audio when you ask it to.</string>
  <key>NSCameraUsageDescription</key><string>Reel records from your webcam when you ask it to.</string>
  <key>CFBundleDocumentTypes</key>
  <array>
    <dict>
      <key>CFBundleTypeName</key><string>Media</string>
      <key>CFBundleTypeRole</key><string>Viewer</string>
      <key>LSHandlerRank</key><string>Alternate</string>
      <key>LSItemContentTypes</key>
      <array>
        <string>public.movie</string>
        <string>public.audio</string>
        <string>public.image</string>
      </array>
    </dict>
    <dict>
      <key>CFBundleTypeName</key><string>Reel project</string>
      <key>CFBundleTypeRole</key><string>Editor</string>
      <key>LSHandlerRank</key><string>Owner</string>
      <key>CFBundleTypeExtensions</key><array><string>reel</string></array>
    </dict>
  </array>
</dict>
</plist>
PLIST

# Signing. install_name_tool broke every original signature, and Apple
# silicon refuses to run unsigned code, so everything is (re)signed here,
# INSIDE-OUT: each dylib, then ffmpeg/ffprobe, then the app itself (whose
# seal covers the rest). No --deep: it signs nested code without the
# runtime flag or entitlements and Apple discourages it for signing.
#
# Developer ID when the identity is in the keychain (hardened runtime +
# secure timestamp, as notarization requires), else ad-hoc — Crucible and
# CI have no certificate, and an ad-hoc app still runs on this Mac.
ENT=scripts/mac/reel.entitlements
# Match the identity by its quoted name or its SHA-1 (both are valid --sign
# values). Captured first: `grep -q` closing the pipe early under pipefail
# would look like "identity missing".
IDENTITIES="$(security find-identity -v -p codesigning 2>/dev/null || true)"
if grep -qF -e "\"$SIGN_IDENTITY\"" -e " $SIGN_IDENTITY " <<<"$IDENTITIES"; then
  SIGN=(--force --sign "$SIGN_IDENTITY" --options runtime --timestamp)
  SIGNED_BY="$SIGN_IDENTITY"
else
  echo "• \"$SIGN_IDENTITY\" not in the keychain: ad-hoc signing" >&2
  # No --options runtime here: ad-hoc code has no Team ID, so library
  # validation would refuse every bundled dylib ("different Team IDs").
  SIGN=(--force --sign -)
  SIGNED_BY=ad-hoc
fi
for d in "$APP"/Contents/Frameworks/*.dylib; do
  [ -L "$d" ] && continue
  codesign "${SIGN[@]}" "$d" >/dev/null
done
codesign "${SIGN[@]}" --entitlements "$ENT" "$APP/Contents/Resources/bin/ffmpeg" >/dev/null
codesign "${SIGN[@]}" "$APP/Contents/Resources/bin/ffprobe" >/dev/null
codesign "${SIGN[@]}" --entitlements "$ENT" "$APP" >/dev/null
codesign --verify --strict --deep "$APP"
echo "• signed: $SIGNED_BY"

echo "• smoke: bundled engine"
"$APP/Contents/MacOS/reel" --version
"$APP/Contents/Resources/bin/ffmpeg" -hide_banner -version | head -1
# Library validation: under the hardened runtime, dlopen refuses a dylib not
# signed by the app's own team. Load the bundled libmpv for real.
if [ "$SIGNED_BY" = ad-hoc ]; then
  REEL_MAC_SELFTEST=1 "$APP/Contents/MacOS/reel"
else
  REEL_MAC_SELFTEST=team "$APP/Contents/MacOS/reel"
  spctl --assess --type execute -vv "$APP" 2>&1 | sed "s/^/  /" || true
fi

# Notarization — only when both PIXYGON_NOTARY_KEY_ID and
# PIXYGON_NOTARY_ISSUER are set (an App Store Connect API key). notarytool
# reads AuthKey_<id>.p8 from the standard private_keys folders; the key is
# never copied, echoed or committed. A notarized run always writes the
# stapled zip (the --zip block below), so Gatekeeper passes offline.
if [ -n "${PIXYGON_NOTARY_KEY_ID:-}" ] && [ -n "${PIXYGON_NOTARY_ISSUER:-}" ]; then
  [ "$SIGNED_BY" != ad-hoc ] || { echo "notarization needs a Developer ID signature, not ad-hoc" >&2; exit 1; }
  KEY=""
  for dir in ./private_keys "$HOME/private_keys" "$HOME/.private_keys" "$HOME/.appstoreconnect/private_keys"; do
    [ -f "$dir/AuthKey_$PIXYGON_NOTARY_KEY_ID.p8" ] && { KEY="$dir/AuthKey_$PIXYGON_NOTARY_KEY_ID.p8"; break; }
  done
  [ -n "$KEY" ] || { echo "no AuthKey_<key id>.p8 in a private_keys folder" >&2; exit 1; }
  echo "• notarizing (xcrun notarytool, waits for Apple)"
  ditto -c -k --sequesterRsrc --keepParent "$APP" "$tmp/notarize.zip"
  xcrun notarytool submit "$tmp/notarize.zip" \
    --key "$KEY" --key-id "$PIXYGON_NOTARY_KEY_ID" --issuer "$PIXYGON_NOTARY_ISSUER" --wait
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  ZIP=1
fi

if [ "$ZIP" = 1 ]; then
  Z="$OUT/Reel-$VERSION-macos-arm64.zip"
  rm -f "$Z"
  ditto -c -k --sequesterRsrc --keepParent "$APP" "$Z"
  shasum -a 256 "$Z" | cut -d' ' -f1 > "$Z.sha256"
  echo "• $Z ($(du -h "$Z" | cut -f1))"
fi

if [ "$INSTALL" = 1 ]; then
  mkdir -p "$HOME/Applications"
  rm -rf "$HOME/Applications/Reel.app"
  ditto "$APP" "$HOME/Applications/Reel.app"
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$HOME/Applications/Reel.app" || true
  echo "• installed ~/Applications/Reel.app"
fi
