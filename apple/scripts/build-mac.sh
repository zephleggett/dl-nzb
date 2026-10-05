#!/usr/bin/env bash
#
# Build dl-nzb for Mac into apple/build/<flavour>/dl-nzb.app.
#
#   scripts/build-mac.sh appstore            Release, App Store flavour (no Sparkle)
#   scripts/build-mac.sh direct              Release, Direct flavour (Sparkle)
#   CONFIG=debug OUT=apple/build scripts/build-mac.sh appstore
#
# OUT is the folder the app is copied into (apple/build/<flavour> by default).
#
# The build is ad hoc, unless macos/Config/Local.xcconfig (git-ignored) names
# a team: then it is signed with that team's Apple Development certificate.
# Either way scripts/sign.sh then signs the Direct flavour inside out with
# SIGN_IDENTITY, and the App Store flavour is archived and exported for
# release instead (scripts/archive-appstore.sh).
#
# A team signature is what local testing wants: the Keychain ties the server
# password to the app's signature, an ad hoc one changes with every build, and
# each new build would then ask for the login keychain's password before it
# could read the one the last build saved.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$SCRIPT_DIR/common.sh"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"
MACOS_DIR="$APPLE_DIR/macos"
FLAVOUR="${1:-appstore}"
CONFIG="${CONFIG:-release}"

case "$FLAVOUR:$CONFIG" in
  appstore:release) SCHEME="dl-nzb"; CONFIGURATION="Release" ;;
  appstore:debug) SCHEME="dl-nzb"; CONFIGURATION="Debug" ;;
  direct:release) SCHEME="dl-nzb Direct"; CONFIGURATION="Release-Direct" ;;
  direct:debug) SCHEME="dl-nzb Direct"; CONFIGURATION="Debug-Direct" ;;
  *) echo "usage: [CONFIG=release|debug] $0 appstore|direct" >&2; exit 2 ;;
esac

# The version the app shows (common.sh): Cargo.toml's, or 0.0.0.
dlnzb_version "$APPLE_DIR"
TEAM="$(dlnzb_team "$APPLE_DIR")"

DERIVED="${DERIVED:-$APPLE_DIR/build/DerivedData}"
OUT="${OUT:-$APPLE_DIR/build/$FLAVOUR}"

echo "==> building $SCHEME ($CONFIGURATION, version $DLNZB_VERSION, build $DLNZB_BUILD, ${TEAM:+team $TEAM}${TEAM:-ad hoc})"
XCODEBUILD_ARGS=(
  -project "$MACOS_DIR/dl-nzb.xcodeproj"
  -scheme "$SCHEME"
  -configuration "$CONFIGURATION"
  -destination 'generic/platform=macOS'
  -derivedDataPath "$DERIVED"
  build
  "MARKETING_VERSION=$DLNZB_VERSION"
  "CURRENT_PROJECT_VERSION=$DLNZB_BUILD"
  CODE_SIGNING_ALLOWED=YES
)
# Without a team, ad hoc. An identity is only ever "-" on this command line:
# a Developer ID here would reach the Swift package targets too, which sign
# automatically, and xcodebuild refuses the pair. A team's identity comes from
# Local.xcconfig, which only the app targets read.
if [ -z "$TEAM" ]; then
  XCODEBUILD_ARGS+=("CODE_SIGN_IDENTITY=-")
fi
# Sparkle's feed is empty in the project, which turns updates off. A release
# passes it (the key is in the project, and can be overridden the same way).
for setting in DLNZB_UPDATE_FEED_URL DLNZB_UPDATE_PUBLIC_KEY; do
  if [ -n "${!setting:-}" ]; then
    XCODEBUILD_ARGS+=("$setting=${!setting}")
  fi
done
xcodebuild "${XCODEBUILD_ARGS[@]}"

PRODUCT="$DERIVED/Build/Products/$CONFIGURATION/dl-nzb.app"
[ -d "$PRODUCT" ] || { echo "error: no product at $PRODUCT" >&2; exit 1; }

mkdir -p "$OUT"
rm -rf "$OUT/dl-nzb.app"
ditto "$PRODUCT" "$OUT/dl-nzb.app"
# ditto keeps the product's dates, and Xcode never updates the bundle
# folder's own, so the Dock would go on showing an older icon.
touch "$OUT/dl-nzb.app"
echo "$OUT/dl-nzb.app"
