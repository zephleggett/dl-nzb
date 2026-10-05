# Build contract: dl-nzb engine ↔ FFI ↔ Swift

Every workstream builds against this file. If you need to change a name or shape here, change it here first and say
so in your hand-back. Product/UX rules live in `apple/SPEC.md`; read it.

## Decisions (owner-approved 2026-10-04)

- iOS: full on-device app for iPhone + iPad (engine runs on device). Mac: list + trailing inspector. System accent.
- Settings: the app's own (UserDefaults + Keychain), with a first-launch import from the CLI's `config.toml`.
- Engine v1: pause/resume, resume after quit/relaunch, RAR passwords, speed limit (plus Job API, per-job cancel,
  free-space check, NZB metadata, per-job folders, filename-collision fix).
- Mac extras: menu bar item (off by default), Dock icon progress bar (custom tile only while downloading, restored
  after), Shortcuts actions (App Intents), Sparkle updates (feed URL empty unless set at release, like jetlink).
- **App Store is a target** (owner is the sole copyright holder of dl-nzb and par2-rs; the unRAR licence allows use in
  any software if its paragraph is reproduced). So: the Mac app is **sandboxed** (`network.client`,
  `files.user-selected.read-write`, `files.downloads.read-write`, `files.bookmarks.app-scope`); the download folder
  is a security-scoped bookmark; CLI import goes through an open panel pointed at the CLI config path. Two Mac
  flavours from one source tree: **App Store** (no Sparkle) and **Direct** (Developer ID + Sparkle), selected by
  target/configuration. Both apps get a privacy manifest and an Acknowledgements screen (unRAR paragraph + Rust
  crate and Sparkle notices). We make builds submission-ready; we never upload or submit.
- SDK: Xcode 26.6, macOS 26.0 / iOS 26.0 deployment, arm64. Look forward to 27 through standard components only.
- **Release machinery like jetlink (owner, 2026-10-05):** CI signs and notarizes the Mac Direct builds for BOTH
  arm64 and x86_64 (so the xcframework gains an x86_64-apple-darwin slice) and releases iOS to TestFlight, using the
  same Apple Developer account/team and the same workflow shape and secret names as jetlink. We write the workflows;
  we never push, upload or run them against the real account from here.
- **Demo videos and docs like jetlink/zoompilot:** Mac and iPhone demo videos (mp4 + animated webp) recorded from the
  apps in a demo mode with **generic, legitimate file names only** (Blender open movies, Linux ISOs). No movie/TV
  release names from ~/Downloads may appear in previews, screenshots, demos or store material. README + user guides
  follow jetlink's short, plain, direct style.
- **Site refresh (owner, 2026-10-05):** `site/` (static, base16-eighties terminal style, Cloudflare via
  `wrangler.jsonc`) gains the Mac and iPhone apps: demo webp linked to mp4, Mac DMG + TestFlight links beside the
  CLI install, plainer copy. Keep "no daemon, no web UI" positioning and the owner's minimal taste. Preview locally
  only; deploying is the owner's call.
- Git: branch `feat/apple-app`. **Workstream agents do not commit**; the lead commits at milestones.

## Ownership (do not edit outside your paths without saying so)

| Path | Owner |
|---|---|
| `src/**`, `tests/**`, root `Cargo.toml`, `README.md` CLI notes | Rust engine |
| `ffi/**`, `apple/scripts/build-xcframework.sh`, `apple/DlNzbKit/Sources/{DlNzbFFI,DlNzbRust}/**` | FFI |
| `apple/DlNzbKit/**` (everything else), `apple/Makefile` | Kit |
| `apple/macos/**` | macOS app |
| `apple/ios/**` | iOS app |
| `apple/Resources/**` (icon, menu bar glyph) | Icon |

## 1. Rust library API (`dl_nzb::engine`)

One code path: the CLI is rewritten as one observer of this API. No terminal code (indicatif, `src/ui`, stdin
prompts, `eprintln!`/`println!`) may run inside `engine`, `download`, `processing` or `nntp` when used by the app;
terminal deps move behind a default-on `cli` feature. The process-wide `shutdown` flag is replaced by a per-job
context (cancellation token, pause signal, observer, stats); the CLI's Ctrl-C maps to stopping its jobs.

