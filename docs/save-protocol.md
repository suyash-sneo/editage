# Save protocol

This is the exact sequence of a save, as implemented by `run_save_transaction`
in [`crates/editage-core/src/save.rs`](../crates/editage-core/src/save.rs).
The tests in
[`crates/editage-core/tests/save_protocol.rs`](../crates/editage-core/tests/save_protocol.rs)
are its executable form: they run real saves against temporary files and
inject a failure at each step.

If this document and save.rs disagree, that is a bug. Update both in the same
pull request.

## Guarantee

Before the atomic replacement succeeds, the previous encrypted file is never
opened for writing, truncated, or renamed. Every failure before that point
leaves it byte-for-byte unchanged.

## Inputs

A save runs from a `SaveJob`, built on the UI thread by
`DocumentSession::begin_save` and moved to a background thread. It contains:

- the **destination path** (symbolic links already resolved at open);
- the **expectation** about the destination:
  - `UnchangedSince { fingerprint }` for a normal save (and for Save As to the
    document's own path): the file must still be exactly the version this
    application last read or wrote, identified by the SHA-256 of its complete
    contents;
  - `UserChoseDestination` for a first save or Save As to another path: the
    user chose the path in a save dialog, which already asked before replacing
    an existing file;
- the **format** (age, binary or armored; an armored file stays armored);
- a **copy of the current text** (`Plaintext`), made by the frontend just
  before the save;
- the **credential** to encrypt with;
- optionally a **passphrase check**: the document's `PassphraseVerifier` (its
  age header) and the passphrase the user typed for this save;
