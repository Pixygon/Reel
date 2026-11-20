# Notarization — only when PIXYGON_NOTARY_PROFILE names a notarytool
# keychain profile (`xcrun notarytool store-credentials`; on the M5 that is
# normally elixir-notary, team 78FMJHZ97T). The credentials live in that
# keychain item only: nothing is read, echoed or committed here. Submit the
# zip, wait for Apple, staple the ticket to the .app, then the --zip block
# below re-zips the STAPLED app into target/mac so Gatekeeper passes offline.
if [ -n "${PIXYGON_NOTARY_PROFILE:-}" ]; then
  [ "$SIGNED_BY" != ad-hoc ] || { echo "notarization needs a Developer ID signature, not ad-hoc" >&2; exit 1; }
  echo "• notarizing with keychain profile \"$PIXYGON_NOTARY_PROFILE\" (waits for Apple)"
  ditto -c -k --sequesterRsrc --keepParent "$APP" "$tmp/notarize.zip"
  xcrun notarytool submit "$tmp/notarize.zip" --keychain-profile "$PIXYGON_NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  spctl --assess --type execute -vv "$APP" 2>&1 | sed "s/^/  /" || true
  ZIP=1
fi
