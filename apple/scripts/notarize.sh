#!/usr/bin/env bash
#
# Notarize and staple a Developer ID signed dl-nzb.app (the Direct flavour).
#
#   NOTARY_KEY_ID=... NOTARY_ISSUER_ID=... NOTARY_KEY_PATH=AuthKey_XXXX.p8 \
#     scripts/notarize.sh apple/build/direct/dl-nzb.app
#
# An ad hoc signed build cannot be notarized; sign with a Developer ID
# Application certificate first (scripts/sign.sh with SIGN_IDENTITY set).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"

APP="${1:-$APPLE_DIR/build/direct/dl-nzb.app}"
[ -d "$APP" ] || { echo "error: no app bundle at $APP" >&2; exit 1; }

for var in NOTARY_KEY_ID NOTARY_ISSUER_ID NOTARY_KEY_PATH; do
  if [ -z "${!var:-}" ]; then
    echo "error: $var is not set; notarizing needs an App Store Connect API key" >&2
    exit 1
  fi
done
[ -f "$NOTARY_KEY_PATH" ] || { echo "error: no key file at $NOTARY_KEY_PATH" >&2; exit 1; }

ZIP="$(dirname "$APP")/dl-nzb-notarize.zip"
echo "==> packing $APP"
rm -f "$ZIP"
# --keepParent so the archive contains dl-nzb.app rather than its contents.
ditto -c -k --keepParent "$APP" "$ZIP"

echo "==> submitting"
SUBMIT_OUTPUT="$(xcrun notarytool submit "$ZIP" \
  --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" \
  --wait --timeout 30m --output-format json)"
echo "$SUBMIT_OUTPUT"

STATUS="$(printf '%s' "$SUBMIT_OUTPUT" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')"
SUBMISSION_ID="$(printf '%s' "$SUBMIT_OUTPUT" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')"
rm -f "$ZIP"

if [ "$STATUS" != "Accepted" ]; then
  echo "error: notarization returned $STATUS" >&2
  if [ -n "$SUBMISSION_ID" ]; then
    xcrun notarytool log "$SUBMISSION_ID" \
      --key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID" >&2 || true
  fi
  exit 1
fi

echo "==> stapling"
xcrun stapler staple "$APP"

echo "==> assessing"
spctl --assess --type execute --verbose "$APP"
echo "notarized and stapled: $APP"
