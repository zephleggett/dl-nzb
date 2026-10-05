#!/usr/bin/env bash
#
# Build the Rust engine for the Apple apps:
#
#   apple/DlNzbKit/Frameworks/DlNzbCore.xcframework   static libraries for
#       macOS (one universal library: aarch64- and x86_64-apple-darwin),
#       aarch64-apple-ios and aarch64-apple-ios-sim, with the C header and
#       module map of the DlNzbCore module (git-ignored)
#   apple/DlNzbKit/Sources/DlNzbFFI/DlNzbFFI.swift   UniFFI's Swift bindings
#       (committed so the package reads without a Rust build; regenerated here)
#   apple/DlNzbKit/Sources/DlNzbUI/Resources/rust-crates.json   every crate
#       linked into the apps, with its licence, for the Acknowledgements screen
#
# Usage: apple/scripts/build-xcframework.sh        (or: make -C apple xcframework)
#
# Environment:
#   RUSTUP_TOOLCHAIN   default 1.92.0, with the four Apple std libraries
#   CARGO_PROFILE      default release-ffi (release with panic = "unwind")
#   PLATFORMS          default "macos ios"; "macos" or "ios" builds only those
#                      slices (a Mac release needs no iOS ones, TestFlight no Mac)
#
# Safe to run again: everything it writes is replaced.
#
# Gotchas:
# - Homebrew's rustc comes first on many PATHs and has no iOS std library, so
#   ~/.cargo/bin goes in front and the toolchain is pinned with RUSTUP_TOOLCHAIN.
# - The bindings come from the workspace's own `uniffi-bindgen` binary
#   (ffi/uniffi-bindgen.rs), built against exactly the UniFFI the library uses.
#   A generator of another version writes bindings whose checksums fail at the
#   first call. Library mode reads the metadata out of the static library.
# - UniFFI's generated module map says `use "_Builtin_stdbool"` and
#   `use "_Builtin_stdint"`, which Xcode 26's clang rejects
#   (mozilla/uniffi-rs#2917). Both lines are removed.
# - An xcframework takes one library per platform variant, so the two macOS
#   architectures are joined with lipo into one universal library.
# - The header and module.modulemap go in Headers/DlNzbCore/, not Headers/: an
#   app linking two xcframeworks with a top-level module.modulemap each fails
#   with "multiple commands produce include/module.modulemap". Clang finds a
#   module map in a subfolder named after the module.
# - unrar_sys asks for libc++ only when building for macOS, and std needs
#   libiconv; the DlNzbFFI target in Package.swift links both, plus Security
#   and CoreFoundation, for every platform. The libraries each build actually
#   wants are printed below (rustc's native-static-libs) when it relinks.
# - The deployment targets must be in the environment of the cargo build, or
#   the C and C++ parts (unrar) are built for the SDK's default and every app
#   link warns about objects built for a newer OS.
# - `xcodebuild -create-xcframework` will not overwrite, so the old one is
#   removed first.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLE_DIR="$(dirname "$SCRIPT_DIR")"
REPO_ROOT="$(dirname "$APPLE_DIR")"
KIT_DIR="$APPLE_DIR/DlNzbKit"

export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-1.92.0}"
export MACOSX_DEPLOYMENT_TARGET=26.0
export IPHONEOS_DEPLOYMENT_TARGET=26.0
PROFILE="${CARGO_PROFILE:-release-ffi}"

