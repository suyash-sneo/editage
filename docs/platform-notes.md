# Platform notes

Editage runs on macOS today. The core (`editage-core`) also compiles and is
tested on Linux, but there is no Linux or Windows frontend.

Do not pretend that cross-compiling a GUI binary proves it works on that
operating system. A platform is supported when the application has been
built, tested and checked by hand on it.

## macOS

Minimum version: macOS 13.

### Files and durability

- **Flushing.** The staging file is flushed with `fcntl(F_FULLFSYNC)`, which
  asks the drive to flush its write cache. Plain `fsync(2)` on macOS only
  hands the data to the drive, which may keep it in a volatile cache. If
  `F_FULLFSYNC` is refused (some network and external filesystems), the code
  calls `fsync(2)` directly and records the refusal. It cannot use Rust's
  `File::sync_all` for this, because on Apple platforms that is itself
  implemented with `F_FULLFSYNC`. The folder is flushed the same way after the
  rename. See [save-protocol.md](save-protocol.md#durability).
- **Replacement.** `rename(2)` within one volume atomically replaces the
  destination directory entry. The staging file is always created in the
  destination's folder, so the rename never crosses volumes (a cross-volume
  rename would fail with `EXDEV`, and the message says so).
- **Network volumes** are detected with `statfs(2)`: the filesystem type names
  `smbfs`, `nfs`, `afpfs`, `webdav`, `cifs` and `ftp` are treated as network
  filesystems; the Security Inspector then shows a "Network volume" row (no
  notice is shown at open). Read-only volumes are detected
  from the `MNT_RDONLY` mount flag.
- **Cloud placeholders.** Files managed by a File Provider (iCloud Drive,
  OneDrive Files On-Demand and others) whose contents are not downloaded carry
  the `SF_DATALESS` flag in `st_flags`. Editage refuses to open them rather
  than trigger a download that could block for a long time, and asks the user
  to download the file first (for example with "Download Now" in Finder). The
  check is made at open only.
- **Writability** is checked with `access(2)` and `W_OK`.

### Preferences

Preferences are stored with `NSUserDefaults`, using the keys in
`editage_core::preferences::keys`. The application is currently run as an
unbundled executable (there is no `.app` bundle yet), so it has no bundle
identifier, and the defaults domain is the executable name: **`Editage`**,
stored in `~/Library/Preferences/Editage.plist`. You can inspect it with:

```sh
defaults read Editage
```

When the application is packaged as a bundle, the domain will become the
bundle identifier and the file name will change accordingly. Existing
preferences would then need to be migrated or will be reset.

Other state that macOS keeps for the application:

- **Open panel location.** `NSOpenPanel` and `NSSavePanel` remember the last
  folder used. macOS stores this itself, in its own preferences for the
  application; Editage does not control it.
- **Window frames.** Window sizes and positions are saved in the same
  defaults domain under the names `EditageDocumentWindow` (saved when a
  document window closes), `EditageSecurityInspector` and
  `EditageDiagnostics`. They contain no document content.
- **No `NSDocument`.** Editage does not use the `NSDocument` architecture, so
  AppKit's autosave, versions browser and document-based restoration are not
  involved.
- **Restoration disabled.** Every window is created with
  `setRestorable(false)`, and at launch the application writes
  `NSQuitAlwaysKeepsWindows = false` and `ApplePersistenceIgnoreState = true`
  to its own defaults domain, so macOS does not save window state for the next
  launch. Documents are reopened at launch only if the user enables "Reopen
  documents that were open" in Settings › General, and then they are reopened
  locked.

### Launching and opening files

- Paths given on the command line are opened like File › Open…:
  `cargo run -p editage-macos -- path/to/file.age`. Only arguments that
  name existing files are opened; other arguments (such as options macOS or
  Xcode may pass) are ignored.
- Files can also be opened from Finder through the application delegate's
  `application:openURLs:` (for example by dropping a file on the Dock icon),
  from File › Open Recent, and from the welcome window, which appears when no
  document is open.
- Opening a file that is already open brings its window to the front instead
  of opening it twice.

### Text system

- **Spelling.** With "Check Spelling While Typing" on (the default), AppKit
  sends the text to the system spelling service, which runs in a separate
  process on the same Mac, over local inter-process communication. The
  Security Inspector shows whether it is on. It is toggled with Edit ›
  Spelling and Grammar › Check Spelling While Typing. Grammar checking is off.
