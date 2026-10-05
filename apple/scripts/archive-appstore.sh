#!/usr/bin/env bash
#
# Archive the App Store flavour and export a signed package for App Store
# Connect, into apple/build/appstore/. Never uploads: submitting is the
# owner's step (Xcode's Organizer or Transporter, with the exported .pkg).
#
#   scripts/archive-appstore.sh
#
# Needs a team: DEVELOPMENT_TEAM in apple/macos/Config/Local.xcconfig (copy
# Local.xcconfig.example), with an Apple Distribution certificate and the
# Mac App Store provisioning Xcode manages for com.zephleggett.dl-nzb.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"
MACOS_DIR="$APPLE_DIR/macos"
OUT="$APPLE_DIR/build/appstore"
ARCHIVE="$OUT/dl-nzb.xcarchive"
EXPORT="$OUT/export"

TEAM="$(dlnzb_team "$APPLE_DIR")"
if [ -z "$TEAM" ]; then
  echo "error: no DEVELOPMENT_TEAM; set it in apple/macos/Config/Local.xcconfig (see Local.xcconfig.example)" >&2
  exit 1
fi

# The same version and build number a local build gets (common.sh).
dlnzb_version "$APPLE_DIR"

# With an App Store Connect API key (CI), xcodebuild signs and makes profiles
# itself; without one, the team signed in to Xcode does.
AUTH=()
if [ -n "${ASC_KEY_ID:-}" ] && [ -n "${ASC_ISSUER_ID:-}" ] && [ -n "${ASC_KEY_PATH:-}" ]; then
  AUTH=(-allowProvisioningUpdates -authenticationKeyPath "$ASC_KEY_PATH"
    -authenticationKeyID "$ASC_KEY_ID" -authenticationKeyIssuerID "$ASC_ISSUER_ID")
fi

echo "==> archiving (team $TEAM, version $DLNZB_VERSION, build $DLNZB_BUILD)"
rm -rf "$ARCHIVE" "$EXPORT"
mkdir -p "$OUT"
xcodebuild archive ${AUTH[@]+"${AUTH[@]}"} \
  -project "$MACOS_DIR/dl-nzb.xcodeproj" \
  -scheme dl-nzb \
  -configuration Release \
  -destination 'generic/platform=macOS' \
  -archivePath "$ARCHIVE" \
  -derivedDataPath "$APPLE_DIR/build/DerivedData" \
  "MARKETING_VERSION=$DLNZB_VERSION" \
  "CURRENT_PROJECT_VERSION=$DLNZB_BUILD"

echo "==> exporting for App Store Connect (not uploading)"
OPTIONS="$OUT/ExportOptions.plist"
sed "s/TEAM_ID/$TEAM/" "$MACOS_DIR/Resources/ExportOptions-AppStore.plist" > "$OPTIONS"
# Homebrew's rsync, if first on PATH, makes the export fail with "Copy failed".
PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH" xcodebuild -exportArchive -archivePath "$ARCHIVE" -exportPath "$EXPORT" \
  -exportOptionsPlist "$OPTIONS" -allowProvisioningUpdates ${AUTH[@]+"${AUTH[@]}"}

echo "exported: $EXPORT"
/bin/ls -1 "$EXPORT"