CRATE=dl-nzb-ffi
LIB=libdl_nzb_ffi.a
MODULE=DlNzbCore            # the C module (ffi/uniffi.toml: ffi_module_name)
HEADER=DlNzbCoreFFI.h       # ffi/uniffi.toml: ffi_module_filename
SWIFT_MODULE=DlNzbFFI       # ffi/uniffi.toml: module_name
PLATFORMS="${PLATFORMS:-macos ios}"
TARGETS=()
case " $PLATFORMS " in *" macos "*) TARGETS+=(aarch64-apple-darwin x86_64-apple-darwin) ;; esac
case " $PLATFORMS " in *" ios "*) TARGETS+=(aarch64-apple-ios aarch64-apple-ios-sim) ;; esac
[ ${#TARGETS[@]} -gt 0 ] || { echo "error: PLATFORMS must name macos and/or ios" >&2; exit 1; }

XCFRAMEWORK="$KIT_DIR/Frameworks/$MODULE.xcframework"
SWIFT_OUT="$KIT_DIR/Sources/$SWIFT_MODULE"
CRATES_JSON="$KIT_DIR/Sources/DlNzbUI/Resources/rust-crates.json"
TARGET_DIR="$REPO_ROOT/target"
WORK="$TARGET_DIR/xcframework-work"

die() { echo "error: $*" >&2; exit 1; }

for tool in cargo rustup xcodebuild xcrun lipo python3; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is missing"
done
installed="$(rustup target list --installed)"
for target in "${TARGETS[@]}"; do
  grep -qx "$target" <<<"$installed" || die "rust target $target is missing; run: rustup +$RUSTUP_TOOLCHAIN target add $target"
done

cd "$REPO_ROOT"
echo "==> rustc $(rustc --version | cut -d' ' -f2) ($RUSTUP_TOOLCHAIN), profile $PROFILE"

# 1. The static libraries.
for target in "${TARGETS[@]}"; do
  echo "==> building $CRATE for $target"
  log="$(mktemp)"
  if ! cargo rustc -p "$CRATE" --lib --profile "$PROFILE" --target "$target" -- --print native-static-libs 2>"$log"; then
    cat "$log" >&2
    rm -f "$log"
    die "cargo failed for $target"
  fi
  grep -E '^(warning|error)' "$log" >&2 || true
  sed -n 's/^note: native-static-libs: /    links: /p' "$log"
  rm -f "$log"
  [ -f "$TARGET_DIR/$target/$PROFILE/$LIB" ] || die "no $LIB for $target"
done

# 2. The Swift bindings, the C header and the module map.
rm -rf "$WORK"
mkdir -p "$WORK/generated" "$WORK/headers/$MODULE"
echo "==> generating the Swift bindings"
cargo run -q -p "$CRATE" --features bindgen --bin uniffi-bindgen -- \
  generate --library "$TARGET_DIR/${TARGETS[0]}/$PROFILE/$LIB" \
  --language swift --out-dir "$WORK/generated"
for file in "$SWIFT_MODULE.swift" "$HEADER" "${HEADER%.h}.modulemap"; do
  [ -f "$WORK/generated/$file" ] || die "uniffi-bindgen wrote no $file"
done
cp "$WORK/generated/$HEADER" "$WORK/headers/$MODULE/"
grep -v -e '_Builtin_stdbool' -e '_Builtin_stdint' "$WORK/generated/${HEADER%.h}.modulemap" \
  >"$WORK/headers/$MODULE/module.modulemap"
grep -q "module $MODULE" "$WORK/headers/$MODULE/module.modulemap" || die "the module map does not declare $MODULE"

# 3. The xcframework.
echo "==> creating $MODULE.xcframework"
rm -rf "$XCFRAMEWORK"
mkdir -p "$(dirname "$XCFRAMEWORK")"
args=()
if [[ " $PLATFORMS " == *" macos "* ]]; then
  mkdir -p "$WORK/macos"
  lipo -create "$TARGET_DIR/aarch64-apple-darwin/$PROFILE/$LIB" "$TARGET_DIR/x86_64-apple-darwin/$PROFILE/$LIB" \
    -output "$WORK/macos/$LIB"
  args+=(-library "$WORK/macos/$LIB" -headers "$WORK/headers")
fi
if [[ " $PLATFORMS " == *" ios "* ]]; then
  for target in aarch64-apple-ios aarch64-apple-ios-sim; do
    args+=(-library "$TARGET_DIR/$target/$PROFILE/$LIB" -headers "$WORK/headers")
  done
fi
xcodebuild -create-xcframework "${args[@]}" -output "$XCFRAMEWORK" >/dev/null

# 4. The bindings into the package (with a note on where they come from).
mkdir -p "$SWIFT_OUT"
{
  echo "// Generated by apple/scripts/build-xcframework.sh with UniFFI from ffi/ (crate $CRATE)."
  echo "// Do not edit: run the script (make -C apple xcframework) after changing the Rust API."
  echo "// swift-format-ignore-file"
  echo
  cat "$WORK/generated/$SWIFT_MODULE.swift"
} >"$SWIFT_OUT/$SWIFT_MODULE.swift"

# 5. The licences of every crate linked into the apps (normal dependencies,
# no proc-macros, any of the three targets), as DlNzbUI's Acknowledgement.
echo "==> listing the Rust crates and their licences"
trees="$WORK/crates.txt"
# One cargo tree over every target (--target=<triple> each).
cargo tree -q -p "$CRATE" "${TARGETS[@]/#/--target=}" -e normal,no-proc-macro --prefix none -f '{p}' >"$trees"
cargo metadata -q --format-version 1 >"$WORK/metadata.json"
python3 - "$trees" "$WORK/metadata.json" "$CRATES_JSON" <<'PY'
import json, os, re, sys

trees, metadata_path, out_path = sys.argv[1:4]
# The app itself is credited on its own in the Acknowledgements screen.
SKIP = {"dl-nzb", "dl-nzb-ffi"}

wanted = set()
for line in open(trees):
    parts = line.split()
    if len(parts) >= 2 and parts[1].startswith("v"):
        wanted.add((parts[0], parts[1][1:]))

packages = {(p["name"], p["version"]): p for p in json.load(open(metadata_path))["packages"]}
LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|unlicense)", re.IGNORECASE)

def licence_text(package):
    root = os.path.dirname(package["manifest_path"])
    names = []
    if package.get("license_file"):
        names.append(package["license_file"])
    names += sorted(n for n in os.listdir(root) if LICENCE_FILE.match(n) and os.path.isfile(os.path.join(root, n)))
    texts, seen = [], set()
    for name in names:
        path = os.path.normpath(os.path.join(root, name))
        if path in seen or not os.path.isfile(path):
            continue
        seen.add(path)
        with open(path, encoding="utf-8", errors="replace") as f:
            texts.append(f.read().strip())
    return "\n\n".join(texts)

entries = []
for key in sorted(wanted):
    if key[0] in SKIP:
        continue
    package = packages.get(key)
    if package is None:
        sys.exit(f"error: {key[0]} {key[1]} is in cargo tree but not in cargo metadata")
    licence = package.get("license") or ("See the licence text" if package.get("license_file") else "Unknown")
    url = package.get("repository") or package.get("homepage")
    text = licence_text(package) or f"{package['name']} is licensed under {licence}." + (f" See {url}." if url else "")
    entry = {"name": package["name"], "version": package["version"], "licence": licence, "text": text}
    if url:
        entry["url"] = url
    entries.append(entry)

missing = [e["name"] for e in entries if e["licence"] == "Unknown"]
if missing:
    sys.exit("error: no licence declared by " + ", ".join(missing))
with open(out_path, "w") as f:
    json.dump(entries, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"    {len(entries)} crates, {sum(1 for e in entries if 'is licensed under' in e['text'])} without a licence file")
PY

rm -rf "$WORK"

echo "==> done"
for target in "${TARGETS[@]}"; do
  printf '    %-24s %s\n' "$target" "$(du -h "$TARGET_DIR/$target/$PROFILE/$LIB" | cut -f1)"
done
printf '    %-24s %s\n' "$MODULE.xcframework" "$(du -sh "$XCFRAMEWORK" | cut -f1)"
printf '    %-24s %s\n' "$SWIFT_MODULE.swift" "$(du -h "$SWIFT_OUT/$SWIFT_MODULE.swift" | cut -f1)"
printf '    %-24s %s\n' "rust-crates.json" "$(du -h "$CRATES_JSON" | cut -f1)"