```rust
pub struct Engine;                       // Clone (Arc inside). Owns one NNTP pool for the configured server, built lazily.
impl Engine {
    pub fn new(config: Config) -> Result<Engine>;            // validates; no network
    pub fn update_config(&self, config: Config);             // affects jobs started afterwards; pool rebuilt when idle
    pub fn set_speed_limit(&self, bytes_per_sec: Option<u64>); // live, engine-wide token bucket; None = unlimited
    pub async fn test_connection(server: &UsenetConfig) -> Result<ServerCheck>; // connect + auth, real error kind
    pub fn inspect(nzb_path: &Path) -> Result<NzbInfo>;      // parse only, no network
    pub fn start(&self, request: JobRequest, observer: Arc<dyn JobObserver>) -> JobHandle; // spawns on the current tokio runtime
    pub fn reprocess(&self, output_dir: PathBuf, passwords: Vec<String>, observer: Arc<dyn JobObserver>) -> JobHandle; // post-processing only (e.g. after a password is supplied)
    pub async fn shutdown(&self);                            // stops every job resumably, closes the pool
    pub fn config(&self) -> Config;                          // added: current config
    pub fn speed_limit(&self) -> Option<u64>;                // added: what set_speed_limit stored
}
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime>; // added: multi-thread runtime with the bounded blocking pool the engine expects (FFI uses it)

pub struct JobRequest {
    pub nzb_path: PathBuf,
    pub output_dir: PathBuf,       // the exact job folder; the caller chooses and de-duplicates it; engine creates it
    pub passwords: Vec<String>,    // user's, newest first: tried in order for encrypted RARs, BEFORE the NZB's own
                                   // (<meta type="password">, then {{password}} in its title or file name)
    pub preflight: Preflight,      // Auto (scan only when the NZB has no PAR2) | Always | Never
    pub on_unrepairable: OnUnrepairable, // Stop (finish with Outcome::Unrepairable) | Continue
    pub free_space_hint: Option<u64>, // added: bytes free on the output volume as the caller sees them; when Some, the
                                      // free-space check uses it instead of statvfs (iOS passes
                                      // volumeAvailableCapacityForImportantUsage: statvfs leaves out purgeable space)
    pub title: Option<String>,     // added 2026-10-05: the job's name (the app passes the item's release title). Renaming
                                   // (deobfuscate_file_names) calls an obfuscated main file after it, made safe for a file
                                   // name; None = the NZB's title (NzbInfo.title), as is a title that makes no file name
                                   // (".."). Never the folder's name, which the caller may have de-duplicated ("Name 2").
                                   // A title ending in the file's extension isn't given it twice; characters that show as
                                   // nothing (format characters: bidi controls, zero-width space/joiners, BOM, soft hyphen,
                                   // word joiner; Hangul fillers, variation selectors) are dropped.
                                   // Kept in the sidecar, so `reprocess` uses it too.
}

impl JobRequest { pub fn new(nzb_path, output_dir) -> Self; } // added: Auto / Stop / no passwords / no hint / no title
pub trait JobObserver: Send + Sync { fn on_event(&self, event: JobEvent); } // events serialised, in order; may call JobHandle methods
pub struct NullObserver;               // added: ignores events

pub enum JobEvent {
    Phase(JobPhase),
    Progress(JobProgress),          // at most 4 Hz (only when changed), plus one on every phase change and one at the end of each download phase
    Availability(AvailabilityInfo), // after a pre-flight scan
    Warning(String),                // one plain-English sentence
    Finished(JobSummary),           // exactly once, always last
}

pub enum JobPhase { Connecting, Checking, Downloading, DownloadingRecovery, Verifying, Repairing, Extracting, Renaming }

pub struct JobProgress {
    pub phase: JobPhase,
    pub bytes_done: u64, pub bytes_total: u64,   // download phases: this phase's file set in NZB article bytes (the unit of
                                                 // NzbInfo.total_bytes), advanced as each article settles so it ends exactly at
                                                 // bytes_total; Verifying: bytes hashed; Extracting: bytes unpacked; else 0
    pub speed_bps: f64,                          // per-job wire speed, smoothed (EMA ~2 s), counted as bytes arrive;
                                                 // 0 while paused and after 3 s with no byte (offline, stalled)
    pub eta_secs: Option<u64>,                   // None until bytes have flowed steadily for 3 s since the phase began,
                                                 // a resume, or a stall; None above 1 day or below 1 KiB/s
    pub files_done: u32, pub files_total: u32,
    pub articles_failed: u64,
    pub fraction: f64,                           // 0...1 within the current phase
    pub detail: Option<String>,                  // e.g. "2 of 5", current archive or file name
    pub paused: bool,
    pub damaged_blocks: u64,                     // Verifying: damaged blocks found so far; Repairing: blocks being rebuilt
                                                 // ("Repairing 12 damaged blocks"); 0 otherwise
}

pub struct JobHandle;                // Clone
impl JobHandle {
    pub fn pause(&self);             // download phases only: stops within about a second. Requests in flight are
                                     // abandoned (their articles go back to the queue, no retry spent), every connection
                                     // of the job is closed (idle pool ones too), the sidecar is saved; resume reconnects
    pub fn resume(&self);
    pub fn stop(&self);              // cancel promptly (no waiting out read timeouts); keeps data + sidecar so start() resumes
    pub fn is_finished(&self) -> bool;
    pub async fn wait(&self) -> JobSummary;
}

pub enum Outcome { Completed, CompletedWithIssues, Failed, Stopped, NeedsPassword, Unrepairable }

pub struct JobSummary {
    pub outcome: Outcome,
    pub message: Option<String>,     // one sentence for the UI when not Completed
    pub error_kind: Option<ErrorKind>, // set when Failed because of an error: the queue pauses on Auth/Dns/Connect/Tls/Timeout
                                     // instead of failing every job, and shows DiskFull as Needs Attention
    pub output_dir: PathBuf,
    pub files: Vec<OutputFile>,      // final user-facing files in output_dir: name (relative), bytes; no par2/nfo/sfv/srr,
                                     // .partial, hidden files, or volumes of archives that extracted
    pub nzb_files: Vec<FileReport>,  // added: per NZB file, NZB order: name, path, bytes, articles_total, articles_failed
    pub data_bytes: u64, pub wire_bytes: u64, // decoded non-PAR2 bytes written; plaintext bytes read in the transfer window
    pub elapsed_secs: f64, pub download_secs: f64, // download_secs = transfer window (workers start to last article written)
    pub check_secs: f64, pub post_secs: f64,       // added: pre-flight scan time; verify/repair/extract/rename time
    pub articles_total: u64, pub articles_failed: u64,
    pub par2: Par2Report,            // ran, verified_ok, damaged_blocks, repaired_blocks, repaired (bool), skipped_reason
    pub archives_extracted: u32, pub archives_failed: u32, pub files_renamed: u32,
    pub availability: Option<AvailabilityInfo>,
    pub resumable: bool,             // true iff Stopped/Failed, the sidecar remains, and start() has work left (download
                                     // or unfinished post-processing); false for every other outcome
}

pub struct AvailabilityInfo { pub articles_total: u64, pub articles_missing: u64, pub missing_bytes: u64,
    pub recovery_bytes: u64, pub verdict: Verdict /* Complete | Repairable | Unrepairable | Unknown */ }

pub struct NzbInfo { pub title: String /* <meta title> unless it looks obfuscated, else file stem */, pub passwords: Vec<String>,
    pub category: Option<String>, pub total_bytes: u64, pub data_bytes: u64, pub par2_bytes: u64,
    pub files: Vec<NzbFile> /* name (as written: sanitized, de-duplicated), bytes, segments: u32, kind: FileKind (Data|Par2|Archive|Other) */,
    pub content_kind: ContentKind /* Video|Audio|Archive|Image|Document|Software|Other, by dominant bytes */ }

pub struct ServerCheck { pub greeting: String, pub tls: bool, pub latency_ms: u32 }

pub enum ErrorKind { Config, Auth, Dns, Connect, Tls, Timeout, Protocol, Nzb, Io, DiskFull }  // on DlNzbError
// Connect: refused (also a 400/502 greeting, and "too many connections" in reply to the login), unreachable, or
// reset/closed by the server, including mid-TLS-handshake; Auth: any other refused login;
// Tls: a handshake that failed on its own terms (certificate, protocol), or a server that answered it in plain text
// (SSL on a plaintext port: "The server doesn't use SSL on this port."); Timeout: no answer in time.
// DlNzbError::kind() -> ErrorKind; DlNzbError::user_message() -> one or two plain sentences, never credentials and
// never a library's wording in parentheses (e.g. "The connection to news.example.com was lost.");
// DlNzbError::Job { kind, message } carries a finished job's error back as an error (added)
pub fn cli_config_import() -> Option<(Config, PathBuf)>;   // reads the CLI's config file if present (password included)
pub fn cli_config_path() -> Option<PathBuf>;              // added: real-home path even when sandboxed (for the open panel)
pub fn cli_config_import_from(path: &Path) -> Result<Config>; // added: parse a user-picked file (sandboxed import)
```

