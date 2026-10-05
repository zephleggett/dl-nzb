# dl-nzb for Mac, iPhone and iPad: report

Branch `feat/apple-app`, on top of v0.7.0 (`2f79cdb`). Nothing pushed, uploaded or submitted.
Spec: [SPEC.md](SPEC.md). Contract and decisions: [CONTRACT.md](CONTRACT.md).

dl-nzb is now a native app on Mac, iPhone and iPad as well as a command line tool. Open an NZB; it downloads,
repairs with PAR2 and extracts, and the files land in a folder of their own. The apps and the CLI share one
Rust engine.

## What shipped

**Engine (Rust, `dl_nzb::engine`)**
- One job API (Engine, JobHandle, events) used by the CLI and both apps. The CLI is one observer of it.
- Per-job stop and pause. Pause stops network use within about a second and spends no retries; stop keeps the
  data.
- Resume after stop, quit or crash: a `.dl-nzb-job.json` sidecar records written articles, finished files,
  PAR2's verdict and extracted archives. A resumed job fetches nothing twice.
- RAR passwords from the user, the NZB's `<meta>` and `{{password}}` in the NZB name. Data- and
  header-encrypted archives work. Extraction goes to a staging folder; only whole archives move into place.
  Links inside archives are skipped. unRAR is serialised process-wide.
- Speed limit: one lock-free token bucket per engine, live changes, `--limit-rate` in the CLI.
- Also:
  - A free-space check based on the work left.
  - NZB title, passwords and category read from the NZB.
  - Per-job folders, with post-processing confined to them.
  - Real PAR2 block counts.
  - A connection test that reports the real reason.
  - Plain-sentence errors.
  - Connection-limited providers handled without dropping articles.
  - A prompt failure, resumable, when the server goes away.
- Hardened against hostile NZBs and articles:
  - Message IDs and group names are validated, so a malicious NZB can't inject NNTP commands.
  - Line and body reads are capped.
  - Article placement is bounded.

**FFI.** `ffi/` (UniFFI 0.32.2) exports the engine's types directly. `apple/scripts/build-xcframework.sh` builds
`DlNzbCore.xcframework`: one universal macOS library (arm64 + x86_64), iOS and iOS Simulator, plus the Swift
bindings and the crate licences. `RustEngine` implements the Kit's `DownloadEngine`.

**Shared Swift (DlNzbKit, DlNzbUI).** Models, the queue, settings and app stores, a simulated engine for
previews and demos, shared rows, checklist, ring and settings sections, and all user-facing copy.

