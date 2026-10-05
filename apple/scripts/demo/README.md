# Demo videos

Tools for the README's demo videos (`docs/images/{mac,iphone}-demo.{mp4,webp}`), in the look of jetlink's: a dark stage, the window or phone floating on it, one caption at a time, a green badge on sped-up parts, and an end card.

A take is recorded with the simulated engine (`-simulate YES`) and the NZBs from `make-nzbs.py`, whose names are Blender open movies and Linux ISOs. A storyboard (`iphone.json`, `mac.json`) says what the take does (`take`) and how it is cut (`cut`).

```sh
# From the repository root.
D=apple/scripts/demo

# iPhone: build Debug for the simulator, record, cut
xcodebuild build -project apple/ios/dl-nzb.xcodeproj -scheme "dl-nzb Debug" -configuration Debug \
  -destination 'generic/platform=iOS Simulator' -derivedDataPath /tmp/demo-dd
$D/run-iphone.py $D/iphone.json /tmp/take-iphone --delete-device \
  --app /tmp/demo-dd/Build/Products/Debug-iphonesimulator/dl-nzb.app
$D/compose.py $D/iphone.json /tmp/take-iphone docs/images/iphone-demo.mp4
$D/to-webp.py docs/images/iphone-demo.mp4 docs/images/iphone-demo.webp

# Mac: quit dl-nzb, build Debug, record (hands off the Mac while it runs), cut
CONFIG=debug OUT="$PWD/apple/build" apple/scripts/build-mac.sh appstore
$D/run-mac.sh $D/mac.json /tmp/take-mac apple/build/dl-nzb.app
$D/compose.py $D/mac.json /tmp/take-mac docs/images/mac-demo.mp4
$D/to-webp.py docs/images/mac-demo.mp4 docs/images/mac-demo.webp
# Quit the demo copy, then:
rm -rf ~/Downloads/"dl-nzb Demo" ~/Library/Containers/com.zephleggett.dl-nzb/Data/tmp/dl-nzb-scratch-demo
defaults delete ~/Library/Containers/com.zephleggett.dl-nzb/Data/Library/Preferences/com.zephleggett.dl-nzb.scratch.demo
```

To check a cut without encoding, write stills: `$D/compose.py $D/iphone.json /tmp/take-iphone /tmp/cut.mp4 --stills 1,6,12`. A cut can be redone from the same take as often as needed; only `take` changes need a new recording.

| File | What it does |
|---|---|
| `make-nzbs.py` | Writes the demo NZBs (realistic RAR sets, MKV and ISO with PAR2) and prints the scenario each plays; `--states` adds one per end state |
| `run-iphone.py` | Records an iPhone take on its own simulator ("dl-nzb Demo iPhone"), driven through `simctl` only |
| `record-mac.swift` | Records a region around the app's window with ScreenCaptureKit at 2x, HEVC, and logs the window frames |
| `drive-mac.swift` | Plays the Mac steps: eased pointer glides, clicks and keys, targets found by their Accessibility label; `--dump <bundle id>` lists labels |
| `run-mac.sh` | Builds the two Swift tools into `apple/build/demo`, sets up, records and drives a Mac take |
| `compose.py` | Cuts the take: stage, window mask and shadow or drawn iPhone, captions, badge, push-in, end card; H.264 1920x1200 at 60 fps |
| `to-webp.py` | The README preview: 960x600, 15 fps, quality 72, looping |

## Storyboards

`take.steps` run while recording; `take.setup` runs before. Each step is a JSON array, listed at the top of `run-iphone.py` and `drive-mac.swift`. A `["mark", "name"]` step notes the time, and the cut refers to it: `"from": "opened+1.5"`.

Each `cut.segments` entry takes part of the take (`from`, `to`), optionally sped up (`speed`, which adds an "N× speed" badge unless `badge` says otherwise), with a `caption`, a push-in (`zoom`: `focus` as a share of the screen or region, `scale`) and a crossfade from the segment before (`fade`). Segments with the same caption share one pill. `taps` draw a touch on the iPhone screen; `end` is the end card.

Captions: short, direct, no full stops ("Open an NZB from Files").

## Gotchas