Behaviour the API guarantees:
- **Resume:** a per-job sidecar in `output_dir` (`.dl-nzb-job.json`, written atomically every 2 s / 1024 articles,
  on pause, at phase ends and when the job ends) records the NZB fingerprint, the articles written per file, finalized
  files, finished download phases and a passed PAR2 verification (with a size/mtime stamp of the folder's files).
  `start()` on a folder with a matching sidecar continues: written articles and finalized files are not fetched again,
  `.partial` files are opened without truncation, the first `Progress` of the phase already counts the bytes on disk,
  the pre-flight scan is skipped, and a finished download goes straight to post-processing. A mismatched fingerprint
  or unreadable sidecar is a fresh job (with a `Warning`). `Completed` removes the sidecar; every other outcome keeps
  it, and `reprocess` skips PAR2 verification when it passed on files unchanged since, and counts the articles it
  records as never downloaded (holes with no PAR2 to fill them are not `Completed`). Renaming (by PAR2's file table
  or by the title) carries PAR2's verdict to the new names and saves it at once, so a job stopped during or after
  renaming resumes (`start()` or `reprocess`) without needing the PAR2 files `delete_par2_after_repair` deleted. A
  stop that lands after the job's last step finished changes nothing: the job ends as it would have; a `Stopped` job
  always has work left.
- **`reprocess` of a download that never finished** (the sidecar says a download phase didn't finish, or a
  `<name>.partial` is left without `<name>`): no post-processing (it renames and deletes files the download must
  find when `start()` finishes it); the job ends `Failed`, "The download did not finish.", with the data files it
  never finished counted as failed articles, `resumable` while the sidecar is there (which it keeps). Never
  `Completed`.
- **Free space** is checked before downloading (payload + recovery + extraction estimate, against
  `JobRequest.free_space_hint` when given, else statvfs); failure is
  `ErrorKind::DiskFull` with a sentence giving the shortfall.
- **Post-processing** only touches `output_dir`. Encrypted archive with no working password → `Outcome::NeedsPassword`
  (download complete, `reprocess` can finish it). Never reported as success.
- **Open-file limit** is raised by the engine, not by `main.rs`.
- **Connections** (2026-10-05): no pool warm-up before downloading. Each download worker opens its own connection
  (at most `max_concurrent_connections` connecting at once) and starts work as soon as it is up, so one slow
  connection holds up only its worker, never the job. The job's one `Connecting` check asks again (`retry_attempts`
  times, with backoff) when the server turns it away (400/502 greeting, "too many connections" at login, refused,
  reset); a rejected password, unknown host or TLS failure (SSL on a plaintext port included) ends the job at once.