- the **edit generation** at the time of the snapshot, used afterwards to
  decide whether the document is clean (see
  [document-lifecycle.md](document-lifecycle.md#edit-generations)).

Each stage below is announced as it begins: it is recorded in the diagnostics
history and reported to the frontend, which shows it in the window and the
Security Inspector.

## Stages

### 1. Checking destination (`CheckingDestination`)

1. The destination's folder must exist and be a directory. If it cannot be
   read (for example the volume was disconnected), the save fails here.
2. The destination is inspected without following a final symbolic link:
   whether anything exists there, whether it is a symbolic link, its metadata
   (device, inode, size, modification time, permission bits, link count), a
   SHA-256 fingerprint of its complete contents, and whether this process may
   write it (`access(W_OK)`).
3. The result is compared with the expectation:

   | Expectation | Found | Result |
   | --- | --- | --- |
   | `UnchangedSince` | nothing | fail: `DestinationMissing` |
   | `UnchangedSince` | symbolic link | fail: `NotARegularFile` |
   | `UnchangedSince` | different fingerprint | fail: `ExternalModification` |
   | `UnchangedSince` | same fingerprint, not writable | fail: `DestinationReadOnly` |
   | `UnchangedSince` | same fingerprint, writable | continue |
   | `UserChoseDestination` | nothing | continue (new file) |
   | `UserChoseDestination` | symbolic link | fail: `NotARegularFile` |
   | `UserChoseDestination` | not writable | fail: `DestinationReadOnly` |
   | `UserChoseDestination` | writable file | continue (replace it) |

   The fingerprint is decisive. A modification time that changed while the
   contents did not (a sync client touching the file) is not treated as a
   change.
4. The **final permission bits** are chosen: the read, write and execute
   bits of the existing file (`st_mode & 0o777`; set-user-ID, set-group-ID
   and sticky bits are not carried over), or `0600` if there is none. They are
   applied in stage 7.

### 2. Confirming passphrase (`ConfirmingPassphrase`)

Only when the passphrase was typed for this save (the "ask again when saving"
policy). The typed passphrase is checked against the document's stored age
header, which costs one scrypt derivation. If it does not unlock the header,
the save fails with `PassphraseMismatch`. Without this check, a typo would
silently re-encrypt the document under a different passphrase.

A save with a retained passphrase, or with a new passphrase (first save,
Change Encryption Password), skips this stage.

### 3. Encrypting (`Encrypting`)

The text is encrypted in memory into a complete age file, with scrypt work
factor 2^18. Immediately afterwards, **the transaction's copy of the plaintext
is dropped** (requesting zeroization), before any file is created. The
SHA-256 fingerprint of the new ciphertext and a new `PassphraseVerifier`
(from the new header) are computed. Nothing has been written to disk yet.

### 4. Creating staging file (`CreatingStagingFile`)

1. A staging path is chosen in the **same folder** as the destination:
   `.<file name>.<16 random hex characters>.tmp`.
2. The file is created with `O_CREAT | O_EXCL | O_CLOEXEC` and mode `0600`
   (the umask can only make it stricter). It cannot open an existing file or
   follow a symbolic link planted at that name. It stays owner-only while it
   is written and flushed.

If creation fails, the transaction still checks whether a file appeared at
the staging path and removes it if so.

### 5. Writing encrypted data (`WritingCiphertext`)

The complete ciphertext is written to the staging file (`write_all`, which
retries short writes and interruptions) and the handle's buffer is flushed.

### 6. Flushing to disk (`Flushing`)

The staging file is flushed to storage before it can replace anything. See
[Durability](#durability) for what this means on each platform. The handle
stays open for stage 7.

### 7. Atomic replacement (`ReplacingOriginal`)

1. **The destination is checked again**, exactly as in stage 1 (including a
   full re-read and fingerprint). Encryption takes about a second, and a sync
   client may have written the file in the meantime. If the check fails, the
   staging file is removed and the save fails at this stage.
2. **The final permissions are applied** to the complete, flushed staging file
   through its still-open handle (the bits chosen in stage 1). If this fails
   (`EditorError::Permissions`), the staging file is removed, the original is
   unchanged, and the save fails at this stage. The handle is then closed.
3. **`rename(staging, destination)`.** On one filesystem this atomically
   replaces the destination's directory entry. Readers see the old complete
   file or the new complete file, never a mixture. **This is the commit
   point.**
4. **The folder is flushed** so the rename is durable. If this fails, the save
   is **not** treated as failed: the replacement has already happened and is
   visible. The failure is recorded (`DirectoryDurability::SyncFailed`) and
   shown in the Security Inspector's "Last save durability" row.
5. The destination is inspected once more to record its new identity for
   later change checks. If it cannot be read, or its fingerprint is not the
   one just written, no identity is recorded and later checks rely on the
   fingerprint alone.

A small window remains between the check in step 1 and the rename in step 3
(it includes applying the permissions and closing the handle).
POSIX has no "replace only if unchanged" operation.

### 8. Cleanup (`CleaningUp`)

If a file still exists at the staging path, it is removed. After a normal
rename there is nothing left (`CleanupResult::NothingLeft`). If removal fails,
the save is still successful; see
[Cleanup failure after a successful save](#cleanup-failure-after-a-successful-save).

### 9. Complete (`Complete`)

The result (`SaveSuccess`) carries the new fingerprint, identity and
verifier, the ciphertext size, the durability achieved, and the cleanup
result. `DocumentSession::finish_save` applies it on the UI thread.

## Failures

"Original" is the `OriginalFileState` reported to the user. "Unchanged" means
unchanged by this application.

| Stage where it fails | Cause (error) | Original | Staging file | What the user sees |
| --- | --- | --- | --- | --- |
| Checking destination | folder missing, unreadable or not a folder (`DestinationDirectoryUnavailable`) | Unchanged for a normal save; did not exist for a first save or Save As to a new location | none created | "The document could not be saved." Edits still in memory. |
| Checking destination | file gone (`DestinationMissing`) | changed by another program | none created | "… is no longer at its original location." Offers Save As. |
| Checking destination | contents changed (`ExternalModification`) | changed by another program | none created | "… changed on disk after you opened it." Offers Reload From Disk and Save As. |
| Checking destination | symbolic link at destination (`NotARegularFile`) | changed by another program (normal save) or unchanged (Save As) | none created | "The document could not be saved." |
| Checking destination | not writable (`DestinationReadOnly`) | Unchanged | none created | "… is read-only." Offers Save As. |
| Checking destination | cannot inspect (`DestinationInspect`) | Unchanged | none created | "The document could not be saved." with the system error. |
| Confirming passphrase | typed passphrase is wrong (`PassphraseMismatch`) | Unchanged or did not exist | none created | "The password you entered does not match this document's password." Offers Try Again. |
| Encrypting | encryption error (`Encryption`) | Unchanged or did not exist | none created | "The document could not be saved." |
| Creating staging file | cannot create, e.g. folder not writable (`StagingFileCreate`) | Unchanged or did not exist | removed if it appeared | "The document could not be saved." with a one-line cause, e.g. no permission to write in the folder. |
| Writing encrypted data | write error, e.g. disk full (`StagingFileWrite`) | Unchanged or did not exist | partial file removed | as above, e.g. "The disk … is full." |
| Flushing to disk | flush error (`Flush`) | Unchanged or did not exist | removed | as above |
| Atomic replacement | second check fails (`ExternalModification`, `DestinationMissing`, …) | as in stage 1 | removed | as in stage 1 |
| Atomic replacement | permissions cannot be applied (`Permissions`) | Unchanged or did not exist | removed | "The encrypted staging file was complete, but its permissions could not be set to match the original file, so the original was not replaced." |
| Atomic replacement | `rename` fails (`AtomicReplace`) | Unchanged or did not exist | removed | "Encryption completed successfully, but the encrypted staging file could not replace the existing file." |

In every row, the edits stay in the editor and the document stays modified
(state `SaveFailed`, then `UnlockedModified` when the sheet is dismissed).

If the staging file could not be removed after a failure, the message says so
and gives its path ("It contains encrypted data only"), the sheet offers to
reveal it, and the Security Inspector lists it with a Retry Cleanup action.

"Show Details" lists: the failure stage, the destination, the staging file
and whether it is still on disk, any cleanup error, whether the original is
preserved, whether the plaintext edits are still in memory, and the system
error. "Copy Details" copies the same as plain text.

## Cleanup failure after a successful save

If the rename succeeded but a file is still at the staging path and cannot
be removed (this should not happen after a normal rename; the tests simulate
it), the save **is** successful: the destination holds the new encrypted
document and the document is marked clean. The user is told "Your document
was saved successfully." followed by the staging file's path, the fact that
it contains encrypted data only, and actions to reveal it or retry cleanup.
The leftover is listed in the Security Inspector until it is removed.

## Durability

What the code asks the operating system to do, and what that means.

**macOS**

- Staging file: `fcntl(F_FULLFSYNC)`, which asks the drive to flush its own
  write cache. This is the strongest request macOS offers. On success the
  inspector reports "flushed with F_FULLFSYNC".
- If `F_FULLFSYNC` is refused (some network and external filesystems), the
  code calls `fsync(2)` directly (`libc::fsync`; Rust's `File::sync_all` would
  not do, because on Apple platforms it is itself implemented with
  `F_FULLFSYNC`). If that succeeds, the result is recorded as `FsyncOnly`
  with the reason `F_FULLFSYNC` was refused, and the inspector reports
  "flushed with fsync only (F_FULLFSYNC refused: …)". `fsync(2)` on macOS
  hands the data to the drive but does not ask it to empty its write cache.
  If `fsync(2)` also fails, the save fails at "Flushing to disk".
- Folder: the same function on a descriptor for the folder: `F_FULLFSYNC`,
  falling back to `fsync(2)`. A failure is recorded and does not fail the
  save.

**Linux** (core only; there is no Linux frontend yet)

- Staging file: `fsync(2)` (`File::sync_all`), recorded as `Fsync`.
- Folder: `fsync(2)` on the directory.
- Whether `fsync` reaches stable storage depends on the filesystem, its mount
  options and the drive. See [platform-notes.md](platform-notes.md).

**What is not guaranteed** on any platform: that the drive honours flush
requests, that a network filesystem's server does, or that a sync client
uploads the new version before the old one is lost elsewhere. After a power
loss following a failed folder flush, the previous encrypted version may
reappear. Because the staging file is flushed before the rename, the
destination should never be observed with partial contents after a crash on a
filesystem that honours these requests.

## Save coalescing

Only one save runs at a time per document. A save requested while one is
running is not started; the session records that one more save is wanted.
When the running save commits, the frontend is told to start one follow-up
save if there are edits newer than the ones just saved. If the running save
fails, the queued request is dropped and the user sees the failure. See
[document-lifecycle.md](document-lifecycle.md#save-coalescing).

## Tests

| Behaviour | Test (`tests/save_protocol.rs` unless noted) |
| --- | --- |
| stage order; no file operation before encryption | `stages_are_reported_in_order` |
| failure at each pre-commit step leaves the original intact | `original_survives_staging_file_creation_failure`, `original_survives_staging_write_failure_and_partial_staging_file_is_removed`, `original_survives_flush_failure`, `original_survives_permission_failure_before_replacement`, `original_survives_replacement_failure`, `original_survives_a_failure_before_encryption_output_exists`, `interrupted_save_before_commit_leaves_original_valid` |
| complete ciphertext exists before replacement | `complete_ciphertext_is_written_before_destination_is_replaced` |
| staging file holds ciphertext only | `staging_file_contains_ciphertext_not_plaintext_buffer` |
| external change detected at the start and before the rename | `external_modification_prevents_overwrite`, `stale_fingerprint_blocks_replacement_when_file_changes_during_the_save` |
| leftover staging file reported with its path | `staging_file_that_cannot_be_removed_after_a_failure_is_reported_with_its_path` |
| post-commit cleanup and folder-flush failures are not save failures | `cleanup_failure_does_not_report_save_as_failed`, `directory_flush_failure_after_commit_is_recorded_not_reported_as_failure` |
| read-only file and read-only folder | `read_only_destination_produces_correct_error_stage`, `read_only_folder_fails_at_staging_file_creation_with_permission_error` |
| permissions | `replaced_file_keeps_the_original_permissions`, `special_permission_bits_are_not_carried_over_to_the_replacement`, `new_documents_are_created_readable_by_the_owner_only`, `storage::tests::staging_files_are_created_owner_only` (unit test) |
| failed save keeps edits and stays modified | `failed_save_remains_dirty_and_keeps_edits_in_memory` |
| successful save | `valid_new_file_exists_after_successful_replacement`, `path_with_unicode_and_spaces_works`, `empty_file_works` |
| staging name; `O_EXCL` | `storage::tests::staging_path_is_hidden_random_and_in_the_same_folder`, `storage::tests::staging_file_creation_refuses_to_reuse_an_existing_name` (unit tests) |
| mechanism text follows the stages | `save::tests::mechanism_steps_follow_stage_order_and_mention_the_atomic_replacement` (unit test) |

Not covered by automated tests: the `F_FULLFSYNC` refusal path and the
`fsync(2)` fallback, a symbolic link appearing at the destination, and
network volumes.
