# dl-nzb for Mac, iPhone and iPad: product and UX spec

Status: approved scope (2026-10-04). Owner decisions are recorded in `apple/CONTRACT.md` § Decisions; every **[D]** below is resolved there.

## Goal

One job, done well: open an NZB, get the files, see how it is going. Everything else stays out of the way.

## Principles

1. **The download is the content.** Liquid Glass lives only on the navigation and control layer (toolbar, sheets,
   menus, the few buttons that float). Rows, progress bars and inspector content sit on standard materials. No glass
   on glass, no custom bar or sheet backgrounds, no tinted toolbar glyphs except one primary action.
2. **Standard components first.** `List`, `Form(.grouped)`, `Settings`, toolbars, `.inspector`,
   `ContentUnavailableView`, `confirmationDialog`. Built on the 26 SDK this way, the app picks up the macOS/iOS 27
   glass refinements (diffusion, the system tint slider, edge-to-edge sidebars, tighter window corners) by recompiling,
   with no redesign.
3. **Honest progress.** Determinate whenever the size is known. Status text says what is happening ("Repairing 12
   damaged blocks"), never "Loading". Never alternate a spinner and a bar.
4. **No setup friction.** First launch asks for the server once, with Test Connection. If the dl-nzb CLI config
   exists, offer to import it.
5. **Quiet by default.** One notification per finished job. No sounds, no badges that are not counts.
6. **Copy:** plain English, Title Case buttons, sentence-case toggles, one-line footers, no em dashes, no emoji,
   middle dot (·) as the separator.

## Core flows

1. **First launch, no server** → sheet "Connect to Your Usenet Server": Host, Port, Use SSL, Username, Password,
   Connections. [Test Connection] shows the real result inline (auth vs DNS vs TLS, not "pool exhausted").
   [Import from dl-nzb CLI] appears when `~/Library/Application Support/dl-nzb/config.toml` exists.
2. **Open an NZB.** Finder double-click, drop on the Dock icon or window, ⌘O, or on iOS Files / Safari / share sheet.
   The job joins the queue and starts when it reaches the front. Opening never creates a second window.
   Opening an NZB that is already in the list or already downloaded asks: [Download Again] [Show in Finder].
3. **Monitor.** The row shows phase, one progress bar and one status line. Selecting it shows detail.
4. **Problems are inline, specific and actionable.**
   - Pre-flight finds too many missing articles for PAR2 → row becomes *Needs Attention*: "9% of articles are
     missing and there is not enough recovery data." [Download Anyway] [Remove]
   - Encrypted archive and no password → *Password Required* with a field. Passwords in the NZB are used
     automatically. **[D]**
   - Wrong credentials / server unreachable → alert with [Open Settings]; the queue pauses rather than failing every job.
   - Disk too small for the job → checked before download starts, not discovered as write errors.
5. **Done.** Notification "Download Finished · Sintel (8.2 GB)" with [Show in Finder]. The row shows a reveal
   button; double-click reveals.
6. **Quit while downloading** → "Quit dl-nzb? 1 download will continue next time you open dl-nzb." Downloaded data
   is kept and the queue is restored on relaunch. **[D]**

## Job lifecycle and status copy

Phases: Queued → Connecting → Checking → Downloading → Downloading Recovery Data (only when needed) → Verifying →
Repairing → Extracting → Renaming → Finished | Failed | Cancelled. Side states: Paused, Needs Attention.

| State | Status line (monospaced digits) | Bar |
|---|---|---|
| Queued | Waiting · 8.2 GB | none |
| Checking | Checking 21,840 articles… | indeterminate |
| Downloading | 3.1 GB of 8.2 GB · 84 MB/s · 1 min left | bytes |
| Paused | Paused · 3.1 GB of 8.2 GB | frozen, secondary tint |
| Recovery | Downloading recovery data · 140 MB of 400 MB | bytes |
| Verifying / Repairing | Repairing 12 damaged blocks · 43% | phase fraction |
| Extracting | Extracting · 2 of 5 | phase fraction |
| Finished | 8.2 GB · Finished in 3 min (· Repaired 12 blocks) | none, green check |
| Failed | 9% of articles missing · Retry | none, red mark |

## macOS

### Scenes and app behaviour
- One main window (`Window` id `main`) plus `Settings`. File opens go through `NSApplicationDelegateAdaptor`
  `application(_:open:)`, buffered until the UI exists and re-opening the window if it was closed
  (`.onOpenURL` does not fire for a `Window` scene). Closing the window does not quit while jobs run.
- Menus: File › Open… ⌘O; a Downloads menu (Pause All, Resume All, Show in Finder, Retry, Remove from List ⌫,
  Move to Trash ⌘⌫); View › Show Inspector ⌘I; Settings… ⌘,. Every toolbar command also exists in the menu bar.
  Dock menu: Pause All / Resume All.
- `.nzb` is declared with an **imported** UTI `com.zephleggett.dl-nzb.nzb` (conforms to `public.xml`, `public.data`;
  tags `nzb`, `application/x-nzb`), `CFBundleDocumentTypes` role Viewer, rank Default.

### Main window **[D: layout]**
- Toolbar (system glass): trailing [+ Add] (the single prominent action), [Pause All / Resume All], [Inspector].
  Window subtitle carries the aggregate: "2 downloading · 84 MB/s" (`navigationSubtitle`, not on glass).
- List (inset). Row: content icon (film / music / archive / doc by dominant extension) · middle-truncated release name ·
  thin determinate bar · status line · trailing inline buttons (pause/resume, cancel while active; reveal when done).
  Context menu: Show in Finder, Open, Quick Look, Copy Name, Pause/Resume, Retry, Remove from List, Move to Trash.
  Drag to reorder the queue. Space = Quick Look on finished rows.
- Inspector (⌘I): name, size, destination with [Show in Finder]; a phase checklist (Check · Download · Verify ·
  Repair · Extract) with ✓ / progress / skipped; Files (name, size, state); Details (average speed, time, articles
  missing, blocks repaired, server, NZB category/password present).
- Empty: `ContentUnavailableView` "No Downloads" · "Open an NZB file or drop one here." [Open NZB…]. Whole window
  highlights as a drop target while a file hovers.

### System integration
- Finder and the Dock's Downloads stack show progress on the job folder through a published Foundation `Progress`
  (`kind .file`, `fileOperationKind .downloading`), the way Safari does it. **[D: Dock icon bar]**
- `ProcessInfo.beginActivity(.userInitiated)` while any job is active: blocks idle sleep and App Nap (jetlink
  learned App Nap throttles an in-process engine).
- Notifications via `UNUserNotificationCenter`, one per finished or failed job, with Show in Finder.
- Optional menu bar item **[D]**.

## iPhone and iPad **[D: scope]**
- Single `NavigationStack` "Downloads" (tab bars are for sections; this app has one). Toolbar: [+] trailing
  (`fileImporter` for `.nzb`), [Settings] leading → sheet with a grouped `Form`.
- Row: name, status line, App Store-style progress ring on the trailing edge (tap = pause/resume). Swipe: Pause/Resume,
  Cancel, Delete. Tap → detail screen (same sections as the Mac inspector) with Share and Open in Files.
- iPad: `NavigationSplitView` list + detail at regular width; `.readableWidth()` lists; size classes, not idiom
  (iOS 27 makes iPhone apps resizable).
- Files land in Documents/Downloads/<job>, visible in Files › On My iPhone › dl-nzb (`UIFileSharingEnabled`,
  `LSSupportsOpeningDocumentsInPlace`).
- Background: one `BGContinuedProcessingTask` covers the queue run, submitted when the user starts a download. The
  system draws its own progress Live Activity (title = job, subtitle = "3.1 of 8.2 GB"), reported in bytes so it
  never looks stalled. On expiry or swipe-away the engine pauses cleanly and resumes when the app returns.
- iOS defaults differ: 20 connections (memory), PAR2 backs off on serious thermal state, a warning before
  downloading on an expensive network.

## Settings (Mac: Settings window with toolbar panes; iOS: grouped Form sheet)

| Pane | Setting | Default | Notes |
|---|---|---|---|
| General | Download folder | ~/Downloads | one subfolder per job, always |
| | Start downloads automatically | on | off = jobs wait for [Start] |
| | Remove finished downloads from the list | Manually | Manually / When dl-nzb quits / After one day |
| | Notify when downloads finish | on | |
| | Prevent sleep while downloading | on | Mac |
| | Show in menu bar | off | Mac, **[D]** |
| Server | Host, Port, Use SSL/TLS | —, 563, on | port follows SSL (563/119) unless edited |
| | Username, Password | — | password in Keychain (`kSecClassInternetPassword`, NNTPS) |
| | Connections | 30 (iOS 20) | footer: "Your provider sets the maximum." |
| | [Test Connection] | | inline result |
| Processing | Repair with PAR2 | on | |
| | Extract archives | on | |
| | Delete archives after extracting | off | |
| | Delete PAR2 files after repairing | off | |
| | Rename obfuscated files | on | |
| Advanced | Check availability before downloading | Automatic | Automatic / Always / Never |
| | Download all recovery files up front | off | |
| | Speed limit | off | **[D]** |
| | Verify server certificate | on | |
| | Retry attempts | 2 | |
| | Flush files to disk when finished | off | fsync |
| | [Import from dl-nzb CLI…] [Reset All Settings] [Show Logs] | | |

Dropped from the GUI: `usenet.timeout` (unused by the engine), `download.force_redownload` (unused),
`logging.*` (unused), `create_subfolders` (always on: post-processing works on the whole folder), `tuning.*` except
the ones above, `notifications.*` (terminal bell).

## Architecture

- **Repo:** Cargo workspace. Root crate `dl-nzb` keeps the CLI (terminal deps behind a `cli` feature). New
  `ffi/` crate `dl-nzb-ffi` (UniFFI proc-macros, staticlib, `panic = "unwind"`). `apple/` holds `DlNzbKit/`
  (SwiftPM: engine xcframework `binaryTarget`, a nonisolated bindings target, `DlNzbKit` stores/models/settings/
  Keychain, `DlNzbUI` shared views), `macos/` and `ios/` (xcodegen `project.yml`, generated projects committed),
  `scripts/` (xcframework build, sign, dmg) and a Makefile. Mirrors jetlink.
- **Engine refactor (Rust):** a library `Engine` + `Job` API that the CLI and the app both use (one code path).
  Per-job context replaces the process-wide shutdown flag (cancellation token, pause signal, observer, stats).
  Observer events: phase changes, 4 Hz progress, availability verdict, file finished, warnings, PAR2 report,
  final summary. Also: unique per-job folder, free-space check, NZB `<head>` metadata (title, password,
  category), filename-collision fix, `test_connection` with the real error, open-file limit raised by the engine,
  idle connections closed when the queue empties.
- **Swift:** Swift 6, strict concurrency complete, explicit `@MainActor @Observable` stores fed by an
  `AsyncStream` from the engine listener (jetlink pattern). Blocking FFI calls never on the main actor.
  Settings in `UserDefaults` through an `@Observable` settings class; password in Keychain.
- **Targets:** macOS 26.0, iOS/iPadOS 26.0, arm64. Xcode 26.6. No availability checks needed for Liquid Glass APIs.
- **Distribution:** App Store-ready on both platforms. Mac is sandboxed and ships in two flavours from one tree:
  App Store (no Sparkle) and Direct (Developer ID, notarized, Sparkle). iOS via TestFlight / App Store. The owner
  holds all dl-nzb and par2-rs copyright; unRAR's licence permits use with its paragraph in Acknowledgements.
  Remaining risk is App Review Guideline 5.2.3: positioned as "bring your own server and NZB", no indexer or search.

## QA and review
- Live downloads of the NZBs in ~/Downloads (scope **[D]**), covering open via Finder (`open`), Dock drop, ⌘O,
  duplicate open, pause/resume, cancel, quit + relaunch, wrong password, no network, iOS simulator flows.
- Light/Dark, Reduce Transparency, Increase Contrast, narrow and wide windows, VoiceOver labels, Dynamic Type (iOS).
- Adversarial review by separate agents: HIG conformance from screenshots, usability heuristics, copy, accessibility,
  and engine correctness of the new Job API. Confirmed findings are fixed and re-verified.

## Out of scope for v1
Indexer search and RSS, categories and scripts, multiple servers, scheduling, remote control, App Store builds,
widgets, custom Live Activities (the system one from continued processing covers iOS), TLS backend change
(SecureTransport → rustls is a separate engine decision).
