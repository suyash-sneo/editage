# Architecture

Editage has two parts:

- **`crates/editage-core`**: a portable Rust library. It decides everything
  that matters for the safety of a document: the file format, encryption and
  decryption, the document state machine, the save transaction, clipboard
  policy, preferences, and the wording of every report the user sees about
  sensitive state. It contains no user interface code.
- **`apps/macos`** (package `editage-macos`, binary `Editage`): a native
  AppKit frontend. It owns windows, menus, sheets and the native text view. It
  asks the core what to do and what to say, and renders the answers.

The rule for the split is: if a decision could affect what is written to
disk, what stays in memory, or what the user is told about either, it belongs
in the core, where it can be tested without a user interface.

## Where the plaintext lives

While a document is unlocked, its text lives in the frontend's native text
view (on macOS, the `NSTextStorage` behind an `NSTextView`). It does not also
live in Rust.

A second full copy in Rust would double the amount of plaintext in memory and
would have to be kept in step with every keystroke, input-method composition
and undo operation. So the core does not hold the text. `DocumentSession`
records only *that* plaintext is present and its length in bytes
(`PlaintextPresence::InEditor { bytes }`), which the Security Inspector
reports.

The core handles plaintext at two moments only:

- **Unlock and reload.** Decryption returns a `Plaintext` value. The frontend
  copies it into the text view and drops it. `Plaintext` requests zeroization
  of its buffer when dropped.
- **Save.** The frontend copies the text view's current text into a
  `Plaintext` and passes it to `DocumentSession::begin_save`. The save
  transaction drops it immediately after encryption, before any file is
  touched.

The macOS frontend also takes short-lived `Plaintext` copies for the Document
Info counts and for Go to Line; they are dropped as soon as they are used.

Memory that belongs to AppKit (the text storage, undo stack, layout caches,
and any copies Foundation makes while converting strings) is not under the
core's control. Locking asks the frontend to remove the text and clear the
undo history. It cannot request zeroization of memory the system frameworks
own. See [security-model.md](security-model.md).

## Threads and the job pattern

Decryption and encryption involve scrypt, which takes about a second at the
default work factor. File I/O can block on slow or network volumes. None of
this may run on the UI thread, and the document's state must not be changed
from two threads at once.

The core therefore splits each slow operation into three steps:

1. **`begin_*`, on the UI thread.** A method on `DocumentSession` checks that
   the transition is allowed, moves the session into its in-progress state,
   and returns a job value that owns everything the work needs.
2. **`run`, on a background thread.** The job does the slow work. It does not
   touch the session.
3. **`finish_*`, on the UI thread.** The frontend passes the job's result back.
   The session applies it and returns what the frontend should show.

| Job | Created by | Run with | Finished by |
| --- | --- | --- | --- |
| `UnlockJob` | `begin_unlock`, `begin_reload` | `UnlockJob::run()` | `finish_unlock`, `finish_reload` |
| `SaveJob` | `begin_save` | `run_save_transaction(job, &storage, &mut report_stage)` | `finish_save` |
| `ExternalCheckJob` | `begin_external_check` | `ExternalCheckJob::run(&storage)` | `finish_external_check` |

Because only `begin_*` and `finish_*` touch the session, and both run on the
UI thread, the session is only ever mutated on one thread. It needs no locks.
The only shared state is `DiagnosticLog`, an `Arc<Mutex<…>>` that jobs append
to from background threads.

While a save runs, `run_save_transaction` calls `report_stage` as each stage
begins. The frontend forwards each stage to the UI thread and calls
`DocumentSession::record_save_stage`, so the window and the inspector can say
what is happening.

`open_encrypted_document`, which reads the file and parses the age header
before any passphrase is requested, is also blocking and is called off the UI
thread. It returns a new, locked session.