**Mac app.**
- Sandboxed, in two flavours from one tree: App Store, and Direct (Sparkle 2.10.0 with its sandboxed
  installer, dl-nzb's own update key). Release builds are universal.
- A list with a trailing inspector, onboarding with a one-time import from the CLI config, Settings panes.
- An optional menu bar item, Dock and Finder progress, sleep prevention, notifications and App Intents.
- A privacy manifest and Acknowledgements.

**iPhone and iPad app.**
- A Downloads list, with a split view on iPad, plus detail, a settings sheet and onboarding.
- Opens NZBs from Files, Safari and the share sheet.
- Downloads keep running in the background through BGContinuedProcessingTask, with a notification if
  background time runs out.
- A cellular gate, notifications, a privacy manifest and Acknowledgements.

**Release.**
- Workflows:
  - `ci.yml`.
  - `release.yml`: CLI binaries, plus a signed and notarized universal DMG with its Sparkle appcast.
  - `testflight.yml`.
  - `mac-app-store.yml`: archive only.
- All eight secrets are set on `zephleggett/dl-nzb`. See [docs/publishing.md](../docs/publishing.md) and
  [docs/releasing.md](../docs/releasing.md).

**Docs, demos, site.**
- The README and guides: [docs/mac-app.md](../docs/mac-app.md) and [docs/iphone-app.md](../docs/iphone-app.md).
- Demo videos: `docs/images/{mac,iphone}-demo.{mp4,webp}`.
- The site (`site/`): an Apps section and `privacy.html`. Not deployed.

## Test results (final HEAD)

| Suite | Result |
|---|---|
| Rust: unit, integration (engine jobs, resume, RAR passwords, speed limit, mock NNTP, 8 review- and QA-regression suites), FFI, doctest | 201 pass, 0 fail |
| `cargo clippy --workspace --all-targets -D warnings`, `cargo fmt --check` | clean |
| DlNzbKit + DlNzbUI + DlNzbRust (Swift Testing) | 144 pass |
| Mac app tests | 33 pass |
| iOS app tests | 39 pass |
| Mac (App Store and Direct) and iOS builds, Swift 6 strict concurrency | 0 warnings |
| iOS accessibility audit (`performAccessibilityAudit`, iPhone + iPad, light/dark, AX5, Increase Contrast) | no app failures (system bordered buttons only, see limitations) |
| actionlint, shellcheck (workflows and release scripts) | clean |

## Live QA

Real Usenet server and the NZBs in `~/Downloads`. The Mac ran the sandboxed App Store build, driven through the
real UI (System Events, a real CGEvent drag onto the Dock). iOS ran in the simulator.

| Check | Mac | iOS simulator |
|---|---|---|
| First launch, server import, Test Connection | PASS ("Connected · 35 ms") | PASS (debug import) |
| Finder double-click / Files and share sheet | PASS | PASS |
| Dock drop / + button | PASS (real drag) | PASS |
| ⌘O | PASS | n/a |
| Duplicate open | PASS | PASS |
| Pause, resume, stop (keep or delete data) | PASS | PASS |
| Quit and relaunch resume | PASS (4.33 → 4.36 GB, finished) | PASS (resumed at 721.9 MB) |
| Wrong server password | PASS (queue paused, alert) | PASS |
| Going offline (relay, this app only) | PASS after fix (alert in ~24 s, data kept) | PASS |
| Needs Attention | PASS | PASS |
| test.nzb | PDF, 36,858,798 B = archive header, 70 pages | n/a |
| S02E04 (1.14 GB) | 39 s, mkv 1,032,350,461 B = header, decodes | 37 s, byte-identical through pause/stop/quit/background/offline |
| Astro-Zombies (5.11 GB) | across a quit, PAR2 verified, mkv 4,501,263,166 B = header, decodes | n/a |
| 8 GB NZBs: pause, resume, stop and delete | PASS, nothing left | n/a |
| Dock, Finder, menu bar, sleep assertion | PASS (notifications await the owner's permission) | background fallback PASS |
| QA output deleted | yes | yes (app uninstalled) |

Bugs found in QA and fixed:
- A lost server took minutes to surface and fetched recovery data offline.
- A relaunch sometimes showed an empty list.
- Notification permission was cached as denied.
- Empty job folders were left behind.
- Finish times ignored earlier runs.
- Pause let requests already in flight finish.
- One slow connection held up the whole start.
- The ETA ballooned after a stall.
- Resets were reported as TLS errors.
- A folder suffix leaked into renamed files.

## Adversarial review

Independent agents reviewed the work. Each fix round was then checked by a separate verifier.

**Engine, FFI and security.** Three reviewers covered concurrency and resume, the unsafe unRAR FFI with the
UniFFI boundary, and untrusted input.

| Severity | Findings (fixed, with regression tests) | Verified |
|---|---|---|
| High | NNTP command injection from NZB message IDs (proof of concept sent a full POST) · unRAR global error state raced across jobs · workers without a connection gave up articles the server had · resume never extracted after "delete PAR2" + stop | yes |
| Medium | unbounded `=ypart` offsets · unbounded line reads · solid-archive skip ignored stop · RAR5 hardlinks outside the job folder · retry during stop grace · free-space check after renames · post-processing after an unfinished download · lost-server handling | yes |
| From the verification passes | PAR2 verdict lost to a late stop · part bound trusted unreliable NZB sizes · pre-flight scan under a connection limit · crafted part raising its own bound · title fallbacks · connection resets reported as missing articles · late stop after the last work · raw library text in warnings · single connect refusal · resume after PAR2 purge + rename · one poison article blocking a job · reprocess of an unfinished job · SSL on a plain port · flaky-server resets read as missing | yes: four further fix-and-verify rounds, each checked by a new independent verifier |
| Low | password Debug/printing, observer panics, event order, `is_finished`, job pruning, staging cleanup, password wiping, STAT codes, a struct-layout mismatch in the unrar_sys binding | yes |

**UI (HIG and Liquid Glass, usability, copy, accessibility).** Reviewers ran each app separately: the Mac with an
accessibility-tree dump and contrast measurements, iPhone and iPad with XCUITest audits at every Dynamic Type size.

| Severity | Findings | Second pass |
|---|---|---|
| High | light-mode status text contrast (2.0–3.7:1) · ⌫ removed an active download without asking · iOS swipe Delete removed the row before confirming (crash) · "Cancelled" for stopped jobs | VERIFIED |
| Medium | subtitle during post-processing · size changing between states · Keychain on the main thread · password and Download Anyway mouse-only · selected-row progress invisible · Dynamic Type layouts · digit blur · iPad centring · unlabelled password row · no notice after "Not Now" · silent background pause · labels | VERIFIED |
| From the second pass | alert titles wrapping mid-word · iOS row wording after a failed extraction · VoiceOver location text · iOS stall wording | fixed in `5435c81` |

Liquid Glass is used only on the system toolbars, sheets and the single prominent Add action. Content stays on
standard materials. Reduce Transparency and Increase Contrast come from the system components (checked in code
and with the audit).

## Screenshots

Legitimate names and a placeholder server only. Files are in [report/](report/).

| Mac | |
|---|---|
| ![](report/mac-01-main-light.png) | ![](report/mac-02-main-dark.png) |
| Main window, light | Dark |
| ![](report/mac-08-onboarding-sheet.png) | ![](report/mac-04-password-sheet.png) |
| Onboarding | Password Required |
| ![](report/mac-05-remove-confirmation.png) | ![](report/mac-09b-server-notice-light.png) |
| Remove confirmation | Server notice |
| ![](report/mac-06a-settings-general.png) | ![](report/mac-06b-settings-server.png) |
| Settings: General | Settings: Server |
| ![](report/mac-07-empty-state.png) | ![](report/mac-11-menu-bar-menu.png) |
| Empty state | Menu bar item |

| iPhone and iPad | | |
|---|---|---|
| ![](report/ios-01-iphone-list-light.png) | ![](report/ios-02-iphone-list-dark.png) | ![](report/ios-03-iphone-detail-downloading.png) |
| List, light | Dark | Detail |
| ![](report/ios-04-iphone-settings.png) | ![](report/ios-06-iphone-ax5.png) | ![](report/ios-08-iphone-server-notice.png) |
| Settings | Largest text size | Server notice |

![](report/ios-09-ipad-split-light-landscape.png)

More in [report/](report/): inspector, Processing and Advanced panes, Dock tile, iPad dark, Needs Attention,
Password Required.

Demo videos: [Mac](../docs/images/mac-demo.mp4) · [iPhone](../docs/images/iphone-demo.mp4) (simulated engine,
sped up where marked).

## Known limitations

- **Can't be checked here** (needs a device or the account): BGContinuedProcessingTask and its system Live
  Activity on a real iPhone (the simulator rejects submission); Developer ID signing, notarization and a real
  Sparkle update; TestFlight and Mac App Store uploads.
- **Contrast:** system bordered buttons (Enter Password…, Download Anyway, Pause, Stop) measure about 3.5:1 in
  light mode. That is Apple's own style and only a darker accent colour would change it. Secondary-label text
  measures about 3.95:1.
- **Missing .nfo:** a missing .nfo article triggers the whole recovery set, because the .nfo is part of the PAR2
  set (466 MB on one release in QA).
- **Pause during Checking:** a pause during the pre-flight availability check takes effect when downloading
  starts.
- **File-size bound:** a file can't grow past what its NZB claims: max(16 × declared bytes, 4 MiB × part number)
  + 4 MiB. A hostile article in a normal NZB can inflate a file at most about 16×, as sparse space; a hostile NZB can
  only make files as large as it claims.
- **Flaky connections:** losses spread across many articles are treated as the server's fault, so the job ends as a
  resumable connection problem. Losses that keep hitting one or two articles are treated as damage for PAR2.
  Without PAR2 they stay a connection problem: an article that always drops the connection fails the job
  resumably on every try instead of being called missing.
  A server that drops connections with a bare reset can still stall one read for up to 60 s on macOS (TCP
  challenge-ACK); a solo retry of a suspect article waits at most 15 s.
- **Encrypted RAR test coverage:** multi-volume encrypted RAR has no fixture; only single-volume RAR4 and RAR5
  are covered. unRAR handles the volume change itself.
- **TLS:** uses SecureTransport through native-tls (TLS 1.2, a deprecated API). Moving to rustls is a separate
  decision.
- **Logs:** Rust `tracing` logs aren't forwarded into the apps' logs. par2-rs pulls `tracing-subscriber` into the
  library build.
- **List redraws:** each 4 Hz progress event replaces the queue's item array. Per-item observation would cut list
  redraws further.
- **Demos:** the simulated engine drives them, not a real server.

## Owner steps

1. Answer the two macOS prompts from testing:
   - Notification permission for dl-nzb.
   - A test copy's request for the Downloads folder ("Don't Allow" is fine).
2. Register the bundle ID `com.zephleggett.dl-nzb` and create the App Store Connect record (iOS and macOS).
3. Add a `LICENSE` file with an App Store exception to the GPL (you hold all the copyright).
4. Set up TestFlight Test Information with a test Usenet account, and create the Public group. Set
   `TESTFLIGHT_LINK` in `release.yml` and replace the README, guide and site placeholders.
5. Deploy the site (privacy page) with `wrangler`. Tag a release matching `Cargo.toml` (bump to 0.8.0) to run
   `release.yml`.

## App Store submission checklist

Bundle ID `com.zephleggett.dl-nzb` (shared by Mac and iOS). Team 7Y92PH4BPZ. Nothing below has been submitted.

**One-time setup**
- [ ] Register the App ID; create the app record with iOS and macOS platforms.
- [ ] TestFlight: Test Information (feedback email, what to test), an external **Public** group, its link in
  `TESTFLIGHT_LINK`.

**Before the first review (both platforms)**
- [ ] Privacy policy URL: `site/privacy.html` once deployed (no data collected, password in the Keychain, connects
  only to the user's server).
- [ ] Support URL: GitHub issues or the site.
- [ ] App Privacy: "Data Not Collected".
- [ ] Export compliance: `ITSAppUsesNonExemptEncryption = false` (standard TLS only). Set.
- [ ] Age rating: no objectionable content; "Unrestricted Web Access: No".
- [ ] Category: Utilities.
- [ ] Review notes (guideline 5.2.3): "dl-nzb downloads files from a Usenet server the user already subscribes
  to, using an NZB file the user already has. It has no search, indexer, catalogue or link to any content." Give
  review a test Usenet account and a small legal NZB (a Linux ISO or a public-domain file).
- [ ] Licensing: the GPL App Store exception and `LICENSE` file (owner step 3). The unRAR paragraph and crate
  licences are in Acknowledgements.
- [ ] Screenshots with legitimate names: iPhone 6.9" (1320×2868), iPad 13" (2064×2752), Mac (2880×1800), light
  plus one dark. The simulated engine and `apple/scripts/demo/make-nzbs.py` produce the content.
- [ ] Description and keywords in plain wording, no piracy terms. Lead with "Download files from your Usenet
  server with an NZB."

**iOS**
- [ ] Background continued processing verified on a real device.
- [x] Privacy manifest: UserDefaults CA92.1, disk space E174.1/85F4.1, file timestamp C617.1. The engine uses
  `clock_gettime`, not `mach_absolute_time`, so no SystemBootTime entry is needed (checked with `nm`).
- [ ] Upload via `testflight.yml` (tag a release) or `make -C apple ios-testflight`.

**Mac App Store**
- [ ] Archive the App Store flavour (no Sparkle) via `mac-app-store.yml` or `make -C apple mac-archive`; upload
  with Transporter or an `-exportArchive` upload.
- [x] Sandbox entitlements only: network.client, user-selected read-write, Downloads read-write, app-scope
  bookmarks. No temporary exceptions in the App Store flavour.
- [ ] If asked why Downloads access is needed: "to save the files the user downloads".

**Direct**
- [ ] Tag `vX.Y.Z` matching `Cargo.toml`. `release.yml` signs, notarizes and staples the universal DMG and
  publishes `appcast.xml`. The Sparkle private key is in the repo secret and your login keychain (account
  "dl-nzb").
