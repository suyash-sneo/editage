# Editage

Editage is a small native text editor for age-encrypted files. When a
document is unlocked, its plaintext exists in application memory so you can
edit it normally. When you save, the editor encrypts the current text and
atomically replaces the encrypted file. The editor does not provide cloud
storage. Put the encrypted file wherever you normally keep files, including a
synchronized folder such as OneDrive.

The application tries to earn trust through predictability, transparency,
interoperability, restraint and inspectability, not through obscurity. You
should always be able to find out what is in memory, what is on disk, what is
encrypted, where temporary files go, and what failed and where. The Security
Inspector in the app shows this for each open document. The documents in
[`docs/`](docs/) explain the same things in more detail, for people who want
to check the code against the claims.

## What it is

- A plain text editor for one kind of file: a UTF-8 text document encrypted
  with a passphrase in the standard [age](https://age-encryption.org/v1)
  format.
- A native macOS application (AppKit). The editing surface is the system text
  view, so input methods, spelling, undo, find and replace, and VoiceOver work
  in the usual way.
- A portable Rust core (`crates/editage-core`) that holds all the logic that
  decides what happens to a document. It has no user interface code, so other
  frontends could reuse it.

## What it is not

- Not a password manager, vault, notes database or IDE.
- Not an Electron or web application.
- Not a sync product. It does not upload, download or merge anything. It
  saves a file to a folder. What happens to that folder afterwards is up to you
  and your sync software.
- Not an account system. There is no sign-up, no server and no licence key.
- No proprietary file format. Every file it writes is a standard age v1 file.
- No network access. The application makes no network requests. The
  dependency policy in [`deny.toml`](deny.toml) rejects common networking
  crates.
- No telemetry, analytics or crash reporting.

## Interoperability

Files are standard age v1 files protected by a passphrase (scrypt). You can
decrypt any file Editage saves without Editage:

```sh
age --decrypt notes.txt.age
```

Editage opens passphrase-protected age v1 files made by other age tools, in
both the binary and the ASCII-armored (`-----BEGIN AGE ENCRYPTED FILE-----`)
encoding. When you save, it keeps the encoding the file had, so an armored
file stays armored. New documents are saved in the binary encoding.

Current limits:

- Only passphrase-protected files are supported. Files encrypted to age
  recipients (public keys) are recognised and refused with an explanation.
- The decrypted contents must be valid UTF-8. Other contents are refused
  without being changed or shown.
- Files that ask for an scrypt work factor above 2^22 are refused before any
  key derivation is attempted. When Editage encrypts, it always uses 2^18, the
  same value the reference age implementation uses.

The test suite checks interoperability against files made by the reference
Go implementation of age (`tests/fixtures/`), and, as an opt-in test,
decrypts Editage's own output with the reference `age` command.

## Documentation

- [Architecture](docs/architecture.md): how the core and the frontend are
  split, module by module, and why each dependency is there.
- [Security model](docs/security-model.md): what is encrypted, when plaintext
  and passphrases exist in memory, which files are created, and what the
  application does not protect against.
- [Threat model](docs/threat-model.md): the risks the design addresses, the
  mitigation for each, and the tests that enforce them.
- [Document lifecycle](docs/document-lifecycle.md): the document state
  machine, locking, reloading, and new documents.
- [Save protocol](docs/save-protocol.md): the exact steps of a save and what
  is left on disk after a failure at each step.
- [Platform notes](docs/platform-notes.md): macOS specifics, and notes for
  future Windows and Linux frontends.
- [Manual test checklist](docs/manual-test-checklist.md): what to check by
  hand before a release or after a UI change.
- [Security policy](SECURITY.md) and [contributing guide](CONTRIBUTING.md).

## Installing

Editage runs on Apple silicon Macs with macOS 13 or later. Each published
release has a disk image, `Editage-<version>-arm64.dmg`, built by the
[Release workflow](.github/workflows/release.yml) from the tagged commit,
with its SHA-256 checksum beside it.

The app is signed ad hoc, not with an Apple Developer ID, and it is not
notarized, so macOS cannot tell who built it. Downloading with the GitHub
command-line tool does not mark the file as downloaded from the internet, so
macOS opens the app without a Gatekeeper prompt:

```sh
gh release download --repo suyash-sneo/editage --pattern 'Editage-*-arm64.dmg*'
shasum -a 256 -c Editage-*-arm64.dmg.sha256
open Editage-*-arm64.dmg      # then drag Editage to Applications
```

A build of the Release workflow started by hand (not from a tag) is attached
to its run instead of to a release; download it with
`gh run download <run id> --repo suyash-sneo/editage --name Editage-macos-arm64`.

If you download it with a web browser instead, macOS refuses to open the app
the first time. Open System Settings › Privacy & Security and choose
**Open Anyway** for Editage, or remove the download mark yourself:
`xattr -dr com.apple.quarantine /Applications/Editage.app`.

To build the same disk image locally (it is written to `target/dist/`):

```sh
packaging/macos/build-app.sh
```

The installed app opens `.age` files from Finder (double-click, or Open With)
and declares itself the default editor for them.

## Building

Requirements:

- macOS 13 or later to run the application.
- Rust 1.85 or later (`rust-version` in `Cargo.toml`).

```sh
cargo build
cargo run -p editage-macos
```

To open files directly, pass their paths; they open as with File › Open…
(locked, asking for the password):

```sh
cargo run -p editage-macos -- path/to/notes.txt.age
```

`cargo run` starts the executable (named `Editage`) without an `.app`
bundle. It then keeps its preferences in a different place from the
installed app; see [platform notes](docs/platform-notes.md#preferences).

## Testing

```sh
cargo test --workspace
```

One test decrypts Editage's output with the reference `age` command-line tool.
It is ignored by default because it needs external tools. To run it as well:

```sh
brew install age          # expect is already part of macOS
cargo test --workspace -- --include-ignored
```

The age CLI reads passphrases only from a terminal, so the test drives it with
`expect`.

The user interface is checked by hand with the
[manual test checklist](docs/manual-test-checklist.md).

## Contributing

Contributions are welcome. Please read [CONTRIBUTING.md](CONTRIBUTING.md)
first. In short: format, lint and test before you open a pull request; if you
change how documents are stored or protected, update the tests and the
documents that describe that behaviour in the same pull request.

To report a security problem, please do not open a public issue. See
[SECURITY.md](SECURITY.md).

## License

Copyright 2026 The Editage contributors.

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