In the macOS frontend, `background::run_in_background` runs the job on a
Grand Central Dispatch global queue and sends the result back to the main
queue. Completion closures capture only plain data (such as a `DocumentId`)
and look the window up again on the main thread. If the window was closed in
the meantime, the result is simply dropped; an unlock result's plaintext is
zeroized as it is dropped.

## The macOS frontend

`apps/macos/src/` is organised by window and by concern: `app.rs` (the
application controller and delegate, the one-second timer for clipboard
clearing, auto-lock and inspector refresh), `document_window.rs` (every user
flow on a document), `editor_view.rs` (the `NSTextView` subclass),
`password_sheet.rs`, `sheets.rs`, `security_inspector.rs`,
`settings_window.rs`, `menu.rs`, `clipboard.rs`, `preferences_store.rs`,
`background.rs`, and small helpers in `controls.rs`.

AppKit controls send their actions to target objects. `controls::ActionTarget`
is an Objective-C object that forwards an action to a Rust closure. Controls
and delegates reference their targets weakly, so each owner keeps its targets
in a `TargetBag`. When a sheet closes or a view is rebuilt, the bag and the
sheet's panel are **released on the next main-queue turn, never immediately**
(`TargetBag::clear`, `Sheet::close`, `app::defer_on_main_thread`). Often the
code that closes a sheet or rebuilds a view is running inside one of those
targets' own actions, and releasing an object AppKit is still messaging would
be a use-after-free.

## Modules

### `secrets`

The types that carry secret material: `Passphrase`, `Credential`
(passphrase-only in this version) and `Plaintext`.

- None of them implements `Clone`. `Passphrase::duplicate_for_operation` is
  the only way to copy one, so every copy is visible where it is made.
- Their `Debug` output is redacted (`Passphrase(<redacted>)`,
  `Plaintext(<redacted: 17 bytes>)`).
- They request zeroization when dropped (`secrecy::SecretString`,
  `zeroize::Zeroizing`). `Passphrase::from_string` also zeroizes the `String`
  it is given.

The module's own documentation states the limit: these types request
zeroization of memory they own. They cannot guarantee that no other copy has
ever existed in the process.

### `crypto` (with `crypto::age_format`)

The only module that knows which encryption formats exist.

- `detect_encryption_format` recognises age (binary or armored) by its magic
  bytes, parses the header, and checks that it is protected by a passphrase.
  It needs no secret and derives no keys. It names OpenPGP data when it sees
  it, so the user is told what the file is rather than "unknown".
- `decrypt_document` returns either the complete, authenticated, UTF-8
  plaintext or an error. It never returns partial plaintext.
- `encrypt_document` encrypts a complete document in memory and returns a
  complete file.
- `PassphraseVerifier` holds a copy of the file's age header (non-secret) so
  that a passphrase typed at save time can be checked against the document
  before anything is written.

`age_format` does the age-specific work through the `age` crate. It fixes the
scrypt work factor for encryption at 2^18 and caps the accepted work factor
for decryption at 2^22 (see [security-model.md](security-model.md)). No
cryptographic primitive is implemented in this repository.

### `storage`

Reading encrypted files, and the narrow filesystem interface the save
transaction uses.

- `read_encrypted_file` resolves symbolic links, refuses non-regular files,
  cloud placeholders and files over 512 MiB, reads the ciphertext, and records
  the file's identity (device, inode, size, modification time, mode, link
  count), a SHA-256 fingerprint of the ciphertext, and facts about the volume.
  It returns `OpenNotice`s for things the user should know before editing:
  symbolic links, hard links, large files, read-only files or volumes, and
  network volumes.
- `StorageBackend` is a trait with one method per operation at which a save
  can fail: inspect the destination, create the staging file, write, flush,
  apply the final permissions, replace, flush the folder, remove the staging
  file. `FileSystemStorage` is
  the real implementation. `FaultInjectingStorage` (tests only) fails at a
  chosen operation or changes the destination between two operations.
- `staging_path_for` chooses the staging file name.

