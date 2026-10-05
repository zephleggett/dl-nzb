#!/usr/bin/env python3
"""Records an iPhone demo take in the simulator, driven by the storyboard's
`take` section. No taps: everything goes through simctl, the way a user's
actions arrive in the app anyway (an NZB opened from Files is a file URL).

    run-iphone.py STORYBOARD.json TAKE_DIR --app path/to/dl-nzb.app
    run-iphone.py STORYBOARD.json TAKE_DIR --app ... --delete-device   remove the simulator afterwards

The take runs on a simulator of its own ("dl-nzb Demo iPhone" by default),
created on first use, set to US English with a 12-hour clock (so the status
bar reads 9:41), dark or light as the storyboard says, with the status bar
overridden. The app is reinstalled for every take, launched with the
simulated engine (`simulate` is written to its defaults, so a launch by iOS
itself also simulates), and the NZBs from make-nzbs.py are put in Files under
On My iPhone.

Writes TAKE_DIR/take.mov (HEVC, the screen with its corners and Dynamic
Island masked black), take.json (when the recording started) and marks.json
(when each `mark` step ran), for compose.py.

Steps, as JSON arrays:
  ["launch", [args...]]        start the app (terminating a running copy) with these launch arguments
  ["app"]                      bring the app to the front, as tapping its icon would
  ["quit-app"]                 end the app; an `open` then cold-launches it, as from Files
  ["files", "Downloads"]       switch to Files and show a folder of On My iPhone
  ["app-files", "Downloads/X"] show a folder of the app's own (On My iPhone > dl-nzb) in Files,
                               as the app's Show in Files does
  ["open", "Name"]             open Name.nzb from that On My iPhone folder, as a tap in Files does
  ["wait", 1.5]                seconds
  ["wait-file", "Downloads/X/X.mkv", 90]   until the app's Documents has this file
  ["size-files", "Downloads/X"]            give the simulated engine's empty files their real sizes
  ["mark", "name"]             note the time, for the storyboard's cut
"""

import argparse
import datetime
import json
import os
import shutil
import signal
import subprocess
import sys
import time
import urllib.parse
import xml.etree.ElementTree as ET

HERE = os.path.dirname(os.path.abspath(__file__))
BUNDLE_ID = "com.zephleggett.dl-nzb"
DEVICE_TYPE = "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro"


def run(*args, check=True, capture=True):
    result = subprocess.run(list(args), capture_output=capture, text=True)
    if check and result.returncode != 0:
        raise SystemExit(f"{' '.join(args)} failed: {result.stderr.strip()}")
    return result.stdout.strip() if capture else ""


def simctl(*args, check=True):
    return run("xcrun", "simctl", *args, check=check)


def find_or_create_device(name):
    devices = json.loads(simctl("list", "devices", "available", "-j"))["devices"]
    for runtime, entries in devices.items():
        if "iOS" in runtime:
            for device in entries:
                if device["name"] == name:
                    return device["udid"], False
    runtimes = [r for r in json.loads(simctl("list", "runtimes", "available", "-j"))["runtimes"] if r["platform"] == "iOS"]
    if not runtimes:
        raise SystemExit("no iOS simulator runtime is installed")
    runtimes.sort(key=lambda r: [int(p) for p in r["version"].split(".")])
    udid = simctl("create", name, DEVICE_TYPE, runtimes[-1]["identifier"])
    print(f"created {name} ({udid}) on iOS {runtimes[-1]['version']}")
    return udid, True


def boot(udid):
    simctl("boot", udid, check=False)
    simctl("bootstatus", udid, "-b")


def prepare_device(udid, appearance):
    """US English and a 12-hour clock (the simulator copies the Mac's 24-hour
    setting, which turns 9:41 into 09:41). Takes a reboot the first time."""
    boot(udid)
    wanted = {"AppleLocale": ("-string", "en_US"), "AppleICUForce24HourTime": ("-bool", "false")}
    changed = False
    for key, (kind, value) in wanted.items():
        current = simctl("spawn", udid, "defaults", "read", "-g", key, check=False)
        normal = {"false": "0", "true": "1"}.get(value, value)
        if current != normal:
            simctl("spawn", udid, "defaults", "write", "-g", key, kind, value)
            changed = True
    if simctl("spawn", udid, "defaults", "read", "-g", "AppleLanguages", check=False).replace(" ", "").replace("\n", "") != '("en-US")':
        simctl("spawn", udid, "defaults", "write", "-g", "AppleLanguages", "-array", "en-US")
        changed = True
    if changed:
        print("rebooting the simulator for the locale and clock")
        simctl("shutdown", udid)
        boot(udid)
        time.sleep(8)  # SpringBoard settles after bootstatus returns
    simctl("ui", udid, "appearance", appearance)
    simctl("status_bar", udid, "override", "--time", "9:41", "--dataNetwork", "wifi", "--wifiMode", "active", "--wifiBars", "3",
           "--cellularMode", "active", "--cellularBars", "4", "--operatorName", "", "--batteryState", "discharging", "--batteryLevel", "100")