- **Lost connections** (2026-10-05): when a connection is lost (reset, closed, a reply out of step, a timeout) the
  requests in flight go back to the queue, no retry spent. The one whose reply was being read becomes a suspect,
  fetched on its own from then on (its reply gets 15 s to start, not 60 s); the others aren't blamed. An article
  that loses the connection 3 times on its own is given up as missing, so one article that always kills the
  connection doesn't hold up the job: PAR2 repairs it like any missing article. That holds while the losses
  concentrate on a few articles. When more than `max(2, 1% of the phase's articles)` distinct articles have lost
  the connection on their own and at least one of them arrived later, the server is flaky: such articles go back
  to the queue instead (no retry spent), and one that still loses it 12 times on its own is given up as lost to
  the connection, never missing. The phase doesn't count as finished when the server is to blame (flaky, or 2 or
  more articles given up on their own and nothing requested after the first loss arrived, or they are most of the
  phase's articles), or the articles whose connection failed before their request could be sent are at least half
  of the phase's failures: the job ends `Failed` with `ErrorKind::Connect`, "The connection to <server> kept
  dropping.", `resumable` (the queue pauses), as when the server can't be reached at all. "412" refusals are
  refusals (retried, then missing), whatever shares their window. Without PAR2 in the NZB, giving an article up
  as missing can't help, so articles that lost the connection count as the connection's: when they are most of
  what failed, the job ends that same resumable way (an article that always kills the connection then fails it on
  every resume); when articles the server says it hasn't are most, the job is "missing" as before.
