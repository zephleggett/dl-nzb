#!/usr/bin/env bash
#
# Sign the Direct flavour of dl-nzb.app for distribution outside the App Store.
#
#   scripts/sign.sh apple/build/direct/dl-nzb.app
#   SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" scripts/sign.sh ...
#
# The App Store flavour is not signed here: scripts/archive-appstore.sh
# archives it and Xcode's export signs it for App Store Connect.
#
# Gotchas:
#   - codesign --deep is not used for signing: it would sign nested code with
#     the app's entitlements. Sparkle's pieces are signed first, inside out, in
#     the order sparkle-project.org/documentation/sandboxing gives.
#   - Sparkle's Downloader service is removed before sealing: it is for
#     sandboxed apps without network access, and dl-nzb has network.client.
#     The Installer Launcher service is what a sandboxed app needs.
#   - The entitlements name $(PRODUCT_BUNDLE_IDENTIFIER) for Sparkle's Mach
#     services; Xcode expands it at build time, so this script expands it from
#     the app's Info.plist the same way.
#   - Ad hoc ("-") cannot carry a secure timestamp, so --timestamp is dropped
#     then. --options runtime stays on either way so a local build behaves
#     like a release one.
#   - The hardened runtime's library validation lets the app load Sparkle only
#     when both carry the same Team ID, and an ad hoc signature has none. An
#     ad hoc app takes dl-nzb-Direct-AdHoc.entitlements, which turn it off; a
#     Developer ID one keeps it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"
RESOURCES="$APPLE_DIR/macos/Resources"

APP="${1:-$APPLE_DIR/build/direct/dl-nzb.app}"
SIGN_IDENTITY="${SIGN_IDENTITY:--}"
ENTITLEMENTS="$RESOURCES/dl-nzb-Direct.entitlements"

[ -d "$APP" ] || { echo "error: no app bundle at $APP" >&2; exit 1; }

TIMESTAMP=--timestamp
if [ "$SIGN_IDENTITY" = "-" ]; then
  TIMESTAMP=--timestamp=none
  ENTITLEMENTS="$RESOURCES/dl-nzb-Direct-AdHoc.entitlements"
  echo "==> signing ad hoc; this build is for local use only"
fi

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")"
WORK="$(mktemp -d -t dl-nzb-sign)"
trap 'rm -rf "$WORK"' EXIT
EXPANDED="$WORK/entitlements.plist"
sed "s/\$(PRODUCT_BUNDLE_IDENTIFIER)/$BUNDLE_ID/g" "$ENTITLEMENTS" > "$EXPANDED"

NESTED="$(find "$APP/Contents" \( -path "$APP/Contents/MacOS" -o -path "$APP/Contents/Frameworks" \) -prune -o -type f \( -name '*.dylib' -o -name '*.so' \) -print)"
if [ -n "$NESTED" ]; then
  echo "error: nested code this script does not sign yet:" >&2
  printf '  %s\n' "$NESTED" >&2
  exit 1
fi

SPARKLE="$APP/Contents/Frameworks/Sparkle.framework"
if [ -d "$SPARKLE" ]; then
  echo "==> signing Sparkle"
  rm -rf "$SPARKLE/Versions/B/XPCServices/Downloader.xpc"
  for code in "$SPARKLE"/Versions/B/{XPCServices/Installer.xpc,Autoupdate,Updater.app} "$SPARKLE"; do
    codesign --force --options runtime "$TIMESTAMP" --sign "$SIGN_IDENTITY" "$code"
  done
fi

OTHER="$(find "$APP/Contents/Frameworks" -mindepth 1 -maxdepth 1 ! -name Sparkle.framework 2>/dev/null || true)"
if [ -n "$OTHER" ]; then
  echo "error: frameworks this script does not sign yet:" >&2
  printf '  %s\n' "$OTHER" >&2
  exit 1
fi

echo "==> signing the app"
codesign --force --options runtime "$TIMESTAMP" --entitlements "$EXPANDED" --sign "$SIGN_IDENTITY" "$APP"

echo "==> verifying"
codesign --verify --deep --strict --verbose=2 "$APP"
codesign -d --entitlements - --xml "$APP" | plutil -p - | grep -E 'spk[si]|sandbox' || true

if [ "$SIGN_IDENTITY" != "-" ]; then
  # Before notarization this reports "Unnotarized Developer ID". That is
  # expected; notarize.sh is the next step.
  spctl --assess --type execute --verbose "$APP" || true
fi
