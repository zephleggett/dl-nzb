#!/usr/bin/env bash
#
# Records a Mac demo take: sets the app up, records its window with
# record-mac while drive-mac plays the storyboard's steps, then stops.
# The NZBs go in "~/Downloads/dl-nzb Demo/NZBs", or NZBS=/some/folder ("{nzbs}"
# in the steps), so Finder's path bar and title show nothing of the take
# folder. A folder outside a File Sharing share point (NZBS=/Users/Shared/NZBs)
# keeps Finder's "Folder shared with File Sharing" banner out too.
#
#   scripts/demo/run-mac.sh scripts/demo/mac.json TAKE_DIR path/to/dl-nzb.app
#
# The app must be a Debug build (the steps use -scratchState and -appearance)
# and must not be running: the take starts it. Downloads land in
# "~/Downloads/dl-nzb Demo" (inside Downloads, which the sandbox allows), so
# Show in Finder shows only demo files; delete that folder afterwards.
#
# Leave the Mac alone while it runs: drive-mac stops if anyone else uses the
# keyboard or mouse, or if another app comes to the front. Needs Screen
# Recording and Accessibility permission for the terminal.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
APPLE_DIR="$(cd "$HERE/../.." && pwd)"
STORYBOARD="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
TAKE="$2"
APP="$(cd "$(dirname "$3")" && pwd)/$(basename "$3")"
BIN="$APPLE_DIR/build/demo"
DOWNLOADS="$HOME/Downloads/dl-nzb Demo"

mkdir -p "$BIN" "$TAKE"
TAKE="$(cd "$TAKE" && pwd)"
for tool in record-mac drive-mac; do
  if [[ ! -x "$BIN/$tool" || "$HERE/$tool.swift" -nt "$BIN/$tool" ]]; then
    echo "building $tool"
    swiftc -O -swift-version 5 "$HERE/$tool.swift" -o "$BIN/$tool"
  fi
done

BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")"
# Every flavour shares the bundle identifier and the queue: one copy at a time.
if pgrep -f "dl-nzb.app/Contents/MacOS/dl-nzb" >/dev/null; then
  echo "dl-nzb is running; quit it first (the take starts its own copy)" >&2
  exit 1
fi

# A take starts from nothing: an empty download folder (so no job folder gets
# a " 2" and another scenario) and no named scratch queue from the last take.
if [[ -d "$DOWNLOADS" ]] && [[ -n "$(ls -A "$DOWNLOADS")" ]]; then
  echo "$DOWNLOADS has files from another take; delete it first" >&2
  exit 1
fi
SCRATCH="$(python3 -c '
import json, sys
take = json.load(open(sys.argv[1]))["take"]
args = [a for step in take.get("setup", []) + take.get("steps", []) if step[0] == "launch" for a in step[1]]
print(args[args.index("-scratchStateName") + 1] if "-scratchStateName" in args else "")
' "$STORYBOARD")"
if [[ -n "$SCRATCH" ]]; then
  CONTAINER="$HOME/Library/Containers/$BUNDLE_ID/Data"
  rm -rf "$CONTAINER/tmp/dl-nzb-scratch-$SCRATCH"
  defaults delete "$CONTAINER/Library/Preferences/$BUNDLE_ID.scratch.$SCRATCH" 2>/dev/null || true
fi

NZBS="${NZBS:-$DOWNLOADS/NZBs}"
mkdir -p "$DOWNLOADS"
python3 "$HERE/make-nzbs.py" "$NZBS" >/dev/null
BOOKMARK="$("$BIN/drive-mac" --bookmark "$DOWNLOADS")"
drive() {
  "$BIN/drive-mac" --storyboard "$STORYBOARD" --out "$TAKE" --set "app=$APP" --set "bookmark=$BOOKMARK" --set "downloads=$DOWNLOADS" \
    --set "nzbs=$NZBS" "$@"
}
# Which other windows to record: take.record_windows ({bundle id: [title
# parts]}) records only those windows of those apps ({} for the app alone),
# so the owner's other Finder windows and desktop stay out; without it, all
# of Finder.
INCLUDE=()
while IFS= read -r arg; do [[ -n "$arg" ]] && INCLUDE+=("$arg"); done < <(python3 -c '
import json, sys
take = json.load(open(sys.argv[1]))["take"]
windows = take.get("record_windows")
if windows is None:
    print("--include"); print("com.apple.finder")
for app, parts in (windows or {}).items():
    for part in parts:
        print("--include-window"); print(f"{app}={part}")
' "$STORYBOARD")

echo "setting up"
drive --steps setup

echo "recording"
"$BIN/record-mac" --app "$BUNDLE_ID" ${INCLUDE[@]+"${INCLUDE[@]}"} --out "$TAKE" &
RECORDER=$!
sleep 1.5
status=0
drive --steps steps || status=$?
kill -INT "$RECORDER"
wait "$RECORDER" || true

if [[ $status -ne 0 ]]; then
  echo "the take stopped early (drive-mac exited $status); marks so far are in $TAKE/marks.json" >&2
  exit "$status"
fi
echo "take in $TAKE; downloads in $DOWNLOADS (delete when done)"
if [[ -n "$SCRATCH" ]]; then
  echo "scratch queue \"$SCRATCH\" kept for a retake; clear it with:"
  echo "  rm -rf \"$CONTAINER/tmp/dl-nzb-scratch-$SCRATCH\"; defaults delete \"$CONTAINER/Library/Preferences/$BUNDLE_ID.scratch.$SCRATCH\""
fi