- The NZBs' articles do not exist: takes only work with `-simulate YES`. The simulated engine writes empty files; `run-iphone.py` gives the ones Files shows their real sizes as sparse files.
- `SimulatedScenario` gives about one name in four a repair, from the high bits of a hash of the name; a name with "repair" in it always gets one. `make-nzbs.py` prints what each NZB will play, mirroring the Swift code: Tears of Steel repairs, so it is the hero of both takes.
- iPhone: the simulator copies the Mac's 24-hour clock, which turns the status bar's 9:41 into 09:41; `run-iphone.py` sets a 12-hour clock and US English (one reboot the first time).
- iPhone: `simctl openurl file:///…/X.nzb` opens an NZB in dl-nzb exactly as a tap in Files does, and `shareddocuments://<path>` opens Files at a folder. A file URL launches the app without launch arguments, so `simulate` is written to the app's defaults.
- iPhone: opening Files from inside the app puts "◀ dl-nzb" in the status bar; `files` steps launch Files first. The app's own folders (`app-files`) keep it, as Show in Files would.
- iPhone: `-importCLIConfig` also imports post-processing settings, so set those in the storyboard's `cli_config`, not in defaults.
- iPhone: the notification prompt cannot be answered without a tap, and `simctl privacy` has no notifications service, so takes run with `notifyWhenFinished` off. A prompt left open survives reinstalling the app; reboot the simulator.
- iPhone: the simulator has no `BGContinuedProcessingTask` (submitting fails with code 1), so the background beat shows the app's grace period, not the system's progress UI. For that, record a real iPhone.
- iPhone: `simctl io recordVideo` writes frames only when the screen changes, so the movie ends at the last change, earlier than the last mark. `compose.py` holds the last frame.
- Mac: an app-only ScreenCaptureKit filter makes macOS 26 swap the traffic lights for a sharing pill. `record-mac` uses a display filter that excludes every other app, so the background is black and `compose.py` masks the window (26 pt corners) from the logged window frames.
- Mac: `SCStreamConfiguration.backgroundColor` does not retain its `CGColor`; keep a reference or creating the stream crashes. Record HEVC, not ProRes (ProRes 4444 at 2x is about 77 MB/s).
- Mac: on macOS 26, posted CGEvents reset `HIDIdleTime` too (jetlink's notes say otherwise), so `drive-mac` counts only input newer than its own last event as someone using the Mac, and stops the take when it sees any (also during waits). It also checks the pointer is where it left it, and stops before a click or key if another app is in front. A tool that only sleeps never hears that the frontmost app changed; `drive-mac` runs the run loop while it checks.
- Mac: never open an NZB through Launch Services in a take (a Finder double-click, `open` without `-a`): the default .nzb app can be another copy of dl-nzb with real settings, which then fetches the made-up articles from the real server. `drive-mac`'s `open` names the app. Do not drag files from Finder either: a synthetic drag can drop the file on a sidebar folder and move it there. The Mac take shows only dl-nzb's own windows.
- Mac: `take.record_windows` says which other windows to record: `{}` for the app alone (the Mac take), `{"com.apple.finder": ["NZBs"]}` for those Finder windows only (`record-mac --include-window`), so the owner's other windows and the desktop stay out even where they sit behind the app. Finder may show whole paths in titles and a "Folder shared with File Sharing" banner; a storyboard that shows Finder can put the NZBs elsewhere with `NZBS=/Users/Shared/NZBs`.
- Mac: a relaunched app is a new process; `record-mac` also matches windows by the app's name, so its window is in the mask as soon as it shows.
- Mac: opening an NZB makes the app's window fade out and back in (about 0.3 s), even when the app is in front; the cut crossfades over it. A relaunched window comes back at its default size, so the take sets it again.
- Mac: the sandboxed app may only write in ~/Downloads, so `run-mac.sh` points it at "~/Downloads/dl-nzb Demo" with a `-downloadFolderBookmark <hex>` launch argument (`drive-mac --bookmark`), so Show in Finder shows only demo files.
- Mac: `-scratchState YES` keeps the real queue and settings out of the take; with `-scratchStateName demo` (Debug) the scratch queue survives the take's quit and relaunch. `run-mac.sh` clears it before a take and refuses a download folder that still has files; afterwards delete `~/Library/Containers/com.zephleggett.dl-nzb/Data/tmp/dl-nzb-scratch-demo` and the suite (`defaults delete ~/Library/Containers/com.zephleggett.dl-nzb/Data/Library/Preferences/com.zephleggett.dl-nzb.scratch.demo`).
- Mac: the menu bar gains a purple recording item while recording and, on a notched display, can push status items out of sight; keep the menu bar out of the region.
- Encoding: weak dither is smoothed away by x264 and the stage bands; `compose.py` uses Gaussian grain (σ 1.2), which survives. ffmpeg 8 tags only the matrix from the `-color_*` flags, so the filter graph adds `setparams`.
- WebP: Homebrew's ffmpeg has no WebP encoder; Pillow writes it. Frames that look the same are merged, comparing 4x4 averages so the grain does not count as change.
- Fonts: SF Pro Display from /Library/Fonts. This ffmpeg has no drawtext, so all text is drawn with Pillow.
