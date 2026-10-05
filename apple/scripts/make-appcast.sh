#!/usr/bin/env bash
#
# Write the Mac app's update feed for the DMG with Sparkle's generate_appcast:
#
#   apple/build/direct/appcast.xml   one signed item: this version's DMG on its GitHub release
#
# The release workflow attaches the feed to the release, and installed copies
# read it from releases/latest/download/appcast.xml. Run it after make-dmg.sh:
# stapling changes the DMG, so it is signed for Sparkle once it is final.
#
#   SPARKLE_ED_PRIVATE_KEY=<base64 seed> apple/scripts/make-appcast.sh apple/build/direct/dl-nzb.app
#   SPARKLE_ED_KEY_FILE=path/to/key apple/scripts/make-appcast.sh apple/build/direct/dl-nzb.app
#
# The notes are this release's CHANGELOG.md section (scripts/changelog.py);
# the update window links to every release for older ones. The download URL is
# GITHUB_REPOSITORY's release (zephleggett/dl-nzb outside CI) for RELEASE_TAG,
# v<the app's version> unless set: a prerelease tag (v0.8.0-rc.1) builds an
# app that says 0.8.0. UPDATE_DOWNLOAD_BASE puts the DMG somewhere else, for a
# test feed, and GENERATE_APPCAST names another copy of the tool.
#
# Gotchas:
#   - generate_appcast only warns when the key is not the other half of the
#     app's SUPublicEDKey, and leaves the DMG unsigned. The signature it wrote
#     is checked against that key here, so the release stops instead of
#     publishing an update every installed copy refuses.
#   - generate_appcast reads a whole folder, so it gets one holding only this
#     DMG and its notes. It is the copy the Sparkle package brought into
#     apple/build/DerivedData, the version the app embeds (project.yml pins
#     it): build-mac.sh has to have run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$APPLE_DIR")"

APP="${1:-$APPLE_DIR/build/direct/dl-nzb.app}"
[ -d "$APP" ] || { echo "error: no app bundle at $APP" >&2; exit 1; }
OUT_DIR="$(cd "$(dirname "$APP")" && pwd)"

VERSION="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP/Contents/Info.plist")"
PUBLIC_KEY="$(/usr/libexec/PlistBuddy -c 'Print :SUPublicEDKey' "$APP/Contents/Info.plist" 2>/dev/null || true)"
TAG="${RELEASE_TAG:-v$VERSION}"
DMG="$OUT_DIR/dl-nzb-$VERSION-macOS.dmg"
OUT="$OUT_DIR/appcast.xml"
REPO="${GITHUB_REPOSITORY:-zephleggett/dl-nzb}"
DOWNLOAD_BASE="${UPDATE_DOWNLOAD_BASE:-https://github.com/$REPO/releases/download/$TAG}"
GENERATE_APPCAST="${GENERATE_APPCAST:-$APPLE_DIR/build/DerivedData/SourcePackages/artifacts/sparkle/Sparkle/bin/generate_appcast}"

[ -n "$PUBLIC_KEY" ] || { echo "error: $APP has no SUPublicEDKey; is it the Direct flavour?" >&2; exit 1; }
[ -f "$DMG" ] || { echo "error: no $DMG; run make-dmg.sh first" >&2; exit 1; }
[ -x "$GENERATE_APPCAST" ] || { echo "error: no generate_appcast at $GENERATE_APPCAST; run build-mac.sh direct first" >&2; exit 1; }
if [ -z "${SPARKLE_ED_PRIVATE_KEY:-}" ] && [ -z "${SPARKLE_ED_KEY_FILE:-}" ]; then
  echo "error: set SPARKLE_ED_PRIVATE_KEY or SPARKLE_ED_KEY_FILE" >&2
  exit 1
fi

STAGE="$(mktemp -d -t dl-nzb-appcast)"
trap 'rm -rf "$STAGE"' EXIT
cp "$DMG" "$STAGE/"
NOTES="$STAGE/$(basename "$DMG" .dmg).md"
python3 "$REPO_ROOT/scripts/changelog.py" notes "$TAG" > "$NOTES"
if [ ! -s "$NOTES" ]; then
  echo "warning: CHANGELOG.md has no section for $TAG; the update window links to the release page instead"
  printf 'See the [release notes](https://github.com/%s/releases/tag/%s).\n' "$REPO" "$TAG" > "$NOTES"
fi

echo "==> writing $OUT"
rm -f "$OUT"
# The key goes in on stdin, never on a command line.
if [ -n "${SPARKLE_ED_PRIVATE_KEY:-}" ]; then
  printf '%s\n' "$SPARKLE_ED_PRIVATE_KEY"
else
  cat "$SPARKLE_ED_KEY_FILE"
fi | "$GENERATE_APPCAST" --ed-key-file - --embed-release-notes \
  --download-url-prefix "$DOWNLOAD_BASE/" \
  --link "https://github.com/$REPO/releases/tag/$TAG" \
  --full-release-notes-url "https://github.com/$REPO/releases" \
  -o "$OUT" "$STAGE"

echo "==> checking the DMG's signature against the app's SUPublicEDKey"
SIGNATURE="$(sed -n 's/.*sparkle:edSignature="\([^"]*\)".*/\1/p' "$OUT")"
if [ -z "$SIGNATURE" ]; then
  echo "error: the feed has no signature for the DMG; is the private key the other half of the app's SUPublicEDKey?" >&2
  exit 1
fi
xcrun swift "$SCRIPT_DIR/check-update-signature.swift" "$PUBLIC_KEY" "$SIGNATURE" "$DMG"
echo "$OUT"
