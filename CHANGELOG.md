# Changelog

All notable changes to dl-nzb will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.7.0] - 2026-06-05

A polish-and-speed release: a refreshed interface, faster and lighter downloads,
and more accurate results.

### Changed
- **Refreshed terminal output.** Each download now opens with a clear title line,
  the progress bar tidies itself away as each step finishes (no more leftover
  "100%" bars stacking up), and you get a clean completion summary that lists your
  files. Colours and symbols are consistent from start to finish.
- **Colour control.** New `--color` option (`auto`, `always`, `never`) plus
  support for the `NO_COLOR` standard. Output is now clean — no stray colour
  codes — when you pipe dl-nzb into a file or another program.
- **Readable time estimates.** The download ETA reads like "2h 5m" instead of a
  raw number of seconds.
- **Quieter by default.** The terminal bell at the end of a download is now off
  by default (re-enable it with `notify_on_complete` in your config).

### Performance
- **Faster downloads**, especially against distant or busy servers, with a higher
  default connection count.
- **Finishes sooner after the last byte.** When everything arrives intact, dl-nzb
  skips a redundant integrity re-scan that used to run after downloading, and it
  begins downloading more quickly.
- **Much lower memory use** on large downloads.

### Fixed
- **Accurate completion status.** A download that needed PAR2 repair now correctly
  reports "Complete" instead of falsely warning about errors, and missing optional
  files (such as `.nfo`) no longer count as errors.

## [0.6.1] - 2026-06-05

### Fixed
- **"Too many open files" on NZBs with many files.** A download keeps one open
  file descriptor per output file (large releases have hundreds) plus one per
  connection, and the inherited soft limit is often only 256 on macOS. The limit
  is now raised at startup (best-effort, Unix).
- **PAR2 recovery is now correctly deferred for obfuscated releases.** PAR2-on-
  demand keyed off the `.volNN+MM` filename to tell the index from the recovery
  volumes; obfuscated releases lack that marker, so every recovery volume was
  downloaded up front even when the data was complete. Deferral is now by size
  (keep the smallest par2 — the index — and defer the larger volumes), so it
  works regardless of filenames.
- **PAR2 repair of large multi-file releases** (via par2-rs 0.3.1): fixes
  `Total data blocks exceed GF(2^16) limit` caused by duplicated FileDescription
  packets being counted multiple times.

## [0.6.0] - 2026-06-01

### Changed — download engine rewrite (reliability & throughput)
- Replaced the per-connection 50-segment batch model with **article-granularity
  work distribution** over a lock-free `flume` MPMC queue plus a **continuous
  per-connection sliding window** (`tuning.pipeline_depth`, default 4). No
  connection sits idle while work remains, there is no drain-then-refill gap,
  and the tail is bounded by the window rather than a whole batch. The group is
  selected once per connection (`BODY <message-id>` is group-independent per
  RFC 3977). `tuning.pipeline_size` is replaced by `pipeline_depth`.
- **Per-article retry taxonomy.** A mid-pipeline wire error no longer re-downloads
  (and discards) a whole 50-segment batch: already-received segments are kept and
  only the failed article is retried. Outcomes are classified `Ok` / `Missing`
  (430/423, permanent) / `DecodeFailed` (bounded retry, `tuning.decode_retry_cap`,
  default 3) / `Transient` (connection-level, retried uncounted on a fresh
  connection). Connections are reused unless genuinely poisoned. This removes the
  retry-amplification that inflated download volume on flaky links.

### Added
- **PAR2-on-demand** (`post_processing.download_all_par2`, default `false`):
  recovery volumes are deferred and fetched only when a data segment is actually
  missing or corrupt (each delivered segment is yEnc-CRC verified, so an intact
  payload needs no recovery). Saves the (often 10–30%) recovery bytes on the
  common case. Set `download_all_par2 = true` for the old eager behaviour.
- **PAR2-driven filename deobfuscation:** obfuscated files are identified by the
  MD5 of their first 16 KiB against the PAR2 file table and renamed to their real
  names before repair — the authoritative method, independent of the heuristic.
- **Accurate pre-flight scan:** every segment is `STAT`-ed (not just the first of
  each file), producing a byte-accurate availability report, a complete skip set
  (missing articles are never fetched; partially-missing files still download
  their present segments), and a repairability estimate (available recovery bytes
  vs missing data bytes). Runs in every mode.
- JSON summary gains `data_bytes` and `par2_bytes` alongside `wire_bytes`.

### Fixed
- **Ctrl-C now cancels the whole process.** First interrupt requests a graceful
  shutdown and **skips post-processing**; a second forces an immediate exit. PAR2
  repair and RAR extraction poll the shutdown flag (par2-rs reconstructs into temp
  files and only commits on success, so an aborted repair never corrupts data).
  Interrupted runs keep `.partial` files and report `success: false`.
- Corrected the misleading "~10–15% overhead" comments (real yEnc/NNTP framing is
  ~2–4%; the rest was avoidable retry/par2 over-download, now addressed).

### Dependencies
- Requires **par2-rs v0.3.0**: read-only `Par2Info` metadata, cancellable
  temp-file-safe repair, and a fix for multi-block Reed-Solomon repair (input
  blocks are now numbered in little-endian File-ID order per the PAR2 spec, so
  real par2cmdline sets needing ≥2 recovery blocks repair correctly).

## [0.5.0] - 2026-05-12

