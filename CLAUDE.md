# CLAUDE.md

Guidance for AI coding agents working in this repository.

## Product

Editage is a native macOS text editor for passphrase-protected age v1 files.
A portable Rust core (`crates/editage-core`) decides everything about a
document; an AppKit frontend (`apps/macos`, objc2) renders it.

Core principle: **security through transparency.** The user must be able to
see what is in memory, what is on disk, and what failed and where. Nothing the
UI says about sensitive state may be hard-coded: it must come from
`DocumentSession`, `security_state.rs` or `presentation.rs`. Never overclaim:
write "requests zeroization", not "securely wiped"; "not intentionally
written to disk", not "can never reach disk".

## Layout

Core (`crates/editage-core/src/`):
- `secrets.rs`: `Passphrase`, `Credential`, `Plaintext`; not `Clone`, redacted `Debug`, zeroize on drop.
- `crypto/mod.rs`: `EncryptionFormat`, format detection, dispatch; `crypto/age_format.rs`: age via the `age` crate.
- `storage.rs`: reading encrypted files, `StorageBackend` (one method per failure point), staging names, volume facts.
- `save.rs`: `run_save_transaction` and `SaveStage` (incl. `mechanism_step`).
- `document.rs`: `DocumentSession` state machine and the jobs (`UnlockJob`, `SaveJob`, `ExternalCheckJob`).
- `security_state.rs`: `SecurityReport` for the Security Inspector, derived from the session.
- `presentation.rs`: the only place errors and notices become sentences (`FailureReport`, `NoticeText`).
- `clipboard.rs`: `ClipboardTracker` (change-counter based). `preferences.rs`: settings and keys. `diagnostics.rs`: bounded in-memory event log. `error.rs`: `EditorError`.

Frontend (`apps/macos/src/`): `app.rs` (controller, delegate, 1 s timer, quit),
`document_window.rs` (all document flows), `editor_view.rs` (`NSTextView`
subclass), `password_sheet.rs`, `sheets.rs`, `info_popover.rs` (ⓘ, ⌘I),
`security_inspector.rs` (⌥⌘I), `welcome_window.rs`, `settings_window.rs`,
`menu.rs` (items and enabling rules), `toolbar.rs`, `background.rs`
(`run_in_background`), `controls.rs` (`ActionTarget`, `TargetBag`, helpers).

Docs, and when to update them (same change as the code):
- `docs/security-model.md`: anything about what is stored, kept in memory, or protected.
- `docs/save-protocol.md`: any change to the save transaction.
- `docs/document-lifecycle.md`: states, transitions, locking, reload, external changes, window status.
- `docs/threat-model.md`: new mitigations or security tests (list test names).
- `docs/architecture.md`: module structure, job pattern, dependencies (with a "why").
- `docs/platform-notes.md`: macOS specifics, known platform gaps, notes for Windows/Linux.
- `docs/manual-test-checklist.md`: any user-visible UI change.
- `README.md`, `CONTRIBUTING.md`, `SECURITY.md`: usage, contribution rules, reporting.

## Commands

```sh
cargo build
cargo run -p editage-macos -- [file.age ...]      # binary `Editage`, unbundled
cargo test --workspace -- --include-ignored        # needs `brew install age` and `expect`
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
tests/fixtures/generate-fixtures.sh                # regenerate interop fixtures (needs age, age-keygen, expect)
```

Preferences live in the `Editage` defaults domain (`defaults read Editage`).
CI runs on every push and pull request: macOS (fmt, clippy, all tests, release
build), Linux (core tests only) and cargo-deny.

## Invariants and rules