- **Article placement** (2026-10-05): each article says where its part goes (`=ypart`). A part may end at most at
  `max(16 x the file's NZB sizes added up, 4 MiB x its highest NZB part number) + 4 MiB` (the part number counted
  as at least the parts listed, at most twice as many); one past it is a bad article (retried, then missing). The
  limit comes from the NZB alone: no article raises it, and a `=ybegin size=` inside it is fine. Honest releases
  whose NZB sizes are under-reported, 0, the decoded size, or miss an entry still land. What it bounds: a hostile
  article in an ordinary NZB (parts of 256 KB or more) can make its file at most about 16 times what the NZB says,
  and a hostile NZB can only make files as large as it claims (its sizes x 16, or 4 MiB per part number).
- **Pause** (2026-10-05): stops within about a second, not after draining the window: see `JobHandle::pause`. Articles
  written before the pause are in the sidecar within about half a second of the pause; abandoned requests never are.
  A pause during the pre-flight availability check (`Checking`) takes effect when downloading starts: the check runs
  to its end, then the job waits paused before its first article.
- Idle pool connections close when no job has used the pool for 60 s, and at once when a job pauses.
- A job whose config changes the server settings gets a fresh pool; running jobs keep theirs.
- **Speed limit** (merged 2026-10-05): one lock-free token bucket per engine, hooked into `CountingReader::poll_read`
  (`src/nntp/speed_limit.rs`); throttles article bodies only, live changes apply within ~50 ms; config
  `download.speed_limit` and CLI `--limit-rate`.

## 2. FFI (`ffi/`, crate `dl-nzb-ffi`, UniFFI 0.32.2 proc-macros)

Mirrors §1 one-to-one, Swift-cased by UniFFI. As built (module `DlNzbFFI`, C module `DlNzbCore`):

