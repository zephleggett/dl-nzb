#!/usr/bin/env bash
#
# Archive the iPhone and iPad app for the App Store, upload it to App Store
# Connect and wait while Apple processes it, after which TestFlight lists it.
#
#   DLNZB_TEAM=ABCDE12345 ASC_KEY_ID=... ASC_ISSUER_ID=... ASC_KEY_PATH=AuthKey_XXXX.p8 \
#     apple/ios/scripts/testflight.sh
#
# The API key signs as well as uploads: xcodebuild makes the distribution
# certificate and profile itself, which needs a key with the Admin role. The
# notary key's NOTARY_* variables stand in for ASC_* when one key does both.
# UPLOAD=0 stops at an exported .ipa in apple/ios/build/export.
#
# TESTFLIGHT_GROUP=Public also adds the build to that external group and
# submits it for Beta App Review, which is how it reaches the public link.
# TESTFLIGHT_NOTES replaces the What to Test text. A build already uploaded
# (a rerun after a failed review step) is published without a new one.
#
# The version is Cargo.toml's and the build number the commit count, as on
# the Mac (apple/scripts/common.sh); App Store Connect refuses a build number
# it already has for that version, so DLNZB_BUILD overrides it. The Rust
# engine's xcframework is built first when it is missing (make -C apple
# xcframework rebuilds it).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
IOS_DIR="$(dirname "$SCRIPT_DIR")"
APPLE_DIR="$(dirname "$IOS_DIR")"
# shellcheck source-path=SCRIPTDIR/../../scripts source=common.sh
source "$APPLE_DIR/scripts/common.sh"
REPO_ROOT="$(dirname "$APPLE_DIR")"
BUILD="$IOS_DIR/build"
XCFRAMEWORK="$APPLE_DIR/DlNzbKit/Frameworks/DlNzbCore.xcframework"

command -v xcodegen >/dev/null 2>&1 || { echo "error: xcodegen is missing; run: brew install xcodegen" >&2; exit 1; }

ASC_KEY_ID="${ASC_KEY_ID:-${NOTARY_KEY_ID:-}}"
ASC_ISSUER_ID="${ASC_ISSUER_ID:-${NOTARY_ISSUER_ID:-}}"
ASC_KEY_PATH="${ASC_KEY_PATH:-${NOTARY_KEY_PATH:-}}"
for var in DLNZB_TEAM ASC_KEY_ID ASC_ISSUER_ID ASC_KEY_PATH; do
  if [ -z "${!var:-}" ]; then
    echo "error: $var is not set" >&2
    exit 1
  fi
done
[ -f "$ASC_KEY_PATH" ] || { echo "error: no key file at $ASC_KEY_PATH" >&2; exit 1; }
# xcodebuild wants the key's path absolute
ASC_KEY_PATH="$(cd "$(dirname "$ASC_KEY_PATH")" && pwd)/$(basename "$ASC_KEY_PATH")"

# The App Store takes up to three dot-separated integers. dlnzb_version
# turns anything else, a prerelease version (0.8.0-rc.1) included, into
# 0.0.0, which must not go up.
dlnzb_version "$APPLE_DIR"
if [ "$DLNZB_VERSION" = 0.0.0 ]; then
  echo "error: Cargo.toml's version is not a plain X.Y.Z the App Store takes" >&2
  exit 1
fi
VERSION="$DLNZB_VERSION"
BUILD_NUMBER="$DLNZB_BUILD"
BUNDLE_ID="${DLNZB_BUNDLE_ID:-com.zephleggett.dl-nzb}"
export ASC_KEY_ID ASC_ISSUER_ID ASC_KEY_PATH
export DLNZB_BUNDLE_ID="$BUNDLE_ID"

if [ "${UPLOAD:-1}" = 0 ]; then
  DESTINATION="export"
else
  DESTINATION="upload"
fi