class Device:
    def __init__(self, udid, nzb_dir):
        self.udid = udid
        self.nzb_dir = nzb_dir
        storage = None
        for line in simctl("get_app_container", udid, "com.apple.DocumentsApp", "groups").splitlines():
            group, _, path = line.partition("\t")
            if group.strip() == "group.com.apple.FileProvider.LocalStorage":
                storage = path.strip()
        if not storage:
            raise SystemExit("no On My iPhone storage: open the Files app on the simulator once")
        self.on_my_iphone = os.path.join(storage, "File Provider Storage")
        self.app_args = []

    def app_documents(self):
        return os.path.join(simctl("get_app_container", self.udid, BUNDLE_ID, "data"), "Documents")

    def open_url(self, url):
        simctl("openurl", self.udid, url)

    def show_folder(self, path):
        self.open_url("shareddocuments://" + urllib.parse.quote(path))


def write_cli_config(path, extra):
    """A dl-nzb CLI config naming a pretend server, for `-importCLIConfig`.
    The import also takes the post-processing settings, so the storyboard's
    `cli_config` sets those here (CLI key names)."""
    sections = {"usenet": {"server": "news.example.com", "port": 563, "ssl": True, "username": "demo", "password": "demo", "connections": 30}}
    for section, values in extra.items():
        sections.setdefault(section, {}).update(values)
    lines = ["# A pretend server for -simulate YES; nothing connects to it."]
    for section, values in sections.items():
        lines.append(f"[{section}]")
        for key, value in values.items():
            text = ("true" if value else "false") if isinstance(value, bool) else str(value) if isinstance(value, int) else f'"{value}"'
            lines.append(f"{key} = {text}")
    with open(path, "w") as handle:
        handle.write("\n".join(lines) + "\n")


def put_nzbs(device, folder, names, nzb_dir):
    target = os.path.join(device.on_my_iphone, folder)
    shutil.rmtree(target, ignore_errors=True)
    os.makedirs(target)
    # Saved a few minutes before 9:41 today, so Files' times agree with the clock.
    stamp = datetime.datetime.now().replace(hour=9, minute=33, second=0, microsecond=0).timestamp()
    for index, name in enumerate(names):
        path = os.path.join(target, f"{name}.nzb")
        shutil.copy(os.path.join(nzb_dir, f"{name}.nzb"), path)
        os.utime(path, (stamp + 60 * index, stamp + 60 * index))
    return target


def nzb_sizes(nzb_path):
    """The files a finished simulated job leaves, with the sizes the engine
    reports for them: decoded bytes are 97% of the NZB's article bytes."""
    tree = ET.parse(nzb_path)
    ns = {"n": "http://www.newzbin.com/DTD/2003/nzb"}
    files = {}
    title = None
    for meta in tree.getroot().iterfind("n:head/n:meta", ns):
        if meta.get("type") == "title":
            title = meta.text
    for entry in tree.getroot().iterfind("n:file", ns):
        subject = entry.get("subject", "")
        name = subject.split('"')[1] if subject.count('"') >= 2 else subject
        files[name] = sum(int(s.get("bytes", 0)) for s in entry.iterfind("n:segments/n:segment", ns))
    data = sum(size for name, size in files.items() if not name.lower().endswith(".par2"))
    sizes = {name: int(size * 0.97) for name, size in files.items()}
    title = title or os.path.splitext(os.path.basename(nzb_path))[0]
    for ext in ("mkv", "iso", "flac", "epub", "dmg"):
        sizes.setdefault(f"{title}.{ext}", int(data * 0.97))
    return sizes


def size_files(folder, nzb_dir):
    """Sparse files of the right length in place of the simulated engine's
    empty ones, so Files shows real sizes. They take no space on disk."""
    name = os.path.basename(folder.rstrip("/"))
    sizes = nzb_sizes(os.path.join(nzb_dir, f"{name}.nzb"))
    # Dated a minute before the status bar's 9:41, like the rest of the take.
    stamp = datetime.datetime.now().replace(hour=9, minute=40, second=0, microsecond=0).timestamp()
    for entry in os.listdir(folder):
        path = os.path.join(folder, entry)
        if os.path.isfile(path) and os.path.getsize(path) == 0 and entry in sizes:
            with open(path, "r+b") as handle:
                handle.truncate(sizes[entry])
            os.utime(path, (stamp, stamp))


