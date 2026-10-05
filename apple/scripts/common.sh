# shellcheck shell=bash
# Shared by build-mac.sh and archive-appstore.sh, so a local build and an App
# Store archive agree on version and signing. Sourced, not run:
#
#   source "$SCRIPT_DIR/common.sh"
#   dlnzb_version "$APPLE_DIR"     # sets DLNZB_VERSION and DLNZB_BUILD
#   team="$(dlnzb_team "$APPLE_DIR")"
#
# MARK: Version
#
# DLNZB_VERSION and DLNZB_BUILD from the environment win (a release passes
# them). Otherwise the version is the root Cargo.toml's, the one version the
# CLI and both apps share (a release tag must match it), and the build is the
# commit count.
#
# CFBundleShortVersionString must be one to three dot-separated integers and
# CFBundleVersion integers too, or App Store Connect turns the build down.
# A pre-release version (0.8.0-rc1) is not a legal one, so anything else
# falls back to 0.0.0 (and the build to 1), with a warning.

dlnzb_version() {
  local apple_dir="$1"
  local version build
  version="${DLNZB_VERSION:-$(sed -n 's/^version *= *"\([^"]*\)".*/\1/p' "$apple_dir/../Cargo.toml" | head -1)}"
  version="${version#v}"
  if ! [[ "$version" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
    echo "warning: \"$version\" is not a version an app can carry; using 0.0.0" >&2
    version=0.0.0
  fi
  build="${DLNZB_BUILD:-$(git -C "$apple_dir" rev-list --count HEAD 2>/dev/null || echo 1)}"
  if ! [[ "$build" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
    echo "warning: \"$build\" is not a build number an app can carry; using 1" >&2
    build=1
  fi
  DLNZB_VERSION="$version"
  DLNZB_BUILD="$build"
}

# MARK: Team

# The DEVELOPMENT_TEAM the App Store flavour builds with: empty unless
# apple/macos/Config/Local.xcconfig (git-ignored) sets one.
dlnzb_team() {
  local apple_dir="$1"
  xcodebuild -project "$apple_dir/macos/dl-nzb.xcodeproj" -scheme dl-nzb -configuration Release -showBuildSettings 2>/dev/null \
    | awk -F' = ' '/^ *DEVELOPMENT_TEAM = / { print $2; exit }'
}