Only Unix-family systems are implemented (`std::os::unix`, `libc`). The macOS
parts (`F_FULLFSYNC` with an `fsync(2)` fallback, `statfs`, `SF_DATALESS`) are behind
`cfg(target_os = "macos")`.

### `save`

The save transaction, `run_save_transaction`, and its types: `SaveStage`,
`SaveJob`, `SaveSuccess`, `SaveFailure`, `OriginalFileState`,
`StagingFileReport`, `CleanupResult`. [save-protocol.md](save-protocol.md)
describes it step by step.

`SaveStage::mechanism_step` holds one sentence per stage. The Security
Inspector's "How saving works" list is generated from it, so the explanation
shown to users follows the stage list in the code.

### `document`

`DocumentSession`, the per-document state machine and the single source of
truth for a document's security-relevant state: locked or unlocked, whether a
passphrase is retained, whether a save is running, which file on disk it is
bound to and which version of it was last read or written. Frontends drive it
through named transitions and keep no parallel flags of their own. The jobs
described above live here too. See [document-lifecycle.md](document-lifecycle.md).

### `security_state`

`inspect_document_state` builds the Security Inspector's content
(`SecurityReport`) from a `DocumentSession` plus a few application-wide facts
(preferences, clipboard status, the name of the spelling service). Every row
is computed from the same state that controls behaviour. There are no
separate presentation flags and no fixed reassuring text.

### `presentation`

The one place where structured errors and notices become sentences:
`save_failure_report`, `cleanup_warning_report`, `unlock_failure_message`,
`open_failure_report`, `open_notice_text`, `lock_reason_text`,
`external_change_report`. Each returns plain data (`FailureReport`,
`NoticeText`) with a short message, detail rows for "Show Details", and the
actions to offer. Frontends render these as they are, so the wording, and
whether it is honest, is shared and reviewed in one place.

### `clipboard`

`ClipboardTracker` decides when to clear text this application copied. It
identifies "our" clipboard item by the pasteboard's change counter, never by
reading the clipboard's contents. The frontend performs the pasteboard
operations. See [security-model.md](security-model.md#clipboard).

### `preferences`

`Preferences` and their conversion to and from plain string pairs, so every
frontend stores the same, easily inspected keys (`preferences::keys::ALL`).
The only document-related entries are file paths: recent documents and
documents to reopen at launch.

### `diagnostics`

`DiagnosticLog`, a bounded in-memory history (500 events) of typed
`DiagnosticEvent`s, shown in the diagnostics window and copyable as text. It
is never written to disk.

### `error`

`EditorError`, the structured error type used everywhere. Variants carry the
path involved and the original `io::Error` or `age::DecryptError` as the
source. No variant has a field for plaintext or a passphrase.

## Adding an encryption method (for example OpenPGP)

The format is isolated in `crypto` so that a new method does not change the
save protocol, the state machine or the frontends. To add one:

1. Add a variant to `EncryptionFormat` in `crypto/mod.rs`, fill in its
   `display_name`, `protection_description`, `key_derivation_description`,
   `independent_decrypt_command` and `required_credential`, and add its
   detection rule to `detect_encryption_format`. (Detection currently
   recognises OpenPGP only to refuse it with a clear message.)
2. If it needs a different kind of secret (a private key, an age identity
   file), add a variant to `Credential` and `CredentialKind` in `secrets.rs`,
   with a redacted `Debug` implementation and a `duplicate_for_operation`
   branch.
3. Add a sibling module to `age_format` (for example `crypto/openpgp.rs`) with
   the decrypt, encrypt and verifier functions, and dispatch to it from the
   `match` statements in `crypto/mod.rs` (`decrypt_document`,
   `encrypt_document`, `PassphraseVerifier`).
4. Add tests, including interoperability fixtures made by the reference tool
   for that format, and update [security-model.md](security-model.md).

Nothing else should need to change. The save transaction treats the output of
`encrypt_document` as opaque ciphertext, and the state machine only asks the
format which credential it requires. Frontends that show a passphrase prompt
will need a prompt for the new credential kind.

## Adding a frontend (Windows, GTK)

A new frontend reuses all of `editage-core`. It needs to:

- Implement the job pattern above with its platform's background execution
  and a way to return to the UI thread.
- Keep the text in the platform's native text control, and follow the
  two-phase lock (`begin_lock`, clear the text and undo history, `finish_lock`).
- Render `SecurityReport`, `FailureReport` and `NoticeText` as they are, in
  the platform's usual controls. Do not rewrite their wording in the
  frontend; change it in the core so every frontend stays consistent.
- Store preferences with `Preferences::to_entries` / `from_entries` in the
  platform's settings store.
- Perform clipboard operations and report the platform's change counter (or
  equivalent) to `ClipboardTracker`.