class Recorder:
    def __init__(self, udid, path):
        self.process = subprocess.Popen(
            ["xcrun", "simctl", "io", udid, "recordVideo", "--codec", "hevc", "--mask", "black", "--force", path],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        self.start = None
        deadline = time.time() + 20
        for line in self.process.stdout:
            if "Recording started" in line:
                self.start = time.time()
                break
            if time.time() > deadline:
                break
        if self.start is None:
            self.process.kill()
            raise SystemExit("simctl did not start recording")

    def stop(self):
        self.process.send_signal(signal.SIGINT)
        try:
            self.process.wait(timeout=30)
        except subprocess.TimeoutExpired:
            self.process.kill()


def run_steps(steps, device, nzb_folder, marks, take_dir):
    for step in steps:
        op, args = step[0], step[1:]
        if op == "wait":
            time.sleep(float(args[0]))
        elif op == "mark":
            marks[args[0]] = time.time()
        elif op == "launch":
            extra = [a.replace("{take}", take_dir) for a in (args[0] if args else [])]
            simctl("launch", "--terminate-running-process", device.udid, BUNDLE_ID, *device.app_args, *extra)
        elif op == "app":
            simctl("launch", device.udid, BUNDLE_ID, *device.app_args)
        elif op == "quit-app":
            simctl("terminate", device.udid, BUNDLE_ID, check=False)
        elif op == "files":
            # Files first, as a user would switch to it: opening the folder
            # from the app instead puts a "◀ dl-nzb" link in the status bar.
            simctl("launch", device.udid, "com.apple.DocumentsApp")
            device.show_folder(os.path.join(device.on_my_iphone, args[0]))
        elif op == "app-files":
            device.show_folder(os.path.join(device.app_documents(), args[0]))
        elif op == "open":
            path = os.path.join(nzb_folder, f"{args[0]}.nzb")
            device.open_url("file://" + urllib.parse.quote(path))
        elif op == "wait-file":
            path = os.path.join(device.app_documents(), args[0])
            deadline = time.time() + float(args[1] if len(args) > 1 else 120)
            while not os.path.exists(path):
                if time.time() > deadline:
                    raise SystemExit(f"timed out waiting for {args[0]}")
                time.sleep(0.1)
        elif op == "size-files":
            size_files(os.path.join(device.app_documents(), args[0]), device.nzb_dir)
        else:
            raise SystemExit(f"unknown step {step}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("storyboard")
    parser.add_argument("take_dir")
    parser.add_argument("--app", required=True, help="the built dl-nzb.app for the iOS Simulator (Debug)")
    parser.add_argument("--device", default="dl-nzb Demo iPhone", help="simulator name; created if missing")
    parser.add_argument("--delete-device", action="store_true", help="delete the simulator when done")
    args = parser.parse_args()

    with open(args.storyboard) as handle:
        board = json.load(handle)
    take = board["take"]
    take_dir = os.path.abspath(args.take_dir)
    os.makedirs(take_dir, exist_ok=True)

    nzb_dir = os.path.join(take_dir, "nzbs")
    subprocess.run([sys.executable, os.path.join(HERE, "make-nzbs.py"), nzb_dir], check=True, stdout=subprocess.DEVNULL)
    write_cli_config(os.path.join(take_dir, "demo-server.toml"), take.get("cli_config", {}))

    udid, _ = find_or_create_device(args.device)
    prepare_device(udid, take.get("appearance", "dark"))
    simctl("launch", udid, "com.apple.DocumentsApp")  # creates On My iPhone on a new simulator
    time.sleep(2)
    device = Device(udid, nzb_dir)

    simctl("terminate", udid, BUNDLE_ID, check=False)
    simctl("uninstall", udid, BUNDLE_ID, check=False)
    simctl("install", udid, os.path.abspath(args.app))
    defaults = {"simulate": True, "notifyWhenFinished": False}
    defaults.update(take.get("defaults", {}))
    for key, value in defaults.items():
        kind = "-bool" if isinstance(value, bool) else "-int" if isinstance(value, int) else "-string"
        simctl("spawn", udid, "defaults", "write", BUNDLE_ID, key, kind, str(value).upper() if isinstance(value, bool) else str(value))
    device.app_args = ["-simulate", "YES"]

    folder = take.get("folder", "Downloads")
    nzb_folder = put_nzbs(device, folder, take["nzbs"], nzb_dir)
    marks = {}
    print("setting up")
    run_steps(take.get("setup", []), device, nzb_folder, marks, take_dir)

    print("recording")
    recorder = Recorder(udid, os.path.join(take_dir, "take.mov"))
    marks["start"] = recorder.start
    try:
        run_steps(take["steps"], device, nzb_folder, marks, take_dir)
    finally:
        marks["stop"] = time.time()
        recorder.stop()
    with open(os.path.join(take_dir, "take.json"), "w") as handle:
        json.dump({"kind": "iphone", "movie": "take.mov", "start": recorder.start, "device": args.device}, handle, indent=2)
    with open(os.path.join(take_dir, "marks.json"), "w") as handle:
        json.dump(marks, handle, indent=2)
    for name, when in sorted(marks.items(), key=lambda item: item[1]):
        print(f"  {when - recorder.start:7.2f}  {name}")

    simctl("terminate", udid, BUNDLE_ID, check=False)
    if args.delete_device:
        simctl("shutdown", udid, check=False)
        simctl("delete", udid)
        print(f"deleted {args.device}")


if __name__ == "__main__":
    main()