- Never write plaintext to disk: no temp files, autosave, caches or logs. A save writes only ciphertext to a staging file.
- Never derive or print `Debug`/`Display` of secrets or plaintext; secret-bearing types get manual redacted `Debug` plus a test. Never log text. `DiagnosticEvent`s are typed and carry no document text, passphrases, clipboard contents, selections or search strings; free-text fields come only from `EditorError`'s `Display`.
- The plaintext lives in the native text view only. Rust copies are short-lived `Plaintext` values (unlock/reload result, save snapshot, Info counts, Go to Line), dropped as soon as used. Copy secrets only via `duplicate_for_operation`.
- The save stages in `save.rs`, `docs/save-protocol.md` and `SaveStage::mechanism_step` must change together.
- Keep errors structured (`EditorError`) until `presentation.rs`. Do not format errors into strings deeper in the core.
- The state machine lives in `document.rs`. The frontend drives named transitions and reads state from the session; no parallel UI booleans.
- Slow work (scrypt, file I/O) uses the job pattern: `begin_*` on the main thread returns a job, the job runs via `run_in_background`, `finish_*` on the main thread. Mutate sessions only on the main thread. Completion closures capture plain data (`DocumentId`) and look the window up again.
- Never release an AppKit object inside its own action. Use `TargetBag::clear`, `Sheet::close` or `defer_on_main_thread`, and never rebuild a view synchronously from its own control's action.
- `unsafe` only in the platform layer (the AppKit frontend and the `libc` calls in `storage.rs`), in small blocks, each with a `// SAFETY:` comment.
- No network access, telemetry or networking dependencies; `deny.toml` bans common networking crates.
- Every new dependency needs a "why" row in `docs/architecture.md`, and `cargo deny check` must pass.
- scrypt work factor: always 2^18 when encrypting (`ENCRYPTION_WORK_FACTOR`); decryption accepts at most 2^22 (`MAXIMUM_ACCEPTED_WORK_FACTOR`).
- The `test-support` feature lowers the work factor for tests only; `lib.rs` refuses it without debug assertions. Never enable it elsewhere.
- Every security bug gets a permanent regression test with a descriptive name that fails without the fix.
- Tests read like specifications: `snake_case` sentences stating the behaviour (e.g. `original_survives_flush_failure`).

## Extending

- **Encryption method:** add an `EncryptionFormat` variant (display name, descriptions, `required_credential`, detection in `detect_encryption_format`); add `Credential`/`CredentialKind` variants if it needs a new secret (redacted `Debug`, `duplicate_for_operation` branch); add a sibling module to `crypto/age_format.rs` and dispatch from `crypto/mod.rs`; add interop fixtures and tests; update `docs/security-model.md`. The save protocol and state machine should not change.
- **Frontend:** reuse the whole core; implement the job pattern and the two-phase lock (`begin_lock`, clear text and undo, `finish_lock`); render `SecurityReport`, `FailureReport` and `NoticeText` as they are. Windows first needs a Windows `StorageBackend` in `storage.rs` (see `docs/platform-notes.md`).
- **User-visible string:** if it describes security state, a failure or a notice, add it to `presentation.rs` or `security_state.rs`, computed from real state, not to the frontend.

## Product decisions in force

- The UI matches the design reference: the claude.ai design project "Encrypted Text Editor".
- Two transparency surfaces: the ⓘ Info popover (⌘I) and the Security Inspector (⌥⌘I).
- Auto-lock is postponed, never forced, while there are unsaved changes; it never saves or discards.
- Reopen at launch is off by default, stores paths only, and reopens documents locked.
- Passphrase retention is a per-unlock checkbox; its default is set in Settings › Security.
- Clipboard auto-clear defaults to Never; only our own item (by change count) is ever cleared.
- Symbolic links need confirmation at open; saves go to the resolved target.
- External-change checks run before and during every save, when a window becomes key, and when the app becomes active.
- Revert to Saved re-reads and re-decrypts the file from disk.
- Spell checking is paused above 1 MB of text; opening warns above 10 MB; files above 500 MB are refused.

## Workflow

- Commit directly to `master`. End commit messages with:
  `Co-Authored-By: Claude Opus 5.5 (1M context) <noreply@anthropic.com>`
- GUI testing: script the running app via System Events/`osascript` only when the user has said the machine is free, because keystrokes go to whichever app is frontmost. Always bring Editage to the front before sending input, and follow `docs/manual-test-checklist.md`.
