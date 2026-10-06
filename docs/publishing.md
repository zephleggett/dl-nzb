# Publishing setup

What the release workflows need once: the signing secrets, the Mac update key
and the App Store Connect records. Cutting a release: [releasing.md](releasing.md).

## Signing secrets

Set under **Settings > Secrets and variables > Actions** on
`zephleggett/dl-nzb`. They have the same names, and can have the same values,
as jetlink's: one Apple team signs both. The update key is the exception.

| Secret | What |
| --- | --- |
| `MACOS_CERTIFICATE_P12_BASE64` | the Developer ID Application certificate with its private key, as a .p12, base64 encoded |
| `MACOS_CERTIFICATE_PASSWORD` | the .p12 password |
| `KEYCHAIN_PASSWORD` | any random string; it locks the temporary keychain the runner builds in |
| `NOTARY_KEY_ID` | the App Store Connect API key's ID |
| `NOTARY_ISSUER_ID` | that key's issuer ID |
| `NOTARY_PRIVATE_KEY_P8_BASE64` | the key's .p8 file, base64 encoded |
| `APPLE_TEAM_ID` | the team the iPhone app is signed for; without it nothing goes to TestFlight |
| `SPARKLE_ED_PRIVATE_KEY` | the Mac app's update signing key ([below](#mac-update-key)) |

- The API key is a Team key from **Users and Access > Integrations** in App
  Store Connect, with the **Admin** role. It notarizes the Mac app, uploads
  the iPhone app, and lets xcodebuild make the signing certificates.
- Encode a file with `base64 -i cert.p12 | pbcopy`, or straight into the
  secret: `base64 -i AuthKey_XXXX.p8 | gh secret set NOTARY_PRIVATE_KEY_P8_BASE64 --repo zephleggett/dl-nzb`.
- A fork without these secrets still releases: an ad hoc zip labelled
  unsigned, no DMG, no update feed, nothing on TestFlight.
  zephleggett/dl-nzb's release stops instead. The step **Report the signing
  mode** says which ran.

## Mac update key

The Mac app takes an update only when the DMG and the feed are signed with
this Ed25519 key. Its public half is `DLNZB_UPDATE_PUBLIC_KEY` in
`apple/macos/project.yml`, built into every copy as `SUPublicEDKey`. The
private half is the secret: base64 of the 32-byte seed, the format of
Sparkle's `generate_keys -x`.

```bash
gh secret set SPARKLE_ED_PRIVATE_KEY --repo zephleggett/dl-nzb < dl-nzb-ed25519-private.key
```

- It is dl-nzb's own key. Never reuse another app's.
- Back it up outside the repository. Installed copies trust only this key,
  so losing it makes updating them slow and awkward.
- The release checks the DMG's signature against the public key inside the
  app it just built. A secret that is not its other half stops the release
  instead of shipping an update no copy accepts.
- Installed copies check `releases/latest/download/appcast.xml` once a day,
  so they see the newest release that is not a prerelease. Only a build signed
  with both the Developer ID and this key gets that feed address; any other
  build never updates itself.

## App Store Connect, once

App Store Connect has no API for the first two.

1. Register the App ID `com.zephleggett.dl-nzb` under **Certificates,
   Identifiers & Profiles > Identifiers** (explicit, no extra capabilities).
2. Create the app under **Apps > +** with that bundle ID and the iOS
   platform. The Mac App Store flavour shares the record; add macOS when you
   submit it.
3. In **TestFlight**, fill in **Test Information** (description, feedback
   email). Beta App Review needs a way to try the app: put a test Usenet
   account in the review notes, or say what a reviewer can check without one.
4. Create an external group named **Public** and turn on its public link.
   Put the link in `TESTFLIGHT_LINK` at the top of
   `.github/workflows/release.yml`. While it is the placeholder
   (`XXXXXXXX`), the release notes leave out the iPhone app.

## iPhone app on TestFlight

The Release workflow ends with `.github/workflows/testflight.yml`. It builds
the Rust engine, archives the app, uploads it, waits while Apple processes it,
sets What to Test, adds the build to **Public** and submits it for Beta App
Review.

- The version is `Cargo.toml`'s and must be `X.Y.Z`; the build number is the
  commit count.
- xcodebuild makes an Apple Development certificate for the archive, since
  the runner has no key of its own. The job revokes it at the end.
- The job ends at the submission, so a rejection shows under **TestFlight** in
  App Store Connect, not in the run.
- **Actions > TestFlight > Run workflow** runs it on any branch. Unticked,
  **upload** only archives and exports, which checks the signing. Clear
  **group** to keep a build to the internal group.

The same script uploads from a Mac:

```bash
DLNZB_TEAM=ABCDE12345 ASC_KEY_ID=... ASC_ISSUER_ID=... ASC_KEY_PATH=AuthKey_XXXX.p8 \
  TESTFLIGHT_GROUP=Public make -C apple ios-testflight
```

`DLNZB_BUILD` overrides a build number already taken, and `TESTFLIGHT_NOTES`
the What to Test text. `python3 scripts/asc.py wait VERSION BUILD` shows
Apple's verdict on an upload, ITMS errors included, which App Store Connect's
build list leaves out.

## Mac App Store

**Actions > Mac App Store > Run workflow** archives the App Store flavour and
exports it for App Store Connect, signed with the same key. It never uploads:
the `.pkg` is the run's artifact, and submitting it with Transporter stays a
manual step. On a Mac with a team in `apple/macos/Config/Local.xcconfig`,
`make -C apple mac-archive` does the same.