### Fixed
- Download speed was computed against the on-disk (decoded) byte total
  divided by wall-clock time that included the availability check; the
  reported number didn't match the live progress bar and undercounted the
  actual network throughput by ~12% (yEnc overhead) plus another ~5% of
  setup time. Now we track wire bytes (`segment.bytes` from the NZB) per
  file in `FileState::wire_bytes_downloaded` and divide by the active
  transfer window captured by the writer task (first segment to last).
- Guarded the speed calculation against very short downloads (<50 ms)
  that would otherwise divide by ~0.
- Field name `average_speed_mbps` was misleading — the math is 1024-based
  bytes per second (MiB/s), not megabits per second.

### Changed
- **Breaking (JSON schema):** `DownloadSummary` now exposes
  `wire_bytes`, `transfer_time_seconds`, and `average_speed_mib_per_sec`
  in place of `average_speed_mbps`. `total_size` and
  `download_time_seconds` are unchanged.
- The human-readable "Downloaded X" line now includes ` at <N> MiB/s`.
- `Downloader::download_nzb` returns a third tuple element — the active
  transfer `Duration` — so library callers can compute speed against the
  same window the binary uses.

## [0.4.0] - 2026-05-12

### Fixed
- **Correctness:** yEnc `=ypart` headers are now parsed; segments are placed at
  the exact byte offset reported by the producer rather than the cumulative
  encoded sizes from the NZB. The previous behaviour left ~22 KB zero-filled
  gaps at every segment boundary, which made PAR2 repair fail on otherwise-
  clean downloads.
- Connections are now marked poisoned and dropped if a pipelined read fails
  mid-stream, preventing the next caller from reading leftover bytes.
- The deobfuscator no longer suffixes `_1` onto clean filenames that already
  match the directory name, and tightened heuristics to avoid flagging release
  names with lots of digits (S04E33, 1080p, x265, year, etc.) as obfuscated.
- yEnc `pcrc32` is verified against the decoded payload; mismatches are
  treated as missing segments so PAR2 can attempt recovery.

### Added
- Mock-NNTP integration test (`tests/mock_nntp_download.rs`) covering the
  full pipeline (correct offsets, missing segments, retry path).
- Eager connection pool pre-warming so the first batch of segments lands on
  N parallel connections instead of ramping up over the file.
- Per-segment retry via a producer/coordinator/worker design (jobs go back
  through the coordinator instead of being held by the worker).
- Files are downloaded into `*.partial` and atomically renamed on completion.
  An interrupted run leaves the `.partial` in place without overwriting any
  previously-good file at the final name.
- Health probe uses `DATE` (RFC 3977) instead of `NOOP`, which is rejected by
  some providers (e.g. `news.newsgroup.ninja`).
- `--json` mode suppresses decorative ANSI prints from the downloader and
  post-processor so stdout contains only the JSON document.

### Changed
- Removed the lazy connection pool design; connections are warmed up front.
- yEnc decoder is a standalone module with full `=ybegin` / `=ypart` / `=yend`
  parsing and CRC32 verification.

## [0.2.0] - 2025-12-08

### Added
- New `TuningConfig` for performance parameters (pipeline_size, connection_wait_timeout, large_file_threshold)
- Centralized `patterns` module with regex-based RAR and PAR2 detection
- Smooth byte-level progress reporting for PAR2 verification
- Real-time RAR extraction progress via file size monitoring
- Connection wait feedback ("Waiting for connection..." messages)
- Comprehensive unit tests for file pattern matching

### Changed
- Replaced par2cmdline-turbo (C++ FFI) with par2-rs (pure Rust)
- Split `post_process.rs` (714 lines) into focused modules:
  - `post_processor.rs` (157 lines) - orchestration
  - `par2.rs` (236 lines) - PAR2 verification/repair
  - `rar.rs` (308 lines) - RAR extraction
- Improved connection pool management with exponential backoff
- Reduced default connections from 40 to 20 for stability
- Use `Arc<Config>` in download hot path to reduce cloning
- Progress bar templates now use `expect()` with descriptive messages
- PAR2 message parsing now uses level + content matching for reliability

### Fixed
- Connection pool exhaustion when downloading many files
- Error logs breaking terminal progress bar rendering
- Mutex lock panics on poisoned locks (now gracefully handled)
- RAR multi-part detection edge cases (.part01, .part001, .part0001)

## [0.1.0] - 2025-01-13

### Initial Release

First public release of dl-nzb, a fast Usenet NZB downloader written in Rust.

#### Features

- Fast parallel downloads with configurable connection pooling
- Built-in PAR2 verification and repair
- Built-in RAR extraction support
- Automatic file deobfuscation for common obfuscated naming patterns
- Real-time progress display with speed and ETA
- SSL/TLS support for secure connections
- Configurable via TOML config file or environment variables
- Command-line overrides for all major settings
- Async I/O with Tokio runtime for maximum throughput
- Memory-efficient streaming downloads
- Automatic retry on failed segments
- Smart connection management with health checks
- Cross-platform support (Linux, macOS, Windows)

#### Commands

- `dl-nzb <file.nzb>` - Download NZB files
- `dl-nzb test` - Test server connection
- `dl-nzb config` - Show configuration file location and contents
- `dl-nzb -l <file.nzb>` - List NZB contents without downloading

#### Configuration

- Auto-generated config file on first run
- Support for local `dl-nzb.toml` override files
- Environment variable overrides with `DL_NZB_` prefix
- Configurable download directory, connections, memory limits, and post-processing options

#### Technical Details

- Single binary with no runtime dependencies
- PAR2 support via pure Rust par2-rs library with SIMD
- RAR extraction compiled in
- Optimized build with LTO
