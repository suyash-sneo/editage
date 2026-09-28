# Document lifecycle

Every open document is a `DocumentSession` in
[`crates/editage-core/src/document.rs`](../crates/editage-core/src/document.rs).
The session is the single source of truth for the document's
security-relevant state. The frontend drives it through named transitions and
renders what it reports. It keeps no parallel flags of its own.

## States

| State | Meaning |
| --- | --- |
| `Locked` | Encrypted on disk. No plaintext held; no passphrase retained. |
| `Unlocking` | Decryption is running in the background. |
| `UnlockedClean` | Plaintext is in the editor and matches the last save. |
| `UnlockedModified` | Plaintext is in the editor and differs from the last save, or has never been saved. |
| `Saving(stage)` | A save transaction is running, currently at `stage`. Editing continues during a save. |
| `SaveFailed` | The last save failed. The edits are still in the editor. |
| `Locking` | The frontend is clearing its editor; the plaintext may still be there. |
| `Closed` | The session has ended. |

A transition that is not allowed from the current state returns
`EditorError::InvalidTransition` and changes nothing.

## Transitions

```
  open_encrypted_document
            |
            v
     +-------------+   begin_unlock   +-------------+
     |   Locked    |----------------->|  Unlocking  |
     |             |<-----------------|             |
     +-------------+  finish_unlock   +-------------+
            ^           (failure)            |
            | finish_lock                    | finish_unlock (success)
            |                                v
     +-------------+    begin_lock    +-----------------+
     |   Locking   |<-----------------|  UnlockedClean  |<---- new_untitled
     +-------------+  (also from      +-----------------+
                       Modified and     |            ^
                       SaveFailed)      | record_edit| finish_save: committed,
                                        v            | no edits during the save
                          +------------------+       |
                          | UnlockedModified |       |
                          +------------------+       |
                             |            ^          |
                  begin_save |            | finish_save: committed,
                  (also from |            | edits made during the save
                  Clean and  |            |          |
                  SaveFailed)v            |          |
                          +------------------+       |
                          |  Saving(stage)   |-------+
                          +------------------+
                             |     record_save_stage moves to the next stage
                finish_save: |
                      failed v
                          +------------------+  acknowledge_save_failure
                          |    SaveFailed    |--------------------------> UnlockedModified
                          +------------------+

  Not drawn:
    finish_reload (success)  UnlockedClean / UnlockedModified / SaveFailed -> UnlockedClean
    close                    any state except Saving -> Closed
```

In words:

- `open_encrypted_document` reads the file, recognises the format and checks
  that it is passphrase-protected, then returns a session in `Locked`. It
  does not decrypt. The bytes it read are kept (as ciphertext) for the first
  unlock, so the bytes that were validated are the bytes decrypted.
- `begin_unlock(credential, policy)` → `Unlocking`. `finish_unlock` →
  `UnlockedClean` with the plaintext for the editor, or back to `Locked` with
  an error (for example a wrong passphrase; the user can try again).
- `record_edit` → `UnlockedModified` (from `UnlockedClean`). Edits are also
  accepted, without a state change, in `UnlockedModified`, `Saving` and
  `SaveFailed`. They are refused in every other state.
- `begin_save` → `Saving(CheckingDestination)`, from `UnlockedClean`,
  `UnlockedModified` or `SaveFailed`. `record_save_stage` updates the stage as
  the transaction reports it. `finish_save` → `UnlockedClean` or
  `UnlockedModified` if it committed (see below), `SaveFailed` if not.
- `acknowledge_save_failure` (the user dismissed the failure sheet):
  `SaveFailed` → `UnlockedModified`. The failure stays recorded for the
  Security Inspector.
