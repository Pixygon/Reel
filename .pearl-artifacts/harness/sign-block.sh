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

