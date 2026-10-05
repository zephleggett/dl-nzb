# shellcheck shell=bash
# Shared by the build, release and TestFlight scripts, so a local build, an
# App Store archive and a release agree on version, signing and notarization.
# Sourced, not run:
#
#   source "$SCRIPT_DIR/common.sh"
#   crate_version Cargo.toml       # prints its [package] version, as written
#   dlnzb_version "$APPLE_DIR"     # sets DLNZB_VERSION and DLNZB_BUILD
#   team="$(dlnzb_team "$APPLE_DIR")"
#   notarize_and_staple FILE [TARGET]
#
# MARK: Version

# The `version = "..."` inside a Cargo.toml's [package], and nowhere else (a
# dependency table has versions too), prerelease and all. Plain awk, so
# check-version.sh runs on a runner with nothing installed.
crate_version() {
  awk '
    /^\[/ { in_package = ($0 == "[package]") }
    in_package && /^version[[:space:]]*=/ {
      sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*/, ""); print; exit
    }
  ' "$1"
}

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
  version="${DLNZB_VERSION:-$(crate_version "$apple_dir/../Cargo.toml")}"
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

# MARK: Notarization

# Submit FILE to Apple's notary service, wait for the verdict, and staple the
# ticket to TARGET, FILE itself by default (an app goes up zipped, but the
# ticket goes on the app). Anything but Accepted prints the notary's log and
# fails. The key is NOTARY_KEY_ID, NOTARY_ISSUER_ID and NOTARY_KEY_PATH,
# which the caller checks are set.
notarize_and_staple() {
  local file="$1" target="${2:-$1}"
  local key=(--key "$NOTARY_KEY_PATH" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER_ID")
  local output status id
  output="$(xcrun notarytool submit "$file" "${key[@]}" --wait --timeout 30m --output-format json)" || return
  echo "$output"
  status="$(printf '%s' "$output" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("status",""))')"
  id="$(printf '%s' "$output" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin).get("id",""))')"
  if [ "$status" != "Accepted" ]; then
    echo "error: notarization returned $status" >&2
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" "${key[@]}" >&2 || true
    fi
    return 1
  fi
  echo "==> stapling"
  xcrun stapler staple "$target"
}
