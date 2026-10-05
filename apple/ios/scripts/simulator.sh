#!/usr/bin/env bash
# Prints the UDID of the simulator the iOS targets use, creating it first if
# it does not exist yet: "dl-nzb iPhone" (iPhone 17 Pro) by default, on the
# newest iOS runtime installed. A device of its own keeps QA runs from
# disturbing the simulators you use for other things.
#
#   scripts/simulator.sh                 dl-nzb iPhone
#   DEVICE=ipad scripts/simulator.sh     dl-nzb iPad (iPad Pro 13-inch (M5))
#   DEVICE=<UDID or name> ...            an existing simulator
set -euo pipefail

device="${DEVICE:-iphone}"
case "$device" in
  iphone) name="dl-nzb iPhone"; type="com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro" ;;
  ipad) name="dl-nzb iPad"; type="com.apple.CoreSimulator.SimDeviceType.iPad-Pro-13-inch-M5-12GB" ;;
  *) name="$device"; type="" ;;
esac

# An exact UDID or name of an available device.
udid="$(xcrun simctl list devices available -j | /usr/bin/python3 -c '
import json, sys
want = sys.argv[1]
for runtime, devices in json.load(sys.stdin)["devices"].items():
    if "iOS" not in runtime:
        continue
    for d in devices:
        if d["udid"] == want or d["name"] == want:
            print(d["udid"]); sys.exit(0)
' "$name")"

if [[ -z "$udid" ]]; then
  if [[ -z "$type" ]]; then
    echo "no simulator called $name" >&2
    exit 1
  fi
  runtime="$(xcrun simctl list runtimes available -j | /usr/bin/python3 -c '
import json, sys
ios = [r for r in json.load(sys.stdin)["runtimes"] if r["platform"] == "iOS" and r["isAvailable"]]
ios.sort(key=lambda r: [int(p) for p in r["version"].split(".")])
print(ios[-1]["identifier"] if ios else "")
')"
  if [[ -z "$runtime" ]]; then
    echo "no iOS simulator runtime is installed (Xcode > Settings > Components)" >&2
    exit 1
  fi
  udid="$(xcrun simctl create "$name" "$type" "$runtime")"
  echo "created $name ($udid)" >&2
fi

echo "$udid"