```swift
class Engine {                                   // owns a multi-thread tokio runtime (engine::runtime()); no signal handlers
  init(config: EngineConfig) throws              // EngineError; validates, no network
  func updateConfig(config: EngineConfig) throws // validates, then Engine::update_config + set_speed_limit
  func setSpeedLimit(bytesPerSecond: UInt64?)    // nil or 0 = unlimited
  func speedLimit() -> UInt64?
  func testConnection(server: ServerConfig) async throws -> ServerCheck
  func inspect(nzbPath: String) throws -> NzbInfo                 // blocking file I/O: call off the main actor
  func start(request: JobRequest, listener: JobListener) -> JobHandle
  func reprocess(outputDir: String, passwords: [String], listener: JobListener) -> JobHandle
  func shutdown() async
}
class JobHandle { func pause(); func resume(); func stop(); func isFinished() -> Bool; func wait() async -> JobSummary }
                                                 // pause() stops within about a second and closes the job's sockets
                                                 // (call it before an iOS background task expires); resume() reconnects
protocol JobListener: AnyObject, Sendable { func onEvent(event: JobEvent) }   // #[uniffi::export(foreign)]
func cliConfigPath() -> String?                                  // engine::cli_config_path
func cliConfigImport(path: String) throws -> ImportedConfig?     // nil: no file; .Config: names no server

struct ServerConfig { host, port: UInt16, ssl, verifyCertificate, username, password, connections: UInt16, retryAttempts: UInt8 }
struct EngineConfig { server: ServerConfig, autoPar2Repair, autoExtractRar, deleteRarAfterExtract, deletePar2AfterRepair,
                      deobfuscateFileNames, downloadAllPar2, fsyncOnFinalize, speedLimitBytesPerSecond: UInt64? }
struct ImportedConfig { config: EngineConfig, downloadDir: String?, source: String }
struct JobRequest { nzbPath, outputDir: String, passwords, preflight, onUnrepairable, freeSpaceHint: UInt64?, title: String? }
enum JobEvent { phase(phase:), progress(progress:), availability(info:), warning(message:), finished(summary:) }
enum EngineError: Error { Config(message:), Auth, Dns, Connect, Tls, Timeout, Protocol, Nzb, Io, DiskFull }  // flat: case = kind,
                                                                    // message = DlNzbError::user_message()
// Records/enums as §1, Swift-cased: JobPhase, JobProgress (damagedBlocks), JobSummary (errorKind, nzbFiles, checkSecs,
// postSecs), Outcome, ErrorKind, Par2Report, OutputFile, FileReport, AvailabilityInfo, Verdict, NzbInfo, NzbFile, FileKind,
// ContentKind, ServerCheck, Preflight (auto/always/never), OnUnrepairable. Paths are Strings; u64 is UInt64. Plain enums
// are CaseIterable.
```

- Listeners are called on engine threads, sometimes with the job's event lock held: the Swift listener only yields
  into an `AsyncStream` continuation (and finishes it after `finished`); it never calls back into the engine.
- Swift task cancellation does not reach Rust; `JobHandle.stop()` does. Async methods run on the engine's runtime.
- Built with the `release-ffi` profile (`panic = "unwind"`) by `apple/scripts/build-xcframework.sh`
  (`make -C apple xcframework`): `apple/DlNzbKit/Frameworks/DlNzbCore.xcframework` (git-ignored) for
  `aarch64-apple-darwin`, `aarch64-apple-ios`, `aarch64-apple-ios-sim` (deployment 26.0), headers and module map in
  `Headers/DlNzbCore/` (the `_Builtin_std*` lines removed, uniffi-rs#2917); the Swift bindings regenerated into
  `apple/DlNzbKit/Sources/DlNzbFFI/` (committed) by the workspace's own `uniffi-bindgen` binary (`ffi/uniffi-bindgen.rs`,
  same UniFFI version); `DlNzbUI/Resources/rust-crates.json` regenerated from `cargo tree`/`cargo metadata`.
- Native libraries (rustc `--print native-static-libs`): macOS `-lc++ -liconv` + Security, CoreFoundation; iOS the same
  without libc++ (unrar_sys only asks for it on macOS). The `DlNzbFFI` target links c++, iconv, Security and
  CoreFoundation everywhere. No SystemConfiguration.
- Toolchain: rustup `1.92.0` (`~/.cargo/bin` first on PATH; Homebrew rustc has no iOS std). The script sets that up.

## 3. Swift (`apple/DlNzbKit`, swift-tools 6.2, platforms macOS 26 / iOS 26)