For Windows, `storage.rs` must first gain a Windows implementation: it uses
`std::os::unix` and `libc` today and does not compile elsewhere. See
[platform-notes.md](platform-notes.md). For Linux, the core compiles and its
tests run in CI, but the durability behaviour differs from macOS (also in
platform notes).

Cross-compiling a GUI binary does not show that it works on that operating
system. A frontend is supported when it is built, tested and checked by hand
on that system.

## Dependencies

### `editage-core`

| Crate | Why |
| --- | --- |
| `age` 0.11 | The age v1 format and all of its cryptography. Default features are disabled; only `armor` is enabled, for ASCII-armored files. The `plugin` and `ssh` features are not enabled, so no plugin binaries are launched and SSH keys are not supported. The crate still pulls in its own non-optional dependencies, including X25519 support (`x25519-dalek`, `curve25519-dalek`, `bech32`), `futures` and its localisation stack (`i18n-embed`, `fluent`, `rust-embed`). |
| `secrecy` 0.10 | `SecretString`, which the `age` crate's passphrase API takes, with redacted `Debug` and zeroization on drop. |
| `zeroize` 1 | Requests zeroization of plaintext buffers (`Zeroizing<String>`, `Zeroizing<Vec<u8>>`) when they are dropped. |
| `sha2` 0.10 | SHA-256 fingerprints of the ciphertext, used to detect changes made by other programs. Already a dependency of `age`. |
| `getrandom` 0.2 | Random part of staging file names. Already a dependency of `age`'s dependencies. |
| `thiserror` 2 | Derives `Display` and `Error` for `EditorError`, keeping the error type declarative. |
| `libc` 0.2 | The few system calls the standard library does not expose, or does not expose as needed: `fcntl(F_FULLFSYNC)`, `fsync` (Rust's `File::sync_all` uses `F_FULLFSYNC` on Apple platforms), `access(W_OK)`, `statfs`, and errno constants for error messages. |
| `tempfile` 3 (dev only) | Temporary folders for tests that use real files. Not linked into the application. |

### `editage-macos`

| Crate | Why |
| --- | --- |
| `editage-core` | Everything above. |
| `objc2` 0.6 | Calling Objective-C from Rust: messages, classes, reference counting. |
| `objc2-foundation` 0.3 | Typed bindings for Foundation (`NSString`, `NSUserDefaults`, and so on). |
| `objc2-app-kit` 0.3 | Typed bindings for AppKit (`NSApplication`, `NSWindow`, `NSTextView`, `NSPasteboard`, panels and sheets). |
| `block2` 0.6 | Objective-C blocks, used as completion handlers for sheets and panels. |
| `dispatch2` 0.3 | Grand Central Dispatch, to run jobs on a background queue and return results to the main queue. |
| `objc2-uniform-type-identifiers` 0.3 | `UTType`, to restrict the Open and Save panels to `.age` files. |

These are only compiled on macOS (`[target.'cfg(target_os = "macos")'.dependencies]`).

The full dependency graph can be inspected with
`cargo tree -p editage-core -e normal`. Licences, advisories, sources and
banned crates are checked by `cargo deny check` with [`deny.toml`](../deny.toml).
