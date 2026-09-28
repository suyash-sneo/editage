# Security model

This document describes what Editage does with your document, your
passphrase and your files, as the code does it. Where the application's
control ends, it says so. If you find a difference between this document and
the code, please report it (see [SECURITY.md](../SECURITY.md)); it is treated
as a bug.

Related documents: [threat-model.md](threat-model.md) (what the design is
meant to defend against), [save-protocol.md](save-protocol.md) (the exact save
steps), [document-lifecycle.md](document-lifecycle.md) (the state machine).

## What is encrypted

The document file on disk. Each save writes a complete, standard age v1 file:
the whole text is encrypted with a new random file key, which is wrapped with
a key derived from your passphrase by scrypt. The `age` crate does all of the
cryptography. Every save produces different ciphertext, even for the same
text and passphrase.

Nothing else is encrypted, because nothing else the application stores
contains document text or passphrases. The preferences store contains
settings and, if enabled, file paths (see
[Recent files and reopening](#recent-files-and-reopen-at-launch)). File paths
and file names are not secret to Editage; choose file names accordingly.

The age format protects confidentiality and integrity of the contents. It
does not hide the file's approximate size, its name, its location, or when it
was changed.

## When plaintext exists

**Only while the document is unlocked**, in the storage of the native text
view (on macOS, the `NSTextStorage` of the `NSTextView`), together with the
text view's undo history. The Rust core does not keep a second copy (see
[architecture.md](architecture.md#where-the-plaintext-lives)). The Security
Inspector's "In memory" section shows whether plaintext is present and its
size, and whether the undo history may contain earlier versions of text.

There are also short-lived copies:

- **At unlock and reload.** Decryption produces the whole plaintext in a
  buffer that requests zeroization when dropped. The buffer is sized up front
  so it is not reallocated while decrypting, which would leave an unwiped
  partial copy behind. If decryption or authentication fails, the partial
  output is zeroized and nothing is returned. If the text is not valid UTF-8,
  the bytes are zeroized and only the offset of the first invalid byte is
  reported. The frontend copies the returned plaintext into the text view and
  drops it.
- **At save.** The frontend copies the text view's contents into a
  `Plaintext` value and hands it to the save. The save drops it (requesting
  zeroization) as soon as encryption has finished, before any file is created.
- **Document Info and Go to Line.** The macOS frontend makes a short-lived
  `Plaintext` copy of the whole text to count characters, words and lines for
  the Document Info popover (each time it is shown or refreshed), and to find
  line positions for Go to Line. These copies are dropped, requesting
  zeroization, as soon as the counts or positions are computed.

Limits: the `age` crate and the system frameworks make their own internal
copies (for example stream-cipher chunk buffers, and the string conversions
between `NSString` and Rust). Editage requests zeroization of the buffers it
controls. It cannot request it for memory owned by libraries or by the
operating system, and freed memory can be reused without being cleared.

When a document is locked or closed, the frontend removes the text from the
text view and clears the text view's undo history, and the core releases its
secret containers. This makes the text unreachable through the application.
It does not guarantee that the bytes were overwritten in physical memory.

## When the passphrase exists

The passphrase is held in `Passphrase` values, which are not `Clone`, print
as `<redacted>`, and request zeroization when dropped. How long it is kept
depends on the passphrase policy:

- **Keep until locked** (`KeepUntilLocked`, the default). After a successful
  unlock, the passphrase is kept in a secret container in application memory
  and used for saves, until the document is locked or closed.
- **Ask again when saving** (`AskAgainWhenSaving`). The passphrase is
  discarded right after unlocking. Each save asks for it again, and the typed
  passphrase is checked against the document's current age header before
  anything is written, so a typo cannot silently re-encrypt the document under
  a different passphrase.

The unlock sheet has a checkbox for this, per unlock: "Remember password until
this document is locked or closed". Its initial state comes from the
Settings › Security › Password preference. The choice applies to that
document until it is locked or closed.

**Forget Retained Passphrase** (in the File menu and the Security Inspector,
after a confirmation) releases the retained passphrase immediately and
switches that document to "ask again when saving" until it is next unlocked.

Transient duplicates are made for individual operations, because the `age`
crate takes ownership of the passphrase it uses:

- unlocking decrypts with a duplicate; the original is then retained or
  dropped according to the policy;
- a save with a retained passphrase encrypts with a duplicate;
- a save with a re-entered passphrase uses one copy to check it and one to
  encrypt;
- a save with a new passphrase (first save, or Change Encryption Password)
  encrypts with a duplicate and keeps the original until the save finishes; it
  is then retained or dropped according to the policy. If the save fails, the
  new passphrase is dropped and the previous one (if retained) stays in use.

Each duplicate requests zeroization when its operation ends. The passphrase
also passes through the password field of the frontend (an
`NSSecureTextField`) and the system's text input, which Editage does not
control. The frontend empties the field as soon as it takes the passphrase,
and `Passphrase::from_string` zeroizes the Rust `String` it receives. The
`NSString` that AppKit returned for the field's value is released normally,
not zeroized. In the new-password sheet, "Show password" copies the typed
text between a secure field and a plain text field.

## Files that are created

In the folder that contains the document, and nowhere else:

- **The document file** itself (on first save or Save As).
- **Staging files** named `.<document name>.<16 hex characters>.tmp`, for
  example `.notes.txt.age.3F09A1C2B4D5E6F7.tmp`. The leading dot hides them
  in Finder; the random part comes from the operating system's random number
  generator. A document name longer than 200 bytes is shortened in the staging
  name.

A staging file exists only during a save. It is created with
`O_CREAT | O_EXCL` (it can never open an existing file or follow a symbolic
link planted at that name) and mode `0600`, and it stays `0600` while the
ciphertext is written and flushed. Only just before the rename is it given the
permissions the final file will have (see below). It only ever receives
ciphertext: the complete encrypted document. It is then atomically renamed
over the document, which consumes it. If a save fails, the staging file is
removed. If removal fails, its path is shown to the user and in the Security
Inspector, with "Reveal" and "Retry Cleanup" actions. The inspector's
"Staging files" row also scans the document's folder for files matching the
staging name pattern (`.<name>.<16 hex digits>.tmp`), so it reports staging
files that actually exist on disk, including one left by an interrupted
session.

The staging file is in the same folder as the document because an atomic
rename only works within one filesystem. A sync client watching that folder
may see, and even upload, a staging file while it exists. It contains only
ciphertext.

The application does not intentionally write plaintext document content to
filesystem storage. Operating-system behaviour such as memory paging, crash
dumps, hibernation, or external process inspection is outside the
application's absolute control.

The application also writes its preferences (see below). It keeps no log
file, cache, autosave file, or backup copy.

## File permissions

- **Staging files** are `0600` (read and write for the owner only; the umask
  can only make this stricter) while they are written and flushed.
- **New documents** get mode `0600`, set explicitly just before the rename, so
  the umask does not affect the result.
- **Replacing an existing document** gives the new file the read, write and
  execute bits of the file it replaces (`st_mode & 0o777`). They are applied
  to the complete, flushed staging file just before the rename. Set-user-ID,
  set-group-ID and sticky bits are **not** carried over. If the permissions
  cannot be applied, the save fails at "Atomic replacement" before the rename,
  and the original file is unchanged.
- **Not preserved.** A save replaces the file by renaming a new file over it,
  so the result is a new file (a new inode). The following properties of the
  old file are not carried over: owner and group (the new file belongs to the
  user running Editage, with the group the operating system assigns), extended
  attributes (including Finder tags, comments and quarantine information),
  ACLs, file flags, and the creation date. If you rely on any of these, check
  them after saving.

## Clipboard

Copying text puts it on the system clipboard, where every process that can
read the clipboard can read it. Clearing it later shortens that exposure but
cannot undo it: clipboard managers and other programs may already have copied
it.

Editage can clear text it copied after a delay. The choices are **Never**
(the default), **after 30 seconds**, **after 60 seconds** and **after
5 minutes**.

- The application identifies its own clipboard item by the pasteboard's
  change counter (`NSPasteboard.changeCount`), recorded right after it writes.
  It never reads the clipboard's contents to decide anything.
- When the delay passes, the clipboard is cleared only if the change counter
  still has the recorded value, meaning nothing else has been copied since. If
  anything else has written to the clipboard, the application stops tracking
  its item and never clears it.
- Changing the setting applies to future copies. Switching to Never cancels a
  pending clear.
- The Security Inspector shows whether the clipboard holds text copied from
  the document, and when it will be cleared. "Clear Clipboard Now" (in the
  Edit menu and the Security Inspector) is enabled only while the clipboard
  still holds exactly that item.
- When the application quits, it clears the clipboard only if a clear delay is
  set (not Never) and the clipboard still holds exactly the item it copied.
  With Never, copied text stays on the clipboard after quitting.
- Only Copy and Cut in the editor are tracked. "Copy File Path" puts the
  file's path on the clipboard and is not tracked or cleared, because a path
  is not document content.

### Find pasteboard

The editor uses the native find bar (`NSTextFinder`). macOS has a separate
system-wide *find pasteboard* that it can use to share the current search
text between applications, so that a search started in one application
continues in another. Text you type into the find bar, and the selection when
you use Edit › Find › Use Selection for Find (⌘E), may be placed there by
AppKit, where other applications can read it. This is system behaviour that
Editage does not control and does not track or clear. Avoid searching for
secret values, or selecting them for Find, if this matters to you. When a
document is locked, its find bar is hidden. The Security Inspector states this
in its "Find text" row.

## Undo

The text view's undo history holds earlier versions of the text. It exists
only in memory, while the document is unlocked. Locking clears it; reloading
from disk clears it. The Security Inspector shows whether it may contain
plaintext.

Undoing back to the saved text still counts as a modification, because the
core keeps no copy of the saved text to compare against. See
[document-lifecycle.md](document-lifecycle.md#edit-generations).

## Crashes and unsaved edits

- There is **no autosave and no crash recovery**. Edits that have not been
  saved are lost if the application quits unexpectedly, crashes or is killed,
  or if the computer loses power.
- The application does not use `NSDocument`, so AppKit's autosave and
  versions are not involved. Window restoration is disabled: every window is
  marked non-restorable (`setRestorable(false)`), and the application writes
  `NSQuitAlwaysKeepsWindows = false` and `ApplePersistenceIgnoreState = true`
  to its own defaults at launch, so macOS does not save window state for
  relaunch.
- A crash during a save leaves the previous encrypted file intact unless the
  atomic replacement had already happened (see
  [save-protocol.md](save-protocol.md)). It may leave a staging file, which
  contains ciphertext only; the Security Inspector reports it when the
  document is opened again.
- If macOS writes a crash report or core dump, it may contain process memory.
  See the next section.

## Swap, hibernation and crash dumps

While a document is unlocked, its plaintext and possibly its passphrase are
in process memory. The operating system may write process memory to disk:
to swap (macOS encrypts swap on current versions), to a hibernation image, or
to a crash dump. Editage does not lock its memory pages and does not try to
prevent any of this. Use FileVault full-disk encryption if this matters to
you, and lock documents when you are not using them.

## Cloud sync

Editage does not sync anything and makes no claims about sync. It writes an
encrypted file to a folder. If that folder is synchronised (iCloud Drive,
OneDrive, Dropbox and so on), the sync client uploads what it finds, which is
ciphertext: the document and possibly a short-lived staging file.

- If the sync client changes the file while it is open (for example because
  it was edited on another device), Editage detects this by comparing a
  SHA-256 fingerprint of the file with the version it last read or wrote. The
  comparison is made when the document's window becomes key, when Editage
  becomes the active application, at the start of every save, and again
  immediately before the atomic replacement. Saving is refused
  and you are offered Reload From Disk or Save As. Nothing is merged.
- Files that are cloud placeholders (not downloaded; detected with the
  `SF_DATALESS` flag) are refused at open. Download them first.
- Editage cannot know whether the sync client later uploads the new version,
  keeps both versions, or creates a conflict copy. That is the sync client's
  behaviour.

## Spelling checker

When "Check Spelling While Typing" is on (the default), AppKit sends the text
to the macOS spelling service, a separate system process on the same Mac,
through local inter-process communication. The Security Inspector shows
whether spelling checking is on for the unlocked document. Turn it off with
Edit › Spelling and Grammar › Check Spelling While Typing if you do not want
the text to leave the application's process. Grammar checking is off.

The following are disabled in the editor: automatic quote and dash
substitution, text replacement, automatic spelling correction, automatic link
and data detection, smart insert and delete, and automatic text completion.
Silent changes would corrupt passwords and other exact text. Writing Tools
(macOS 15 and later) are also disabled, because they can send text to other
processes or services for processing. On older macOS versions, where Writing
Tools do not exist, the setting is skipped.

## Filesystem assumptions

The save protocol assumes:

- `rename(2)` within one filesystem atomically replaces the destination
  directory entry: readers see either the old complete file or the new
  complete file. This holds for local APFS and HFS+ volumes. On network
  volumes it depends on the server; the Security Inspector says so when it
  detects one.
- On macOS, `fcntl(F_FULLFSYNC)` asks the drive to flush its write cache. The
  staging file is flushed this way before it replaces anything. If a
  filesystem refuses `F_FULLFSYNC`, the code falls back to plain `fsync(2)`
  and records the refusal, which the Security Inspector shows in "Last save
  durability". See [save-protocol.md](save-protocol.md#durability).
- After the rename, the folder is flushed so the new directory entry is
  durable. If that fails, the save is still reported as successful (the
  replacement has happened) and the failure is recorded in the Security
  Inspector's "Last save durability" row.
- There is a small window between the final fingerprint check and the rename.
  A program that writes the file in that window can have its write replaced.
  POSIX offers no "rename only if unchanged" operation. The window is kept as
  short as the code allows: the check is the last thing before the rename.

## Special files and volumes

- **Symbolic links.** If you open a symbolic link, Editage tells you, shows
  the resolved target, and asks you to confirm before unlocking. Saves go to
  the resolved target, so the link keeps working. If the destination itself is
  a symbolic link at save time (for example it was replaced by one after
  opening, or you chose one in Save As), the save is refused.
- **Hard links.** If the file has more than one hard link, Editage warns and
  asks you to confirm. Saving replaces only the directory entry you opened;
  the other links keep pointing to the previous encrypted version.
- **Read-only files and read-only volumes.** The document opens and can be
  read, but saving in place is refused: the window subtitle says "Read-only",
  Save shows "This document is read-only." and suggests Save As, and the
  Security Inspector says why. An informational notice ("This document is
  read-only.", OK only) is shown at open. Saving is also refused inside the
  save transaction if the file is not writable
  (checked with `access(W_OK)`), because replacing a file you marked
  read-only would silently defeat that choice. Save As to another location is
  still possible.
- **Network volumes** (`smbfs`, `nfs`, `afpfs`, `webdav`, `cifs`, `ftp`, as
  reported by `statfs`). The document opens normally; the Security Inspector
  shows a "Network volume" row saying that atomic replacement and flushing
  depend on the server. An informational notice ("This document is on a
  network volume.", OK only) is shown at open.
- **Cloud placeholders.** Refused at open until downloaded.
- **Disconnected volumes.** A save fails at the first stage, "Checking
  destination", because the folder is no longer available. Nothing is
  written; your edits stay in memory.

## Key derivation

When Editage encrypts, it uses scrypt with a fixed work factor of **2^18**
(`ENCRYPTION_WORK_FACTOR`). This is the value the reference age
implementation uses. The `age` crate would otherwise calibrate to about one
second of work on the current machine, which would make the result depend on
the machine and the build (an unoptimised debug build calibrates much lower).
A fixed value is predictable and can be stated in the Security Inspector.

When Editage decrypts, it accepts work factors up to **2^22**
(`MAXIMUM_ACCEPTED_WORK_FACTOR`), the reference implementation's limit. A
file that asks for more is refused before any key derivation. This bounds the
memory (up to about 4 GiB at 2^22) and time a malicious file can demand,
while opening any file a standard age tool produces. Files with a lower work
factor are accepted; when saved, they are re-encrypted at 2^18.

The automated tests use a work factor of 2^10 so they run quickly. That value
is only compiled in with the `test-support` feature, and `lib.rs` refuses to
compile that feature into builds without debug assertions.

Your passphrase is the only thing protecting the file. scrypt makes each
guess expensive; it does not make a weak passphrase strong.

## Size limits

- Files larger than **10 MB** (10,000,000 bytes) of ciphertext open only
  after a confirmation, because the whole text is decrypted into one text view.
- Continuous spell checking is paused for documents larger than **1 MB** of
  text: the system spelling service re-checks the whole buffer and would stall
  the editor for minutes. The Security Inspector says when it is paused.
- Files larger than **500 MB** are refused. Decrypting needs the whole
  ciphertext and the whole plaintext in memory at the same time, and a single
  native text view with hundreds of megabytes of text is impractical.

## Text encoding

Documents must be UTF-8. A file that decrypts to anything else is not opened:
the decrypted bytes are zeroized, the file on disk is not changed, and the
offset of the first invalid byte is shown. Editage never replaces invalid
bytes, because saving would then silently change the document. Documents are
always saved as UTF-8, without a byte-order mark added or removed.

## Recent files and reopen at launch

Preferences are stored in the platform's settings store (on macOS,
`NSUserDefaults`; see [platform-notes.md](platform-notes.md)). They never
contain document text, passphrases, or clipboard contents. The keys are listed
in `preferences::keys::ALL`.

- **Recent documents** (on by default): the paths of up to 10 recently opened
  or saved documents, shown in File › Open Recent (all of them) and the
  welcome window (the six most recent).
  Turning the setting off (Settings › General, or the checkbox in the welcome
  window) clears the list immediately and stops recording. File › Open Recent ›
  Clear Menu clears the list without turning recording off. macOS, Finder or a
  sync application may keep their own records of files you open.
- **Reopen documents at launch** (off by default): the paths of open
  documents. The list is updated whenever a document is opened or closed (not
  only at quit), so it survives a crash or forced quit. Documents are
  reopened **locked**; each asks for its passphrase again. Turning the setting
  off clears the list.

Settings shows a plain description of what is stored
(`Preferences::stored_metadata_description`) and where. The same defaults
domain also holds window positions (frame autosave) and the two restoration
settings described under [Crashes and unsaved edits](#crashes-and-unsaved-edits).

## Diagnostics

The diagnostics window shows a history of operational events: open, unlock,
each save stage, staging file paths, lock, clipboard clears, and failures.

- It is kept **in memory only** and never written to disk. It is lost when
  the application quits. It can be copied as text or cleared from Window ›
  Diagnostics.
- It is bounded to the **500** most recent events.
- Events are typed (`DiagnosticEvent`). No variant has a field for document
  text, passphrases, clipboard contents, selections or search strings. The
  recorded data are file names and paths, byte counts, save stages, and, for
  failures, the text of the structured error (`EditorError`'s `Display`),
  which contains paths and system or library error descriptions.

## What this application does not protect against

Editage is not designed to defend against a fully compromised running
machine. In particular, it does not protect against:

- a compromised operating system, or malware running as your user or as an
  administrator;
- programs that read the memory of other processes, or debuggers attached to
  Editage;
- keyloggers and malicious input methods, which see the passphrase and the
  text as you type them;
- screen capture and screen recording, including by other applications you
  have allowed to record the screen;
- other processes reading the clipboard or the system find pasteboard, and
  clipboard managers keeping copies of what you copied;
- physical attacks on memory (for example cold-boot attacks) or on an
  unlocked, unattended computer;
- plaintext that you copy elsewhere yourself: pasted into another
  application, printed, exported, or captured in a screenshot;
- weak or reused passphrases;
- someone who can modify the Editage executable or its dependencies on your
  computer.

## Non-goals

- Password management, secret sharing, multiple users or access control.
- Syncing, merging, or version history. Use your sync software or a version
  control system on the encrypted files if you need them.
- Hiding that an encrypted document exists, its size, its name, or when it
  changed.
- Plausible deniability, hidden volumes, or duress passphrases.
- Protecting the document while it is unlocked from software running on the
  same computer.
- Formats other than age v1 with a passphrase, in this version.
