# Plan: Developer ID signing and notarization

Status: **deferred** (2026-10-18). Editage currently has two users, who
install the ad hoc signed build as described in README.md › Installing.
Pick this up when the app is shared more widely.

## Goal

The released disk image opens on any Mac without a Gatekeeper warning, however
it was downloaded: no `xattr`, no "Open Anyway". This does **not** mean
publishing on the Mac App Store. Distribution stays the GitHub release (or
any direct download), and there is no App Review.

Notarization uploads the app to Apple's automated notary service. Apple does
not publish it. Only the app is sent, never documents.

## Prerequisites (developer account, once)

1. **Developer ID Application certificate**: Certificates, Identifiers &
   Profiles › Certificates › +. Only the Account Holder can create it.
   Export it from Keychain Access as a `.p12` with a password.
2. **App Store Connect API key** for `notarytool`: Users and Access ›
   Integrations › Team Keys, Developer role. Note the Key ID and Issuer ID and
   download the `.p8` (it can be downloaded only once).
3. No App ID or provisioning profile: Editage uses no entitlements.

## Open decision

Where the certificate lives:

- **GitHub secrets** (the Release workflow signs and notarizes), or
- **the maintainer's Mac only** (sign and notarize locally with the
  certificate in the login keychain, then upload the disk image to the
  release by hand; CI keeps building ad hoc).

## Changes

### `packaging/macos/build-app.sh`

- If a Developer ID identity is configured (`DEVELOPER_ID`, e.g.
  `Developer ID Application: Name (TEAMID)`):
  - sign the app with it, `--options runtime --timestamp` (a secure
    timestamp is required for notarization);
  - build the disk image, then sign the `.dmg` with the same identity;
  - `xcrun notarytool submit <dmg> --key <p8> --key-id … --issuer … --wait`,
    failing the build if the status is not `Accepted` (print the log with
    `notarytool log` on failure);
  - `xcrun stapler staple <dmg>`, so the ticket is available offline;
  - check with `spctl --assess --type open --context context:primary-signature -v <dmg>`
    and `spctl --assess --type execute -v Editage.app`.
- Otherwise keep the current ad hoc signature, so local builds work without
  an account.
- Write the checksum after stapling (stapling changes the file).

### `.github/workflows/release.yml` (if the certificate goes to CI)

- Secrets: `MACOS_CERTIFICATE_P12` (base64), `MACOS_CERTIFICATE_PASSWORD`,
  `NOTARY_KEY_P8`, `NOTARY_KEY_ID`, `NOTARY_ISSUER_ID`.
- Before building: create a temporary keychain, import the `.p12`, allow
  `codesign` to use it (`security set-key-partition-list`), and write the
  `.p8` to a temporary file. Delete both in an `always()` step.
- Pass the identity and key details to the script as environment variables.
- Never echo the secrets.

### Docs

- README.md › Installing: replace the `curl`/`gh` and Open Anyway
  instructions with "download, open the disk image, drag to Applications".
- docs/platform-notes.md › App bundle: describe the Developer ID signature
  and notarization instead of the ad hoc signature.
- docs/architecture.md: no new dependencies (Apple command-line tools only).
- docs/manual-test-checklist.md: install from a browser download on a second
  Mac with no warning; `spctl` reports "Notarized Developer ID".

## Verification

1. Run the workflow manually; the job log shows the notary status `Accepted`
   and a successful `stapler staple`.
2. Download the artifact **with a browser** on another Mac and open the app:
   there is no warning.
3. `spctl --assess -vv /Applications/Editage.app` prints
   `source=Notarized Developer ID`.
4. Tag a release only after the manual run passes.
