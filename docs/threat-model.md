# Threat model

This document says what Editage is designed to protect, against what, and how
each protection is checked. [security-model.md](security-model.md) describes
the resulting behaviour in full.

## Primary concern

**Protect the contents of a text file while it is stored on disk or
synchronised through a storage provider you do not trust.**

The attacker in this scenario can read, copy, keep old versions of, delete,
or modify the encrypted file: a cloud storage provider, someone with access to
a backup, a stolen or lost disk that is not otherwise encrypted, or another
user of a shared folder. They do not control the computer on which you edit
the document while it is unlocked.

## Secondary concerns

Ways the application itself could weaken that protection or lose your data:

1. accidental plaintext temporary files;
2. accidental plaintext logs;
3. accidental overwrite or damage of the encrypted file;
4. stale cloud-sync versions overwriting newer ones, or being overwritten;
5. forgotten plaintext buffers and passphrases in memory;
6. clipboard exposure.

## Explicitly out of scope

Editage is **not** designed to defend against a fully compromised running
machine: malware, a compromised operating system, programs reading process
memory, keyloggers, screen capture, or physical access to an unlocked
computer. While a document is unlocked, its plaintext is in memory by design,
so that it can be edited. The complete list is in
[security-model.md](security-model.md#what-this-application-does-not-protect-against).

## Concerns, mitigations and tests

Test names refer to `crates/editage-core/tests/*.rs` unless they are marked
as unit tests, which live next to the code in `crates/editage-core/src/`.

### Confidentiality and integrity at rest (primary)

Mitigations:

- The file is a standard age v1 file with passphrase (scrypt) protection. The
  `age` crate performs all of the cryptography; Editage implements none.
- scrypt work factor 2^18 when encrypting, the reference implementation's
  value.
- A damaged, truncated or tampered file fails authentication. No partial
  plaintext is returned.
- Each save uses a new random file key, so saving the same text twice gives
  unrelated ciphertext.
- Files are interoperable with the reference implementation, so the format's
  public analysis applies and the user can always decrypt without Editage.
- Files that demand more than 2^22 scrypt work are refused before any key
  derivation, bounding the memory and time a malicious file can consume.

Tests:

- `file_encrypted_by_the_reference_age_cli_opens`
- `armored_file_from_the_reference_cli_opens_and_stays_armored_when_saved`
- `unicode_fixture_from_the_reference_cli_opens`
- `application_output_decrypts_with_the_reference_age_cli` (ignored by
  default; needs the `age` CLI and `expect`; runs in CI)
- `save_produces_decryptable_age_file_and_reopening_preserves_edits`
- `recipient_encrypted_age_files_are_explained_not_misreported`
- unit tests in `crypto`: `wrong_passphrase_fails_with_authentication_failure`,
  `corrupted_payload_fails_without_returning_plaintext`,
  `truncated_payload_fails`, `corrupted_header_is_reported_as_invalid_age_file`,
  `encrypted_output_differs_across_saves_of_the_same_text`,
  `plaintext_round_trips_through_age`,
  `armored_documents_round_trip_and_are_detected_as_armored`

Not tested automatically: rejection of a file above the 2^22 work-factor cap
(no such fixture exists yet).

### 1. Accidental plaintext temporary files

Mitigations:

- The text is encrypted in memory before any file is created. The only file
  a save creates is the staging file, and it receives only ciphertext.
- The transaction drops its copy of the plaintext immediately after
  encryption.
- A new document exists only in memory until its first save, which writes an
  encrypted file directly.
- No autosave, no crash-recovery files, no `NSDocument` autosave or
  versions. Window restoration is disabled (windows are non-restorable, and
  `NSQuitAlwaysKeepsWindows = false` and `ApplePersistenceIgnoreState = true`
  are written to the application's defaults).

Tests:

- `staging_file_contains_ciphertext_not_plaintext_buffer`
- `staging_file_that_cannot_be_removed_after_a_failure_is_reported_with_its_path`
  (also checks that the leftover holds ciphertext only)
- `first_save_of_a_new_document_writes_no_intermediate_plaintext_file`
- `stages_are_reported_in_order` (no file stage precedes encryption)
- `original_survives_staging_write_failure_and_partial_staging_file_is_removed`

Not tested automatically: the absence of AppKit autosave and restoration
files (see the [manual test checklist](manual-test-checklist.md)).

### 2. Accidental plaintext logs

Mitigations:

- No log file. The diagnostics history is in memory only, bounded to 500
  events, and made of typed events that have no field for document text,
  passphrases, clipboard contents, selections or search strings.
- Secret-bearing types (`Passphrase`, `Credential`, `Plaintext`,
  `PassphraseState`, `SaveJob`, `UnlockJob`, `DocumentSession`) have manual,
  redacted `Debug` implementations; none derives `Debug`.
- Errors carry paths and system errors, never contents.
- The workspace lints warn on `dbg!` and `print!`/`println!`.

Tests:

- `diagnostics_never_contain_plaintext_or_passphrase`
- `debug_formatting_a_session_does_not_dump_the_passphrase`
- unit tests in `secrets`: `debug_output_of_passphrase_does_not_contain_the_passphrase`,
  `debug_output_of_plaintext_shows_only_its_length`,
  `debug_output_of_credential_is_redacted`
- unit test in `diagnostics`: `history_is_bounded`

### 3. Accidental overwrite or damage

Mitigations:

- The staged save: encrypt in memory, write a new staging file beside the
  document, flush it, then atomically rename it over the original. Before the
  rename, the original is never opened for writing, truncated or renamed.
- The staging file is created with `O_EXCL`, so it never reuses an existing
  file.
- A save refuses to replace a read-only file or a symbolic link.
- The staging file stays owner-only (`0600`) until it is complete and
  flushed; the original's permission bits (without set-user-ID, set-group-ID
  or sticky bits) are applied only just before the rename.
- With "ask again when saving", a typed passphrase is checked against the
  document before anything is written, so a typo cannot silently re-encrypt
  the document under a different passphrase.
- A failed save keeps the edits in the editor and the document marked
  modified. A failed Save As or password change keeps the previous binding and
  passphrase.
- Failures after the commit point (folder flush, cleanup) are recorded and
  shown, but are not reported as a failed save, so the user is not led to
  retry a save that already happened.

Tests:

- `original_survives_staging_file_creation_failure`
- `original_survives_staging_write_failure_and_partial_staging_file_is_removed`
- `original_survives_flush_failure`
- `original_survives_permission_failure_before_replacement`
- `original_survives_replacement_failure`
- `original_survives_a_failure_before_encryption_output_exists`
- `interrupted_save_before_commit_leaves_original_valid`
- `original_ciphertext_remains_valid_after_injected_failure`
- `complete_ciphertext_is_written_before_destination_is_replaced`
- `valid_new_file_exists_after_successful_replacement`
- `read_only_destination_produces_correct_error_stage`
- `read_only_folder_fails_at_staging_file_creation_with_permission_error`
- `failed_save_remains_dirty_and_keeps_edits_in_memory`
- `cleanup_failure_does_not_report_save_as_failed`
- `directory_flush_failure_after_commit_is_recorded_not_reported_as_failure`
- `save_as_creates_a_separate_valid_encrypted_file_and_only_then_rebinds`
- `failed_password_change_keeps_the_previous_retained_passphrase`
- `replaced_file_keeps_the_original_permissions`
- `special_permission_bits_are_not_carried_over_to_the_replacement`
- `new_documents_are_created_readable_by_the_owner_only`
- `non_utf8_contents_are_reported_without_replacing_bytes_and_source_is_untouched`
- unit tests: `staging_file_creation_refuses_to_reuse_an_existing_name` and
  `staging_files_are_created_owner_only` (`storage`), `verifier_accepts_the_document_passphrase_and_rejects_others`
  (`crypto`)

Not tested automatically: a symbolic link appearing at the destination
between open and save; the `F_FULLFSYNC` refusal path and its `fsync(2)`
fallback.

Residual risk: a write by another program in the short window between the
final fingerprint check and the rename can be replaced. See
[save-protocol.md](save-protocol.md#7-atomic-replacement-replacingoriginal).

### 4. Stale cloud-sync versions

Mitigations:

- The document remembers the SHA-256 fingerprint of the version it last read
  or wrote. Every save compares the file on disk with it twice: at the start,
  and again immediately before the rename. A mismatch blocks the save; the
  user chooses Reload From Disk or Save As. Nothing is merged or silently
  overwritten.
- The file is also checked when the window becomes active, so the conflict is
  usually shown before the user tries to save.
- Unlocking after a lock reads the file from disk afresh.
- Cloud placeholders are refused at open rather than opened as a stale or
  empty file.

Tests:

- `external_modification_prevents_overwrite`
- `external_modification_is_caught`
- `stale_fingerprint_blocks_replacement_when_file_changes_during_the_save`
- `external_change_check_detects_a_changed_file`
- `reload_from_disk_replaces_text_and_marks_clean`
- `locked_document_can_be_unlocked_again_reading_the_current_file`

Not tested automatically: cloud placeholder detection (it needs a File
Provider volume).

### 5. Forgotten plaintext buffers and passphrases

Mitigations:

- The plaintext lives only in the native text view; the core keeps no second
  copy.
- Locking is two-phase: the frontend removes the text and clears the undo
  history, then the session releases the retained passphrase.
- Buffers the core controls request zeroization when dropped. Decryption
  pre-sizes its buffer to avoid a reallocation, which would leave an unwiped
  partial copy behind.
- The passphrase is kept only under the "keep until locked" policy, can be
  forgotten on demand, and is shown as retained or not in the Security
  Inspector.
- Auto-lock after inactivity (15 minutes by default; inactivity means no
  keyboard, mouse or scroll events in Editage). It is postponed, never forced,
  when there are unsaved changes, and the postponement is shown.
- Closing releases everything the session holds.

Tests:

- `lock_clears_document_session_state`
- `locking_with_unsaved_changes_requires_an_explicit_decision`
- `closing_releases_retained_secret_container`
- `closing_with_unsaved_changes_requires_discard_decision`
- `retained_policy_preserves_access_until_lock`
- `prompt_every_save_policy_does_not_retain_passphrase_after_unlock`
- `forgetting_the_retained_passphrase_requires_it_for_the_next_save`
- `auto_lock_is_postponed_while_there_are_unsaved_changes`
- `wrong_passphrase_leaves_the_document_locked_and_allows_retrying`
- `opening_a_document_leaves_it_locked_until_a_passphrase_is_given`

Limits: no test can show that memory was actually overwritten, and memory
owned by AppKit, the `age` crate or the allocator is outside the core's
control. The macOS frontend makes short-lived, zeroize-on-drop copies of the
whole text for saving, for the Document Info counts and for Go to Line. A never-saved document cannot be locked, so its text stays in memory
until it is saved or closed.

### 6. Clipboard exposure

Mitigations:

- Optional clearing of copied text after 30 seconds, 60 seconds or 5 minutes
  (default: never).
- The application's own clipboard item is identified by the pasteboard change
  counter, never by reading clipboard contents, and is cleared only if it is
  still exactly that item. Content copied by another application is never
  cleared.
- The Security Inspector shows whether the clipboard holds text from the
  document and when it will be cleared, and offers Clear Clipboard Now.

Tests (unit tests in `clipboard`):

- `never_policy_never_clears`
- `clears_only_after_the_deadline_while_our_item_is_still_there`
- `content_written_by_another_application_is_never_cleared`
- `switching_to_never_cancels_a_pending_clear`
- `policy_labels`

Additional frontend behaviour: at quit, the clipboard is cleared only if a
clear delay is set and it still holds exactly the application's item.

Limits: other processes and clipboard managers can read the clipboard as soon
as text is copied; clearing later does not undo that. The pasteboard
operations themselves are in the frontend and are checked by hand (see the
[manual test checklist](manual-test-checklist.md)).

**Find pasteboard (not mitigated).** The editor uses the native find bar
(`NSTextFinder`). macOS may place the find bar's search text, and the
selection used with Use Selection for Find (⌘E), on the system-wide find
pasteboard, which other applications can read. This is outside the
application's control; Editage does not track or clear it. It is documented in
[security-model.md](security-model.md#find-pasteboard).
