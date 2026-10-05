#!/usr/bin/env bash
#
# Build the Direct flavour's release artifacts from a signed (and ideally
# stapled) app:
#
#   apple/build/direct/dl-nzb-<version>-macOS.dmg   compressed, with an /Applications link
#   apple/build/direct/SHA256SUMS                   its checksum
#
# An ad hoc build (SIGN_IDENTITY "-") also gets dl-nzb-<version>-macOS.zip: a
# DMG of it would only invite people to install something Gatekeeper refuses.
#
# No third-party tooling: hdiutil and ditto only.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"

APP="${1:-$APPLE_DIR/build/direct/dl-nzb.app}"
[ -d "$APP" ] || { echo "error: no app bundle at $APP" >&2; exit 1; }

# The app's own version: the DMG is named for what it holds.
VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist" 2>/dev/null || echo 0.0.0)"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
OUT_DIR="$(dirname "$APP")"
DMG="$OUT_DIR/dl-nzb-$VERSION-macOS.dmg"
ZIP="$OUT_DIR/dl-nzb-$VERSION-macOS.zip"
STAGE="$OUT_DIR/dmg-stage"

echo "==> staging"
rm -rf "$STAGE"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/dl-nzb.app"
ln -s /Applications "$STAGE/Applications"

echo "==> building $DMG"
rm -f "$DMG"
# hdiutil sometimes fails with "resource busy" while an earlier mount is still
# detaching. One retry clears it.
if ! hdiutil create -volname dl-nzb -srcfolder "$STAGE" -ov -format UDZO "$DMG"; then
  echo "warning: hdiutil failed, retrying once"
  sleep 5
  hdiutil create -volname dl-nzb -srcfolder "$STAGE" -ov -format UDZO "$DMG"
fi

echo "==> signing the image"
if [ "$SIGN_IDENTITY" = "-" ]; then
  codesign --force --sign - "$DMG"
else
  codesign --force --timestamp --sign "$SIGN_IDENTITY" "$DMG"
fi

if [ "$SIGN_IDENTITY" != "-" ] && [ -n "${NOTARY_KEY_ID:-}" ] && [ -n "${NOTARY_ISSUER_ID:-}" ] && [ -n "${NOTARY_KEY_PATH:-}" ]; then
  echo "==> notarizing the image"
  xcrun notarytool submit "$DMG" \
    --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" \
    --wait --timeout 30m
  xcrun stapler staple "$DMG"
else
  echo "==> skipping notarization of the image (no credentials, or an ad hoc signature)"
fi

rm -f "$ZIP"
SUMS=("$(basename "$DMG")")
if [ "$SIGN_IDENTITY" = "-" ]; then
  echo "==> building $ZIP (ad hoc build)"
  ditto -c -k --keepParent "$APP" "$ZIP"
  SUMS+=("$(basename "$ZIP")")
fi

echo "==> checksums"
(cd "$OUT_DIR" && shasum -a 256 "${SUMS[@]}" > SHA256SUMS)
cat "$OUT_DIR/SHA256SUMS"

rm -rf "$STAGE"
echo "$DMG"
[ -f "$ZIP" ] && echo "$ZIP"
exit 0
