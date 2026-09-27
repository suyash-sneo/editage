# Manual test checklist

The AppKit frontend has no automated tests. GUI scripting is deliberately not
used, because it sends keystrokes and clicks to the tester's live desktop. Go
through this list by hand before a release, and the relevant sections after
any UI change. Use dummy content only. In the pull request, say which items
you checked, on which macOS version, and on which kind of volume.

Setup: build and run the app, and create a test file with the reference CLI:

```sh
printf 'Dummy entry\nusername: example\npassword: example\n' > /tmp/dummy.txt
age --passphrase --output ~/Desktop/test.txt.age /tmp/dummy.txt
rm /tmp/dummy.txt
cargo run -p editage-macos -- ~/Desktop/test.txt.age
```

## Launch

- [ ] Launching with no arguments and no documents to reopen shows the welcome
      window with "Open…", "New Document", the recent documents list, and a
      "Remember recent documents" checkbox.
- [ ] Launching with a path argument opens that file locked, with the unlock
      sheet, and no welcome window.
- [ ] Opening a file that is already open brings its window to the front.
- [ ] File › Open… only allows `.age` files ("Only .age files can be opened.").

## Opening and unlocking

- [ ] The window shows the sheet "Unlock “test.txt.age”" ("Enter the password
      used to encrypt this file."). No text is visible behind it; the subtitle
      says "Locked". Unlock stays disabled until something is typed.
- [ ] A wrong password keeps the document locked and shows "The password is
      incorrect, or this file could not be decrypted." Show Details explains
      why this is uncertain. The password field is empty and can be used again.
- [ ] The correct password shows "Decrypting…" briefly, then the editor with
      the file's text.
- [ ] The checkbox "Remember password until this document is locked or
      closed" starts in the state chosen in Settings › Security › Password.
      With it off, the Security Inspector's Passphrase row says "Not retained
      after unlock. You will be asked again when saving." and the next ⌘S asks
      for the password ("Enter the password for “…”").
- [ ] Cancel on the unlock sheet closes the window.
- [ ] A file encrypted to an age recipient (public key) is refused with an
      explanation that only passphrase-protected files are supported.
- [ ] A non-age file is refused ("It is not an age encrypted file. Detected:
      …").
- [ ] A file that decrypts to non-UTF-8 data is refused with "The file was
      decrypted successfully, but its contents are not valid UTF-8 text."; the
      details show the offset; the file is unchanged.

## Editing and saving

- [ ] Typing changes the title to "test.txt.age — Edited".
- [ ] ⌘S saves without a dialog; "— Edited" disappears and the subtitle shows
      "Saved locally at <time>".
- [ ] `age --decrypt ~/Desktop/test.txt.age` in Terminal prints the saved text.
- [ ] After saving, `ls -la ~/Desktop` shows no `.test.txt.age.*.tmp` file.
- [ ] Undoing back to the saved text still shows "— Edited" (expected; see
      docs/document-lifecycle.md).
- [ ] Closing a window with unsaved changes asks "Do you want to save the
      changes made to “…”?" with Save, Cancel and Don't Save. Quitting with
      unsaved changes asks the same for each such document.
- [ ] With the password not remembered, a mistyped password at save time is
      rejected with "The password you entered does not match this document's
      password." and the file is unchanged.
- [ ] New document (⌘N): the subtitle says "Not saved yet — exists only in
      memory". The first ⌘S shows the Save dialog (suggested name
      "Untitled.txt"; ".age" is added), then "Create a password for “…”" with
      Password and Confirm Password. Mismatched entries show "The passwords do
      not match."; fewer than 10 characters shows the short-password hint.
      "Encrypt and Save" creates a file that `age --decrypt` reads, with mode
      `-rw-------` (`ls -l`).
- [ ] Save As… (⇧⌘S) to a new name writes a new file; the window then shows
      the new name; the original file is unchanged. Saving to a name without
      ".age" whose ".age" version exists asks before replacing it.
- [ ] File › Change Encryption Password… shows "Change the password for “…”";
      after "Re-encrypt and Save" the file decrypts only with the new password
      (check with `age --decrypt`).
- [ ] An armored file (`age --armor`) stays armored after saving
      (`head -1` shows `-----BEGIN AGE ENCRYPTED FILE-----`).
- [ ] Saving a file with mode `0644` keeps `0644`; with `4644` (set-user-ID)
      the result is `0644`.

## Locking

- [ ] File › Lock Document (⌃⌘L), or the Lock toolbar button, removes the text,
      hides the find bar, sets the subtitle to "Locked", and shows the unlock
      sheet with "Locked. The decrypted text was removed from the editor, its
      undo history was cleared, and any retained password was released."
- [ ] Locking with unsaved changes asks "Do you want to save the changes made
      to “…” before locking it?"
- [ ] After locking and unlocking, ⌘Z does not bring back earlier text.
- [ ] While locked, the Security Inspector shows "No active editor buffer" and
      the passphrase as "Not held. It is asked for only to unlock."
- [ ] Lock Document is disabled for a new, never-saved document.
- [ ] Auto-lock: set Settings › Security › Lock after inactivity to 5 minutes
      and leave Editage untouched (activity in other apps does not count).
      With no unsaved changes, the document locks and the sheet says "Locked
      after 5 minutes of inactivity. …". With unsaved changes, it stays
      unlocked and the inspector's Auto-lock row says "Postponed: unsaved
      changes (after 5 minutes of inactivity)". Nothing is saved or discarded.
- [ ] File › Forget Retained Passphrase asks for confirmation ("Forget
      Passphrase"); afterwards the next save asks for the password.

## External changes and failures

- [ ] While the document is unlocked, replace the file from Terminal with a new
      encryption of other text, using the same password
      (`age --passphrase --output /tmp/newer.age other.txt` then
      `mv /tmp/newer.age ~/Desktop/test.txt.age`). Switching back to the window
      shows "“test.txt.age” changed on disk while it was open." with Reload
      From Disk, Save As… and Cancel. The subtitle says "Changed on disk by
      another program". ⌘S shows the same conflict and does not write.
- [ ] Reload From Disk asks "Replace the text in this window with the version
      of “…” on disk?" when there are edits, then shows the new contents and
      clears "— Edited".
- [ ] File › Revert to Saved is enabled only when there are unsaved changes.
- [ ] Put a test file in its own folder, open and unlock it, run `chmod 500` on
      the folder, then edit and save. The sheet says "The document could not
      be saved.", that the edits are still in memory, and that you don't have
      permission to write in the folder. Show Details lists Failure stage
      "Creating staging file", Destination, Encrypted staging file, Original
      preserved, Plaintext edits still in memory, and System error. Copy
      Details copies the same text. The subtitle says "Not saved — the last
      save failed". Restore with `chmod 700`.
- [ ] Make a file read-only (`chmod 400`) and open it. Before the unlock
      sheet, a notice "This document is read-only." (OK) appears; the subtitle says "Read-only"; the inspector's
      Writable row says "No — the file is read-only; saving is disabled"; ⌘S
      shows "This document is read-only." and suggests Save As…
- [ ] Open a document from a disk image, then eject the image and save: the
      save fails at "Checking destination" and the edits stay in memory.

## Special files

- [ ] Opening a symbolic link to an age file asks for confirmation ("This
      document was opened through a symbolic link.") and shows the resolved
      target. Saving updates the target; the link still works.
- [ ] Opening a file with two hard links warns that the other links keep the
      previous encrypted version.
- [ ] Opening a file larger than 10 MiB asks for confirmation first.
- [ ] A cloud placeholder (iCloud Drive or OneDrive file set to online-only)
      is refused with a request to download it first.
- [ ] A file on a network share opens without a notice; the Security
      Inspector shows a "Network volume" row saying that atomic replacement and
      flushing depend on the server.

## Security Inspector (File › Document Security…, ⌥⌘I)

- [ ] Values change live: typing updates the plaintext size and "Modified";
      saving updates "Last successful encrypted save" and "Last save
      durability" ("Staging file flushed with F_FULLFSYNC; folder flushed after
      replacement" on a local disk); locking updates the In memory rows;
      Forget Retained Passphrase updates the Passphrase row.
- [ ] "How saving works ▸" expands to the numbered steps, including "the
      staging file is given the original file's permissions", and ends with
      the statement about plaintext temporary files.
- [ ] The Spelling checker row matches Edit › Spelling and Grammar › Check
      Spelling While Typing (on by default).
- [ ] "Save staging directory" is the document's folder.
- [ ] The actions work: Reveal Encrypted File in Finder, Copy File Path, Lock
      Document, Forget Retained Passphrase, Clear Clipboard Now, View
      Diagnostics.

## Clipboard

- [ ] Set Settings › Security › Clear clipboard to "After 30 seconds". Copy
      text from the document; the inspector says when it will be cleared;
      after about 30 seconds the clipboard is empty.
- [ ] Copy from the document, then copy something in another application
      before the delay passes. The other application's content is **not**
      cleared.
- [ ] With "Never", copied text stays on the clipboard, including after
      quitting.
- [ ] With a delay set, quitting while the document's text is still on the
      clipboard clears it; quitting after another app copied something leaves
      that content alone.
- [ ] Edit › Clear Clipboard Now is enabled only while the clipboard still
      holds the document's text.

## Native editing and appearance

- [ ] Input methods: type Japanese or Chinese with an input method; marked
      text and candidate selection work.
- [ ] Unicode: emoji, combining characters and right-to-left text display,
      edit and save correctly.
- [ ] Undo and redo work across several edits.
- [ ] Edit › Find: Find… (⌘F) and Find and Replace… (⌥⌘F) show the native find
      bar; Find Next (⌘G), Find Previous (⇧⌘G) and Use Selection for Find (⌘E)
      work.
- [ ] View › Go to Line… (⌘L) selects the requested line; View › Document Info
      (⌘I) shows character, word and line counts.
- [ ] The context menu appears and offers the usual text actions.
- [ ] Automatic changes are off: type `"password" -- test` and check it is
      unchanged; misspelled words are underlined but not corrected. On
      macOS 15 and later, Writing Tools are not offered.
- [ ] VoiceOver reads the text, the unlock sheet and the failure sheets.
- [ ] Light and dark mode, including switching while the app is running.
- [ ] Format › Bigger / Smaller, Monospaced Font and Wrap Lines apply to open
      documents; View › Show Toolbar toggles the toolbar.

## Quit, relaunch, recent documents and preferences

- [ ] With Settings › General › "Reopen documents that were open" on, quit
      with documents open and relaunch: they reopen **locked**.
- [ ] With it off (the default), nothing is reopened, and window positions are
      the only trace of the previous session.
- [ ] Turning off "Show files in Open Recent" (or "Remember recent documents"
      in the welcome window) empties File › Open Recent. File › Open Recent ›
      Clear Menu empties it without turning recording off.
- [ ] `defaults read Editage` shows only settings, file paths, window frames,
      and `NSQuitAlwaysKeepsWindows` / `ApplePersistenceIgnoreState`; never
      document text or passwords.
- [ ] After quitting, `ls -la` of the documents' folders shows no staging
      files and no other files created by the application.
- [ ] Window › Diagnostics lists events without document text or passwords;
      Copy and Clear work; nothing persists after relaunch.