Products: `DlNzbKit` (models, engine protocol, simulated engine, stores, settings, Keychain, persistence),
`DlNzbUI` (shared views and formatters), `DlNzbRust` (`public final class RustEngine: DownloadEngine`, `init()`, plus
the static `freeSpaceHint(for:)`), built on the `DlNzbFFI` target (the `DlNzbCore` binary
target + generated bindings, nonisolated). Only the apps import `DlNzbRust`; the package needs the xcframework to
resolve, and the kit's make targets build it when missing. On the Mac, `DownloadQueue` holds security-scoped access to
the download folder for each job from start (or reprocess) to finish, on top of `SettingsStore`'s.

Swift-native mirrors of §1 types live in `DlNzbKit/Models` (`JobPhase`, `JobProgress`, `JobSummary`, `Outcome`,
`AvailabilityInfo`, `NzbInfo`, `ServerCheck`, `EngineError`), so views never import generated bindings.

```swift
public protocol DownloadEngine: AnyObject, Sendable {
  func apply(_ settings: EngineSettings) async throws
  func setSpeedLimit(bytesPerSecond: Int64?) async
  func testConnection(_ server: ServerSettings, password: String) async throws -> ServerCheck
  func inspect(_ nzb: URL) async throws -> NzbInfo
  func start(_ request: JobRequest) async throws -> JobSession
  func reprocess(directory: URL, passwords: [String]) async throws -> JobSession
  func importCLIConfig() async -> ImportedSettings?
  func importCLIConfig(from url: URL) async throws -> ImportedSettings  // added: a picked file; RustEngine uses the CLI's
                                                                       // own parser (default: Kit's CLIConfig, for the simulator)
  func shutdown() async
}

public final class JobSession: Sendable {   // events: AsyncStream<JobEvent>; pause(), resume(), stop()
}
```

- `SimulatedEngine` implements `DownloadEngine` with believable timing and every outcome (used by previews, Swift
  tests, and the apps when launched with `-simulate YES`).
- Stores are `@MainActor @Observable final class` with `private(set)` state (jetlink pattern): `AppModel`
  (composition root, idempotent `launch()`), `SettingsStore` (UserDefaults via `didSet`, injectable suite,
  `.preview()`; password through `Keychain` as `kSecClassInternetPassword`, protocol NNTPS), `DownloadQueue`.
- `DownloadQueue` owns: items (`DownloadItem`: id, stored NZB copy, title, output folder, added/finished dates,
  `NzbInfo`, state, latest progress, summary), `add(urls:)` (copies the NZB into Application Support/dl-nzb/Queue,
  de-duplicates the output folder name, flags duplicates), scheduler (one job in network phases at a time,
  post-processing may overlap the next download), pause/resume/stop/retry/remove/trash, password and
  "download anyway" follow-ups, persistence (`queue.json` in Application Support, restored on launch, interrupted
  jobs resume), retention policy, aggregate speed and fraction for Dock / menu bar / Finder.
- `DlNzbUI` holds the pure, tested presentation logic: status line per state (SPEC table), byte/speed/ETA
  formatting, content-kind symbols, `PhaseChecklist`, the iOS `ProgressRing`, the shared settings form sections
  (Server, Processing, Advanced) used by both apps and by onboarding.

## Conventions (from jetlink, see its `macos/` and `JetlinkKit/`)

Swift 6 language mode, `SWIFT_STRICT_CONCURRENCY: complete`, explicit `@MainActor` (no default-isolation setting);
blocking FFI work never on the main actor. xcodegen `project.yml` with the generated `.xcodeproj` committed.
swift-format: 2-space indent, 160 columns (`apple/.swift-format`). Swift Testing only, `@Test("plain sentence")`.
`#Preview("state")` per meaningful state using preview factories. `os.Logger(subsystem: "com.zephleggett.dl-nzb")`,
never `print`. `///` docs that explain why; British spelling in comments. Copy: plain English, Title Case buttons,
sentence-case toggles, no em dashes, no emoji. Bundle IDs `com.zephleggett.dl-nzb` (Mac and iOS),
UTI `com.zephleggett.dl-nzb.nzb` (imported), BG task identifiers `com.zephleggett.dl-nzb.download.*`.
