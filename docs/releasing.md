# Cut a release

A pushed `v*` tag runs the Release workflow. It builds the command-line
binaries and the Mac app, publishes the GitHub release, then puts the iPhone
and iPad app on TestFlight. What it needs once: [publishing setup](publishing.md).

## Steps

1. Set `version` in `Cargo.toml` and `ffi/Cargo.toml`, then run `cargo check`
   so `Cargo.lock` follows.
2. In `CHANGELOG.md`, rename `## [Unreleased]` to `## [0.8.0] - 2026-10-05`.
   That section becomes the release notes and the Mac app's update notes, so
   write what people will notice, not how it was done.
3. Commit, then tag and push (replace `0.8.0`):

```bash
git tag v0.8.0
git push origin v0.8.0
```

4. Watch **Actions > Release**. The Mac job takes longest: it builds the Rust
   engine, then the app, and waits for Apple's notary service twice.
5. Check the release page: `dl-nzb-0.8.0-macOS.dmg`, `appcast.xml`, the five
   command-line binaries, `SHA256SUMS`, and notes ending in **Install**.
6. The **TestFlight** job starts once the release is out and waits while
   Apple processes the upload, usually 5 to 30 minutes. Testers in the
   internal group get it then; the public link gets it after Beta App Review.

## What the Release workflow does

| Job | What |
| --- | --- |
| Check the tag | The tag must match `Cargo.toml` (`apple/scripts/check-version.sh`), so a typo fails in seconds, not after notarizing. |
| CLI | `dl-nzb-linux-x86_64`, `-linux-aarch64`, `-macos-x86_64`, `-macos-aarch64`, `-windows-x86_64.exe`. |
| Mac app | The Direct flavour, universal (Apple silicon and Intel): Developer ID signed, notarized and stapled, in a notarized DMG, with `appcast.xml` for updates. |
| GitHub release | Made last, once every build passed, so a failure leaves nothing half published. Notes come from `scripts/changelog.py`. |
| TestFlight | `.github/workflows/testflight.yml`, after the release. Not for a prerelease. |

- Prerelease: a hyphen in the version (`0.8.0-rc.1` in both `Cargo.toml`
  files, tag `v0.8.0-rc.1`). The Mac app inside says `0.8.0`, since an app's
  version must be `X.Y.Z`. Installed copies are not offered it: the update
  feed is the newest full release's. Nothing goes to TestFlight.
- A build failed before the release job: nothing was published. Fix it,
  delete the tag (`git push --delete origin v0.8.0 && git tag -d v0.8.0`) and
  tag again.
- TestFlight failed: **Re-run failed jobs**. A build already uploaded is
  published as it is, not uploaded again.
- A bad Mac update: delete its feed (`gh release delete-asset v0.8.0
  appcast.xml`) so no more Macs take it. Sparkle never offers an older
  version, so the fix is a new release.

## CI

Every push and pull request runs `.github/workflows/ci.yml`. A change to docs,
Markdown or `site/` alone builds nothing.

- Rust on Ubuntu and macOS: `cargo fmt --check`, `cargo clippy -D warnings`,
  `cargo test --workspace`.
- The Rust engine for the four Apple targets, built once and shared by the
  next three jobs. It is cached by the Rust sources, so a push that leaves the
  Rust alone skips the build, and so does a release of a commit CI built.
- Swift: the DlNzbKit tests, the Mac app's tests and the iPhone app's tests
  on a simulator. It also checks that the committed Xcode projects match
  `project.yml`. Run `make -C apple mac-project ios-project` and commit when
  that fails.
- An ad hoc universal Mac app, kept for 7 days as the run's artifact.
- shellcheck on `apple/scripts` and `apple/ios/scripts`. swift-format lint
  reports but never fails the run.

Every workflow builds with Rust 1.92.0 (`RUSTUP_TOOLCHAIN`, which wins over
`rust-toolchain.toml`) and Xcode 26.6 (`.github/actions/select-xcode`), so a
runner image update cannot change the compiler under a release.