- **Substitutions and Writing Tools.** Automatic quote and dash substitution,
  text replacement, automatic spelling correction, link and data detection,
  smart insert and delete, and text completion are disabled in the editor.
  Silent changes would corrupt passwords and other exact text. Writing Tools
  are disabled with `setWritingToolsBehavior:` (none) where the method exists
  (macOS 15 and later), because they can send text to other processes or
  services.
- **Find.** The find bar is the native `NSTextFinder`. macOS may share the
  search text, and the selection used with Use Selection for Find (⌘E),
  with other applications through the system find pasteboard. Editage does
  not control or clear it; see
  [security-model.md](security-model.md#find-pasteboard). The find bar is
  hidden when a document locks.
- Input methods, Unicode, undo, find and replace, the context menu and
  VoiceOver come from the standard `NSTextView`.

### Clipboard

Copied text is written to `NSPasteboard.generalPasteboard` by the text view,
and its `changeCount` is recorded immediately afterwards. The clipboard is
cleared only if the `changeCount` is unchanged when the clear is due, and at
quit only if a clear delay is set and the item is still there. Universal
Clipboard (Handoff) may copy the clipboard to your other devices; this is
controlled by macOS, not by Editage.

## Windows (future)

There is no Windows frontend. Notes for whoever writes one:

- **`storage.rs` must gain a Windows implementation.** It uses
  `std::os::unix` and `libc` and does not compile on Windows. The
  `StorageBackend` trait is the boundary to implement.
- **Replacement.** Use `ReplaceFileW` to replace an existing file, or
  `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`. Note
  that `ReplaceFileW` preserves some attributes of the replaced file and
  behaves differently from a POSIX rename; decide deliberately and document
  the choice in [save-protocol.md](save-protocol.md).
- **Flushing.** `FlushFileBuffers` on the staging file before replacement.
- **Identity.** Use the volume serial number and file ID
  (`GetFileInformationByHandle`, or `FILE_ID_INFO` for ReFS) in place of
  device and inode.
- **Exclusive creation.** `CreateFileW` with `CREATE_NEW`.
- **Permissions.** There are no Unix mode bits; the equivalent of "owner
  only" is an ACL. Decide what a new file should get and state it in
  [security-model.md](security-model.md).
- **Cloud placeholders.** OneDrive and other Cloud Files API providers mark
  placeholders with `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS` and related
  attributes.
- **Hidden staging files.** A leading dot does not hide a file on Windows; set
  `FILE_ATTRIBUTE_HIDDEN` if hiding is wanted.
- **Clipboard.** Windows has a clipboard sequence number
  (`GetClipboardSequenceNumber`) that can play the role of `changeCount`.
  Clipboard history and cloud clipboard can keep copies.

## Linux (future)

There is no Linux frontend. The core compiles and its tests run on Linux in
CI, using the non-macOS code paths.

- **Flushing.** `File::sync_all` is `fsync(2)`, used for both the staging file
  and the folder. Whether `fsync` reaches stable storage depends on the
  filesystem, its mount options (for example ext4 `barrier`/`nobarrier`) and
  the drive. The folder flush after the rename is required on Linux for the
  new directory entry to be durable.
- **Replacement.** `rename(2)` is atomic within one filesystem, as on macOS.
  `renameat2` with `RENAME_NOREPLACE` could make the first save of a new
  document refuse to overwrite a file created in the meantime; it is not used
  today. There is no "replace only if unchanged" flag, so the small window
  before the rename remains.
- **Volumes.** `inspect_volume` and `is_cloud_placeholder` return defaults on
  non-macOS systems: network filesystems, read-only mounts and placeholders
  are not detected there yet. A Linux frontend should implement them
  (`statfs` `f_type` and `ST_RDONLY`).
- **Frontend.** A GTK frontend would use `GtkTextView` / `GtkTextBuffer` for
  the text, and must disable spelling and completion integrations or show them
  in the Security Inspector as the macOS frontend does.
- **Clipboard.** X11 and Wayland differ; there is no universal change counter.
  The clipboard policy will need a platform-appropriate way to identify the
  application's own item without reading the clipboard's contents.