- `begin_lock` → `Locking`; `finish_lock` → `Locked` (see
  [Locking](#locking)).
- `close` → `Closed`. It releases the retained passphrase and the plaintext
  state, and discards any ciphertext kept from open.
- `begin_reload` / `finish_reload` do not pass through a separate state (see
  [Revert and reload](#revert-to-saved-and-reload-from-disk)).

After locking, unlocking again reads the file from disk afresh, so a version
written by another device while the document was locked is the one that is
decrypted.

## Edit generations

The session does not keep a copy of the saved text. Instead it counts edits.

- Every `record_edit` increments `edit_generation`.
- `begin_save` records the generation at the moment the frontend took its
  snapshot of the text.
- When the save commits, `saved_generation` becomes that recorded value. The
  document is clean only if no edit was recorded since: `edit_generation ==
  saved_generation`.

Consequences:

- **Edits made during a save keep the document modified.** The save wrote
  the snapshot, not the newer text.
- **Undoing back to the saved text still counts as modified.** Undo is an
  edit like any other, and the session has no saved copy to compare with.
  Keeping one would mean a second full copy of the plaintext in memory.
  Saving again makes the document clean.

## Save coalescing

Only one save runs at a time. If `begin_save` is called while a save is
running, it returns `SaveStart::QueuedBehindRunningSave` and records that one
more save is wanted. The text copy and credential passed with that call are
dropped (requesting zeroization) at once; they are not kept for later.

When the running save commits, `SaveCompletion::follow_up_save_requested` is
true if a save was queued **and** there are edits newer than the ones just
saved. The frontend then starts one more save with the current text. However
many saves were requested in the meantime, at most one follow-up runs. If the
running save fails, the queued request is dropped.

The queue records only that a save is wanted, not its target or credential.
A Save As or Change Encryption Password requested while a save is running is
therefore not remembered as such; frontends should not offer those actions
while a save is running.

In the macOS frontend, Save, Save As… and Change Encryption Password… are
disabled while a save is running, so the queue is rarely reached. When a
follow-up save is requested, the frontend starts it only if a passphrase is
retained; otherwise it does not ask for the password again, and the window
simply stays marked "— Edited" until the user saves.

## Passphrase retention

See [security-model.md](security-model.md#when-the-passphrase-exists) for the
two policies. In state terms:

- `finish_unlock` retains the credential if the policy chosen for this unlock
  is `KeepUntilLocked`, and drops it if it is `AskAgainWhenSaving`.
- `save_credential_need` tells the frontend what to ask for:
  `AskForNewPassphrase` for a never-saved document, `UseRetained` if a
  passphrase is retained, otherwise `AskForCurrentPassphrase`.
- `forget_retained_passphrase` drops it and switches the policy to
  `AskAgainWhenSaving` until the document is next unlocked.
- `finish_lock` and `close` drop it.

## Locking

Locking has two phases, because the plaintext is in the frontend's text view,
not in the session.

1. **`begin_lock(decision, reason)`.** Allowed when `lock_readiness()` is
   `Ready`, or `HasUnsavedChanges` with the decision
   `DiscardUnsavedChanges`. Not allowed while a save is running, for a document
   that has never been saved (there would be nothing to unlock), or when the
   document is already locked. The session moves to `Locking` and stops
   treating the document as unlocked.
2. **The frontend clears its editor**: it removes the text from the text view
   and clears the text view's undo history.
3. **`finish_lock(EditorCleared { undo_history_cleared })`.** The session
   drops the retained passphrase and any pending new passphrase, records that
   no plaintext is present, marks the undo history cleared (if the frontend
   says it was), and moves to `Locked`. The lock reason (`LockedByUser`, or
   `Inactivity { minutes }`) is shown on the unlock sheet.

The frontend asks the user before locking a document with unsaved changes
("Do you want to save the changes made to “…” before locking it?", with Save,
Cancel and Don't Save). The session never saves or discards on its own. If
Lock Document is chosen while a save is running, the macOS frontend locks
after the save commits, provided no unsaved changes remain.

The unlock sheet then explains the lock (`lock_reason_text`): "Locked. The
decrypted text was removed from the editor, its undo history was cleared, and
any retained password was released." (or "Locked after N minutes of
inactivity. …"). The macOS frontend also hides the find bar when it locks.

### Auto-lock

`auto_lock_decision(idle, lock_after)` is called periodically by the
frontend. It returns:

- `NotApplicable` if auto-lock is off, the document is already locked, or it
  has never been saved;
- `NotDue` if the idle time is shorter than the setting;
- `LockNow` if it is due and there are no unsaved changes;
- `PostponedUnsavedChanges` if it is due but there are unsaved changes;
- `PostponedSaveInProgress` if it is due but a save is running.

In the macOS frontend, a one-second timer asks each document for this
decision. Inactivity means no key presses, mouse clicks or scrolling in
Editage itself (pointer movement alone may not count; see
[platform-notes.md](platform-notes.md#timer-inactivity-and-quitting)).
Activity in other applications does not count, so a document can lock while
you work elsewhere. If a sheet is open on the window when auto-lock is due,
locking waits and is tried again on the next tick.

Auto-lock **never saves and never discards edits**. When it is postponed
because of unsaved changes, the postponement is recorded in the diagnostics
and shown in the Security Inspector ("Postponed: unsaved changes"), and the
document stays unlocked until the user saves or discards.

A document that has never been saved cannot be locked, manually or
automatically, because there is no encrypted file to unlock. Its text stays
in the editor until it is saved or closed. The Security Inspector shows
"Auto-lock: Not applicable until the document is saved".

## Revert to Saved and Reload From Disk

Both use `begin_reload(credential)` and `finish_reload(outcome)`. They are
allowed from `UnlockedClean`, `UnlockedModified` and `SaveFailed`, not while
a save is running.

- The file is read from disk again and decrypted in full, with the retained
  passphrase or one the user types.
- On success, the frontend replaces the editor's text with the result and
  clears the undo history. The document becomes `UnlockedClean`, bound to the
  version just read, and any recorded external change is cleared.
- On failure, the current edits stay as they are and the state does not
  change.

Reload From Disk is how the user resolves an external change: the conflict
sheet offers it next to Save As. Reloading discards the unsaved edits in the
editor; the sheet says so. In the macOS frontend, File › Revert to Saved is
enabled only when there are unsaved changes, and both paths ask for
confirmation first ("Replace the text in this window with the version of “…”
on disk?") when there are unsaved changes. If no passphrase is retained, the
password is asked for before the file is decrypted again.

## New documents

1. `DocumentSession::new_untitled` creates a document in `UnlockedClean` with
   an empty text and no file (`FileBinding::NotYetSaved`). Its text exists
   only in memory; the window subtitle says "Not saved yet — exists only in
   memory". Nothing is written until the first save.
2. On the first save, `save_credential_need` returns `AskForNewPassphrase`.
   The macOS frontend shows the Save dialog (suggested name "Untitled.txt",
   which the panel shows as "Untitled.age"; ".age" is appended if missing, and an existing file under the appended name
   is replaced only after confirmation), then the sheet "Create a password for
   “…”" with Password and Confirm Password fields and an "Encrypt and Save"
   button.
3. `begin_save(SaveTarget::ChosenPath(path), SaveCredential::New(credential), text)`.
   The transaction runs with `UserChoseDestination`: it encrypts, creates a
   staging file with mode `0600` beside the chosen path, writes and flushes
   it, and renames it into place.
4. On commit, the session is bound to the new file and takes its name. The new
   passphrase is retained or dropped according to the policy.

If the first save fails, no file is created (the staging file is removed),
the document stays unbound and modified, and the new passphrase is dropped.

## Save As

`begin_save(SaveTarget::ChosenPath(path), …)` with a path other than the
document's own. The copy keeps the document's current passphrase, so the
credential is the retained one or the current one re-entered and checked
(`SaveCredential::Retained` or `ReenteredCurrent`). The format and encoding
are kept.

The session is **rebound to the new path only after the save commits**. If
Save As fails, the document stays bound to its original file, which is
untouched. After a successful Save As, the original file stays as it was on
disk, and later saves go to the new file.

In the macOS frontend, "Try Again" on a save-failure sheet repeats exactly the
operation that failed: a failed Save As is retried to the same chosen path
(never silently as a save over the original), and a failed password change
asks for the new password again.

Save As to the document's own path is treated as a normal save, with the
external-change check.

## Change Encryption Password

`begin_save(SaveTarget::CurrentFile, SaveCredential::New(new_credential), text)`.
This is a complete re-encryption of the current text under the new
passphrase, through the normal save transaction, with the same checks and the
same atomic replacement. There is no separate "rekey" path.

- If it commits, the file is encrypted under the new passphrase, and the new
  passphrase replaces the old one in memory if the policy retains passphrases.
- If it fails, the new passphrase is dropped. The file on disk is still
  encrypted under the previous passphrase, and a previously retained
  passphrase stays in use.

The core does not itself require the current passphrase before a change,
and the macOS frontend does not ask for it: the sheet "Change the password for
“…”" asks only for the new password twice, with a "Re-encrypt and Save"
button. Anyone with access to the unlocked window can therefore change the
password. They can already read and copy the text. The menu item is disabled
for never-saved and read-only documents and while a save is running.

## External changes

`begin_external_check` / `ExternalCheckJob::run` / `finish_external_check`
compare the file on disk with the version this application last read or
wrote. The macOS frontend runs the check, for an unlocked document with no
save running, when its window becomes key and, for the document being
inspected, when Editage becomes the active application. While a change is
known, Save shows the conflict sheet instead of starting a save. If the file
is no longer at its path, the sheet says so and offers Save As only.

- If device, inode, size and modification time are all unchanged, the
  contents are assumed unchanged without re-reading them. Otherwise the file
  is read and its SHA-256 compared. (The save transaction itself always
  compares the full fingerprint.)
- A result for an older version is ignored, for example when a save
  committed while the check was running.
- Once a change is known, a later check that finds no change does not hide
  it; the user must reload or use Save As.

## Window status

The macOS window subtitle is derived from the session on every change:
"Locked", "Decrypting…", "Saving…", "Not saved — the last save failed",
"Changed on disk by another program", "File no longer on disk at this path",
"Read-only", "Saved locally at <time>" (after a save in this session), "Read
from disk at <time>" (after a reload that followed this session's last
save), "Not saved yet — exists only in memory", or nothing (unlocked, not yet
saved in this session). The title shows "— Edited" while there are unsaved changes.

## Tests

`crates/editage-core/tests/document_lifecycle.rs` covers these transitions,
including `opening_a_document_leaves_it_locked_until_a_passphrase_is_given`,
`wrong_passphrase_leaves_the_document_locked_and_allows_retrying`,
`edits_made_while_saving_keep_the_document_modified`,
`a_second_save_request_during_a_save_is_queued_not_run_concurrently`,
`lock_clears_document_session_state`,
`locking_with_unsaved_changes_requires_an_explicit_decision`,
`invalid_state_transitions_are_rejected`,
`a_never_saved_document_cannot_be_locked_or_saved_without_a_location`,
`auto_lock_is_postponed_while_there_are_unsaved_changes`,
`reload_from_disk_replaces_text_and_marks_clean`,
`changing_the_password_re_encrypts_the_whole_document` and
`failed_password_change_keeps_the_previous_retained_passphrase`. Save As
rebinding is covered by
`save_as_creates_a_separate_valid_encrypted_file_and_only_then_rebinds` in
`tests/integration_and_interop.rs`.
