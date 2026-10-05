#!/usr/bin/env bash
#
# Check that a release tag matches the crate version.
#
#   apple/scripts/check-version.sh v0.8.0    exit 1 unless Cargo.toml says 0.8.0
#
# The release workflow runs this before it builds anything, so a tag that does
# not match fails in a second instead of after a notarization. The version is
# the root Cargo.toml's [package] version: the CLI, the Mac app and the iPhone
# app all ship under it, and ffi/Cargo.toml has to agree. It is read as
# written, prerelease and all, unlike common.sh's dlnzb_version, which turns a
# version an app cannot carry into 0.0.0.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(dirname "$(dirname "$SCRIPT_DIR")")"

# `version = "..."` inside [package], and nowhere else (a dependency table
# has versions too). Plain awk so this runs with nothing installed.
crate_version() {
  awk '
    /^\[/ { in_package = ($0 == "[package]") }
    in_package && /^version[[:space:]]*=/ {
      sub(/^version[[:space:]]*=[[:space:]]*"/, ""); sub(/".*/, ""); print; exit
    }
  ' "$1"
}

TAG="${1:-}"
[ -n "$TAG" ] || { echo "usage: check-version.sh vX.Y.Z" >&2; exit 1; }
VERSION="$(crate_version "$REPO_ROOT/Cargo.toml")"
[ -n "$VERSION" ] || { echo "error: no [package] version in $REPO_ROOT/Cargo.toml" >&2; exit 1; }
TAG_VERSION="${TAG#v}"

FFI_VERSION="$(crate_version "$REPO_ROOT/ffi/Cargo.toml")"
if [ "$TAG_VERSION" != "$VERSION" ] || [ "$FFI_VERSION" != "$VERSION" ]; then
  echo "error: the tag and the crate versions disagree" >&2
  echo "  tag              $TAG (version $TAG_VERSION)" >&2
  echo "  Cargo.toml       $VERSION" >&2
  echo "  ffi/Cargo.toml   $FFI_VERSION" >&2
  exit 1
fi

echo "$TAG matches Cargo.toml $VERSION"