ASC=(/usr/bin/python3 "$REPO_ROOT/scripts/asc.py")
PUBLISH=(--notes "${TESTFLIGHT_NOTES:-"dl-nzb $VERSION: https://github.com/zephleggett/dl-nzb/releases/tag/v$VERSION

Add your Usenet server in Settings, then open an NZB from Files or Safari. Send feedback with a screenshot from TestFlight."}")
if [ -n "${TESTFLIGHT_GROUP:-}" ]; then
  PUBLISH+=(--group "$TESTFLIGHT_GROUP")
fi

publish() {
  echo "==> waiting for App Store Connect to process $VERSION ($BUILD_NUMBER)"
  # Apple's verdict on the bundle, ITMS errors included, shows only here
  "${ASC[@]}" wait "$VERSION" "$BUILD_NUMBER"
  echo "==> publishing $VERSION ($BUILD_NUMBER) on TestFlight"
  "${ASC[@]}" publish "$VERSION" "$BUILD_NUMBER" "${PUBLISH[@]}"
}

if [ "$DESTINATION" = upload ] && "${ASC[@]}" uploaded "$VERSION" "$BUILD_NUMBER"; then
  echo "==> $VERSION ($BUILD_NUMBER) is already uploaded; publishing it as it is"
  publish
  exit 0
fi

AUTH=(
  -allowProvisioningUpdates
  -authenticationKeyPath "$ASC_KEY_PATH"
  -authenticationKeyID "$ASC_KEY_ID"
  -authenticationKeyIssuerID "$ASC_ISSUER_ID"
)

if [ ! -d "$XCFRAMEWORK" ]; then
  echo "==> building the Rust engine"
  "$APPLE_DIR/scripts/build-xcframework.sh"
fi

echo "==> generating the project"
# with the placeholder version, as make ios-project does: the real one goes to
# xcodebuild below
DLNZB_VERSION=0.0.0 DLNZB_BUILD=1 xcodegen generate --spec "$IOS_DIR/project.yml" --project "$IOS_DIR" --quiet

echo "==> archiving $BUNDLE_ID $VERSION ($BUILD_NUMBER) for team $DLNZB_TEAM"
ARCHIVE="$BUILD/dl-nzb.xcarchive"
rm -rf "$ARCHIVE" "$BUILD/export"
xcodebuild archive \
  -project "$IOS_DIR/dl-nzb.xcodeproj" \
  -scheme dl-nzb \
  -configuration Release \
  -destination 'generic/platform=iOS' \
  -derivedDataPath "$BUILD/DerivedData" \
  -archivePath "$ARCHIVE" \
  "${AUTH[@]}" \
  "DEVELOPMENT_TEAM=$DLNZB_TEAM" \
  "PRODUCT_BUNDLE_IDENTIFIER=$BUNDLE_ID" \
  "MARKETING_VERSION=$VERSION" \
  "CURRENT_PROJECT_VERSION=$BUILD_NUMBER"

OPTIONS="$BUILD/ExportOptions.plist"
cat >"$OPTIONS" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>method</key>
	<string>app-store-connect</string>
	<key>destination</key>
	<string>$DESTINATION</string>
	<key>teamID</key>
	<string>$DLNZB_TEAM</string>
	<key>signingStyle</key>
	<string>automatic</string>
	<key>uploadSymbols</key>
	<true/>
	<key>manageAppVersionAndBuildNumber</key>
	<false/>
</dict>
</plist>
PLIST

echo "==> exporting ($DESTINATION)"
# The export runs /usr/bin/rsync, which starts its other end as whatever rsync
# is first on PATH; Homebrew's 3.x rejects Apple's -E and the export fails
# with "Copy failed".
PATH="/usr/bin:/bin:/usr/sbin:/sbin:$PATH" xcodebuild -exportArchive \
  -archivePath "$ARCHIVE" \
  -exportOptionsPlist "$OPTIONS" \
  -exportPath "$BUILD/export" \
  "${AUTH[@]}"

if [ "$DESTINATION" = upload ]; then
  publish
  echo "$VERSION ($BUILD_NUMBER) is on TestFlight"
else
  echo "exported: $BUILD/export"
fi
