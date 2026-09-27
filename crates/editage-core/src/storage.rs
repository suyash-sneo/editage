//! Filesystem access: reading encrypted files, identifying them, and the
//! small set of operations the save transaction needs.
//!
//! The [`StorageBackend`] trait is intentionally narrow. It covers only the
//! operations at which a save can fail in an interesting way, so tests can
//! inject a failure at exactly one of them (see [`FaultInjectingStorage`]).
//! Everything else uses the standard library directly.
//!
//! Only Unix-family systems (macOS, Linux) are implemented. A Windows
//! frontend will need a Windows implementation of this module (file index
//! identity, `ReplaceFileW`/`MoveFileExW`, `FlushFileBuffers`).

use std::ffi::CString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::error::EditorError;

/// Above this ciphertext size the user is asked before the file is decrypted.
pub const LARGE_DOCUMENT_WARNING_BYTES: u64 = 10_000_000;

/// Files above this size are refused. A single native text view holding
/// hundreds of megabytes of text is impractical, and decrypting needs the
/// whole ciphertext and the whole plaintext in memory at once.
pub const MAXIMUM_DOCUMENT_BYTES: u64 = 500_000_000;

/// Permissions for newly created encrypted files and staging files: readable
/// and writable by the owner only.
pub const NEW_FILE_MODE: u32 = 0o600;

/// Filesystem metadata that identifies a particular version of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified: SystemTime,
    /// Permission bits (`st_mode & 0o777`).
    pub mode: u32,
    pub hard_link_count: u64,
}

impl FileIdentity {
    pub fn from_metadata(metadata: &fs::Metadata) -> Self {
        FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            // Permission bits only. Set-user-ID, set-group-ID and sticky
            // bits are deliberately not carried over to a replacement.
            mode: metadata.mode() & 0o777,
            hard_link_count: metadata.nlink(),
        }
    }
}

/// SHA-256 of the complete encrypted file.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CiphertextFingerprint([u8; 32]);

impl CiphertextFingerprint {
    pub fn of(ciphertext: &[u8]) -> Self {
        CiphertextFingerprint(Sha256::digest(ciphertext).into())
    }

    /// Full lowercase hex, for detail views.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// First 12 hex characters, for compact display.
    pub fn short_hex(&self) -> String {
        self.to_hex()[..12].to_owned()
    }
}

impl fmt::Debug for CiphertextFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.short_hex())
    }
}

/// What the save transaction learns about the destination before replacing it.
#[derive(Debug, Clone)]
pub struct DestinationSnapshot {
    pub identity: FileIdentity,
    pub fingerprint: CiphertextFingerprint,
    /// Whether the file's permissions allow this process to write it. The
    /// replacement itself only needs write access to the folder, but
    /// replacing a file the user marked read-only would silently defeat that
    /// choice, so the save refuses instead.
    pub writable: bool,
    pub is_symbolic_link: bool,
    /// False for a directory, a symbolic link, or any other non-file. Its
    /// contents are then not read, and a save refuses to replace it.
    pub is_regular_file: bool,
}

/// How far the operating system was asked to push the staging file towards
/// permanent storage before the original was replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDurability {
    /// macOS `fcntl(F_FULLFSYNC)`: the drive was asked to flush its own write
    /// cache. This is the strongest request macOS offers.
    FullFsync,
    /// `fsync(2)`. On macOS this reaches the drive but not necessarily its
    /// cache. Used on macOS only when `F_FULLFSYNC` is refused (some network
    /// and external filesystems); the refusal is recorded.
    FsyncOnly { full_fsync_refused: String },
    /// `fsync(2)` on a platform where it is the strongest available request.
    Fsync,
}

/// Whether the folder entry created by the rename was flushed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryDurability {
    Synced,
    /// The rename happened, so the save is committed and visible, but the
    /// folder could not be flushed. A power loss shortly afterwards might
    /// bring back the previous encrypted version.
    SyncFailed {
        error: String,
    },
}

/// The operations the save transaction performs, one per failure point.
pub trait StorageBackend: Send + Sync {
    /// Reads the destination's metadata and full contents (for the
    /// fingerprint). Returns `Ok(None)` if nothing exists at `path`.
    fn inspect_destination(&self, path: &Path) -> io::Result<Option<DestinationSnapshot>>;

    /// Creates a new file that must not already exist, readable and
    /// writable by the owner only.
    fn create_staging_file(&self, path: &Path) -> io::Result<File>;

    /// Writes all bytes, handling short writes.
    fn write_ciphertext(&self, file: &mut File, ciphertext: &[u8]) -> io::Result<()>;

    /// Flushes the file to storage as strongly as the platform allows.
    fn sync_file(&self, file: &File) -> io::Result<FileDurability>;

    /// Gives the complete staging file the permissions the replaced file
    /// had (or owner-only for a new file), through the still-open handle.
    fn apply_final_permissions(&self, file: &File, mode: u32) -> io::Result<()>;

    /// Atomically makes `destination` refer to the staging file.
    fn replace_destination(&self, staging: &Path, destination: &Path) -> io::Result<()>;

    /// Flushes a folder so a rename inside it is durable.
    fn sync_directory(&self, directory: &Path) -> io::Result<()>;

    /// Removes a staging file.
    fn remove_staging_file(&self, path: &Path) -> io::Result<()>;

    /// Whether a staging file still exists (used for cleanup reporting).
    fn staging_file_exists(&self, path: &Path) -> bool {
        fs::symlink_metadata(path).is_ok()
    }
}

/// The production backend: the local filesystem through the standard library
/// and a few direct system calls.
#[derive(Debug, Default, Clone, Copy)]
pub struct FileSystemStorage;

impl StorageBackend for FileSystemStorage {
    fn inspect_destination(&self, path: &Path) -> io::Result<Option<DestinationSnapshot>> {
        let link_metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let is_symbolic_link = link_metadata.file_type().is_symlink();
        let is_regular_file = link_metadata.file_type().is_file();
        if !is_regular_file {
            return Ok(Some(DestinationSnapshot {
                identity: FileIdentity::from_metadata(&link_metadata),
                fingerprint: CiphertextFingerprint::of(&[]),
                writable: false,
                is_symbolic_link,
                is_regular_file,
            }));
        }
        let contents = fs::read(path)?;
        Ok(Some(DestinationSnapshot {
            identity: FileIdentity::from_metadata(&link_metadata),
            fingerprint: CiphertextFingerprint::of(&contents),
            writable: is_writable_by_this_process(path),
            is_symbolic_link,
            is_regular_file,
        }))
    }

    fn create_staging_file(&self, path: &Path) -> io::Result<File> {
        // `create_new` maps to O_CREAT|O_EXCL: it fails rather than opening an
        // existing file or following a symbolic link planted at this name.
        // The umask can only remove permission bits, so the file starts at
        // 0600 or stricter.
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(NEW_FILE_MODE)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)
    }

    fn write_ciphertext(&self, file: &mut File, ciphertext: &[u8]) -> io::Result<()> {
        // `write_all` retries short writes and EINTR until every byte is
        // written or a real error occurs.
        file.write_all(ciphertext)?;
        file.flush()
    }

    fn sync_file(&self, file: &File) -> io::Result<FileDurability> {
        sync_file_to_storage(file)
    }

    fn apply_final_permissions(&self, file: &File, mode: u32) -> io::Result<()> {
        file.set_permissions(fs::Permissions::from_mode(mode))
    }

    fn replace_destination(&self, staging: &Path, destination: &Path) -> io::Result<()> {
        // rename(2) atomically replaces the destination directory entry when
        // both paths are on the same filesystem. Readers see either the old
        // complete file or the new complete file, never a mixture.
        fs::rename(staging, destination)
    }

    fn sync_directory(&self, directory: &Path) -> io::Result<()> {
        let handle = File::open(directory)?;
        sync_file_to_storage(&handle).map(|_| ())
    }

    fn remove_staging_file(&self, path: &Path) -> io::Result<()> {
        fs::remove_file(path)
    }
}

#[cfg(target_os = "macos")]
fn sync_file_to_storage(file: &File) -> io::Result<FileDurability> {
    use std::os::fd::AsRawFd;
    // SAFETY: `fcntl` is called with a valid, open file descriptor owned by
    // `file` for the duration of the call, and F_FULLFSYNC takes no argument.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
    if result == 0 {
        return Ok(FileDurability::FullFsync);
    }
    let refusal = io::Error::last_os_error();
    // Rust's `File::sync_all` is itself implemented with F_FULLFSYNC on
    // Apple platforms, so the fallback must call fsync(2) directly.
    // SAFETY: valid, open file descriptor owned by `file`.
    if unsafe { libc::fsync(file.as_raw_fd()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(FileDurability::FsyncOnly {
        full_fsync_refused: refusal.to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
fn sync_file_to_storage(file: &File) -> io::Result<FileDurability> {
    file.sync_all()?;
    Ok(FileDurability::Fsync)
}

fn path_to_cstring(path: &Path) -> Option<CString> {
    CString::new(path.as_os_str().as_bytes()).ok()
}

/// Whether the current process may write `path`, according to the OS.
pub fn is_writable_by_this_process(path: &Path) -> bool {
    let Some(c_path) = path_to_cstring(path) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

/// Facts about the volume holding a file that change how saving behaves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Filesystem type name, e.g. "apfs", "smbfs".
    pub filesystem_type: Option<String>,
    pub is_network_filesystem: bool,
    pub is_read_only: bool,
}

#[cfg(target_os = "macos")]
pub fn inspect_volume(path: &Path) -> VolumeInfo {
    let Some(c_path) = path_to_cstring(path) else {
        return VolumeInfo::default();
    };
    // SAFETY: `statfs` is plain old data; all-zero is a valid initial value.
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `stats` is a valid out pointer.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stats) } != 0 {
        return VolumeInfo::default();
    }
    let name_bytes: Vec<u8> = stats
        .f_fstypename
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    let filesystem_type = String::from_utf8_lossy(&name_bytes).into_owned();
    let is_network_filesystem = matches!(
        filesystem_type.as_str(),
        "smbfs" | "nfs" | "afpfs" | "webdav" | "cifs" | "ftp"
    );
    VolumeInfo {
        is_network_filesystem,
        is_read_only: stats.f_flags & (libc::MNT_RDONLY as u32) != 0,
        filesystem_type: Some(filesystem_type),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn inspect_volume(_path: &Path) -> VolumeInfo {
    VolumeInfo::default()
}

/// Whether the file is a cloud-storage placeholder (iCloud Drive, OneDrive
/// Files On-Demand and others using Apple's File Provider) whose contents are
/// not on this Mac.
#[cfg(target_os = "macos")]
pub fn is_cloud_placeholder(metadata: &fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt as MacMetadataExt;
    // SF_DATALESS from <sys/stat.h>; not exported by the libc crate.
    const SF_DATALESS: u32 = 0x4000_0000;
    metadata.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
pub fn is_cloud_placeholder(_metadata: &fs::Metadata) -> bool {
    false
}

/// Something about a file that the user should know about before editing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenNotice {
    /// The path the user chose is a symbolic link. Saves go to `target`.
    OpenedThroughSymbolicLink { link: PathBuf, target: PathBuf },
    /// Other directory entries point at the same file. Saving replaces this
    /// entry only; the others keep the previous encrypted version.
    MultipleHardLinks { count: u64 },
    /// The ciphertext is large (see [`LARGE_DOCUMENT_WARNING_BYTES`]).
    LargeDocument { bytes: u64 },
    /// The file or its volume is read-only; saving is not possible.
    ReadOnly { reason: ReadOnlyReason },
    /// The file is on a network filesystem where rename and flush guarantees
    /// depend on the server.
    NetworkFilesystem { filesystem_type: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadOnlyReason {
    FilePermissions,
    ReadOnlyVolume,
}

impl OpenNotice {
    /// Whether the user must explicitly continue before the document is
    /// unlocked. The others are informational and shown in the inspector.
    pub fn requires_confirmation(&self) -> bool {
        matches!(
            self,
            OpenNotice::OpenedThroughSymbolicLink { .. }
                | OpenNotice::MultipleHardLinks { .. }
                | OpenNotice::LargeDocument { .. }
        )
    }
}

/// An encrypted file read from disk, before any decryption.
#[derive(Debug)]
pub struct EncryptedFile {
    /// The path saves will replace (symbolic links resolved).
    pub path: PathBuf,
    /// The path the user chose, if it differs from `path`.
    pub opened_via: Option<PathBuf>,
    pub ciphertext: Vec<u8>,
    pub identity: FileIdentity,
    pub fingerprint: CiphertextFingerprint,
    pub volume: VolumeInfo,
    pub notices: Vec<OpenNotice>,
}

/// Reads a complete encrypted file and gathers everything needed to save it
/// back safely later.
pub fn read_encrypted_file(chosen_path: &Path) -> Result<EncryptedFile, EditorError> {
    let link_metadata =
        fs::symlink_metadata(chosen_path).map_err(|source| EditorError::FileOpen {
            path: chosen_path.to_owned(),
            source,
        })?;

    let mut notices = Vec::new();
    let (path, opened_via) = if link_metadata.file_type().is_symlink() {
        let target = fs::canonicalize(chosen_path).map_err(|source| EditorError::FileOpen {
            path: chosen_path.to_owned(),
            source,
        })?;
        notices.push(OpenNotice::OpenedThroughSymbolicLink {
            link: chosen_path.to_owned(),
            target: target.clone(),
        });
        (target, Some(chosen_path.to_owned()))
    } else {
        (chosen_path.to_owned(), None)
    };

    let metadata = fs::metadata(&path).map_err(|source| EditorError::FileOpen {
        path: path.clone(),
        source,
    })?;
    if !metadata.is_file() {
        return Err(EditorError::NotARegularFile { path });
    }
    if is_cloud_placeholder(&metadata) {
        // Reading a placeholder would make the sync provider download it,
        // possibly blocking for a long time. Ask the user to do that
        // explicitly instead.
        return Err(EditorError::CloudPlaceholder { path });
    }
    if metadata.len() > MAXIMUM_DOCUMENT_BYTES {
        return Err(EditorError::FileTooLarge {
            path,
            bytes: metadata.len(),
            limit: MAXIMUM_DOCUMENT_BYTES,
        });
    }

    let ciphertext = fs::read(&path).map_err(|source| EditorError::FileRead {
        path: path.clone(),
        source,
    })?;
    // Metadata is re-read after the contents so identity and fingerprint
    // describe the same version as closely as the filesystem allows.
    let metadata = fs::metadata(&path).map_err(|source| EditorError::FileRead {
        path: path.clone(),
        source,
    })?;
    let identity = FileIdentity::from_metadata(&metadata);
    let fingerprint = CiphertextFingerprint::of(&ciphertext);
    let volume = inspect_volume(&path);

    if identity.hard_link_count > 1 {
        notices.push(OpenNotice::MultipleHardLinks {
            count: identity.hard_link_count,
        });
    }
    if identity.size > LARGE_DOCUMENT_WARNING_BYTES {
        notices.push(OpenNotice::LargeDocument {
            bytes: identity.size,
        });
    }
    if volume.is_read_only {
        notices.push(OpenNotice::ReadOnly {
            reason: ReadOnlyReason::ReadOnlyVolume,
        });
    } else if !is_writable_by_this_process(&path) {
        notices.push(OpenNotice::ReadOnly {
            reason: ReadOnlyReason::FilePermissions,
        });
    }
    if volume.is_network_filesystem {
        notices.push(OpenNotice::NetworkFilesystem {
            filesystem_type: volume.filesystem_type.clone().unwrap_or_default(),
        });
    }

    Ok(EncryptedFile {
        path,
        opened_via,
        ciphertext,
        identity,
        fingerprint,
        volume,
        notices,
    })
}

/// Chooses a staging file name beside `destination`.
///
/// The staging file is created in the same folder as the destination. This is
/// intentional: an atomic rename is only guaranteed within one filesystem.
/// The staging file only ever contains ciphertext, so no plaintext document
/// data is written to this folder. The leading dot hides it in Finder; the
/// random part makes collisions and predictable names unlikely.
pub fn staging_path_for(destination: &Path) -> io::Result<PathBuf> {
    let folder = destination.parent().unwrap_or(Path::new("."));
    let file_name = destination
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document.age".to_owned());

    let mut random = [0u8; 8];
    getrandom::getrandom(&mut random).map_err(io::Error::other)?;
    let random_hex: String = random.iter().map(|b| format!("{b:02X}")).collect();

    // Keep the name within common 255-byte limits.
    let mut shortened = file_name;
    while shortened.len() > 200 {
        shortened.pop();
    }
    Ok(folder.join(format!(".{shortened}.{random_hex}.tmp")))
}

#[cfg(any(test, feature = "test-support"))]
pub use fault_injection::*;

/// Finds files beside `destination` whose names match this application's
/// staging-file pattern (`.<name>.<16 hex digits>.tmp`). Used so the
/// inspector reports staging files that actually exist on disk, including
/// ones left by an earlier session that was interrupted. Such files contain
/// ciphertext only.
pub fn find_staging_files(destination: &Path) -> Vec<PathBuf> {
    let Some(folder) = destination.parent() else {
        return Vec::new();
    };
    let Some(file_name) = destination
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return Vec::new();
    };
    let prefix = format!(".{file_name}.");
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(middle) = name
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(".tmp"))
            else {
                return false;
            };
            middle.len() == 16 && middle.chars().all(|c| c.is_ascii_hexdigit())
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

#[cfg(any(test, feature = "test-support"))]
mod fault_injection {
    //! A storage backend for tests that fails at one chosen operation.

    use super::*;
    use std::sync::Mutex;

    /// The operation at which [`FaultInjectingStorage`] fails.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FaultPoint {
        InspectDestination,
        CreateStagingFile,
        ApplyFinalPermissions,
        /// Writes the first half of the ciphertext, then fails.
        WriteCiphertextPartially,
        SyncFile,
        ReplaceDestination,
        SyncDirectory,
        RemoveStagingFile,
        /// Copies instead of renaming, leaving the staging file behind (to
        /// exercise post-commit cleanup), combined with a failing removal.
        ReplaceLeavesStagingFileAndRemovalFails,
    }

    /// Something a test wants to happen to the destination between two
    /// operations, simulating another program (e.g. a sync client).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ConcurrentChange {
        None,
        /// Overwrites the destination with these bytes just before the
        /// transaction's final check.
        RewriteDestinationBeforeReplace(Vec<u8>),
    }

    #[derive(Debug)]
    pub struct FaultInjectingStorage {
        pub fault: Option<FaultPoint>,
        pub concurrent_change: ConcurrentChange,
        pub error_kind: io::ErrorKind,
        inspections: Mutex<u32>,
    }

    impl FaultInjectingStorage {
        pub fn failing_at(fault: FaultPoint) -> Self {
            FaultInjectingStorage {
                fault: Some(fault),
                concurrent_change: ConcurrentChange::None,
                error_kind: io::ErrorKind::PermissionDenied,
                inspections: Mutex::new(0),
            }
        }

        pub fn with_concurrent_change(change: ConcurrentChange) -> Self {
            FaultInjectingStorage {
                fault: None,
                concurrent_change: change,
                error_kind: io::ErrorKind::PermissionDenied,
                inspections: Mutex::new(0),
            }
        }

        fn injected(&self, point: FaultPoint) -> io::Result<()> {
            if self.fault == Some(point) {
                Err(io::Error::new(
                    self.error_kind,
                    format!("injected failure at {point:?}"),
                ))
            } else {
                Ok(())
            }
        }
    }

    impl StorageBackend for FaultInjectingStorage {
        fn inspect_destination(&self, path: &Path) -> io::Result<Option<DestinationSnapshot>> {
            self.injected(FaultPoint::InspectDestination)?;
            let mut count = self.inspections.lock().expect("not poisoned");
            *count += 1;
            // The transaction inspects twice: once at the start and once just
            // before replacing. Change the file between the two.
            if *count == 2 {
                if let ConcurrentChange::RewriteDestinationBeforeReplace(bytes) =
                    &self.concurrent_change
                {
                    fs::write(path, bytes)?;
                }
            }
            FileSystemStorage.inspect_destination(path)
        }

        fn create_staging_file(&self, path: &Path) -> io::Result<File> {
            self.injected(FaultPoint::CreateStagingFile)?;
            FileSystemStorage.create_staging_file(path)
        }

        fn apply_final_permissions(&self, file: &File, mode: u32) -> io::Result<()> {
            self.injected(FaultPoint::ApplyFinalPermissions)?;
            FileSystemStorage.apply_final_permissions(file, mode)
        }

        fn write_ciphertext(&self, file: &mut File, ciphertext: &[u8]) -> io::Result<()> {
            if self.fault == Some(FaultPoint::WriteCiphertextPartially) {
                file.write_all(&ciphertext[..ciphertext.len() / 2])?;
                return self.injected(FaultPoint::WriteCiphertextPartially);
            }
            FileSystemStorage.write_ciphertext(file, ciphertext)
        }

        fn sync_file(&self, file: &File) -> io::Result<FileDurability> {
            self.injected(FaultPoint::SyncFile)?;
            FileSystemStorage.sync_file(file)
        }

        fn replace_destination(&self, staging: &Path, destination: &Path) -> io::Result<()> {
            self.injected(FaultPoint::ReplaceDestination)?;
            if self.fault == Some(FaultPoint::ReplaceLeavesStagingFileAndRemovalFails) {
                fs::copy(staging, destination)?;
                return Ok(());
            }
            FileSystemStorage.replace_destination(staging, destination)
        }

        fn sync_directory(&self, directory: &Path) -> io::Result<()> {
            self.injected(FaultPoint::SyncDirectory)?;
            FileSystemStorage.sync_directory(directory)
        }

        fn remove_staging_file(&self, path: &Path) -> io::Result<()> {
            self.injected(FaultPoint::RemoveStagingFile)?;
            self.injected(FaultPoint::ReplaceLeavesStagingFileAndRemovalFails)?;
            FileSystemStorage.remove_staging_file(path)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_path_is_hidden_random_and_in_the_same_folder() {
        let destination = Path::new("/Users/alice/OneDrive/Secure/passwords.txt.age");
        let first = staging_path_for(destination).unwrap();
        let second = staging_path_for(destination).unwrap();
        assert_eq!(first.parent(), destination.parent());
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".passwords.txt.age."));
        assert!(name.ends_with(".tmp"));
        assert_ne!(first, second);
    }

    #[test]
    fn staging_files_are_found_by_name_pattern_only() {
        let folder = tempfile::tempdir().unwrap();
        let destination = folder.path().join("notes.txt.age");
        fs::write(&destination, b"x").unwrap();
        let staging = staging_path_for(&destination).unwrap();
        fs::write(&staging, b"x").unwrap();
        fs::write(folder.path().join(".notes.txt.age.nothex.tmp"), b"x").unwrap();
        fs::write(folder.path().join(".other.age.0123456789ABCDEF.tmp"), b"x").unwrap();
        assert_eq!(find_staging_files(&destination), vec![staging]);
    }

    #[test]
    fn fingerprint_changes_when_one_byte_changes() {
        let a = CiphertextFingerprint::of(b"abc");
        let b = CiphertextFingerprint::of(b"abd");
        assert_ne!(a, b);
        assert_eq!(a.to_hex().len(), 64);
    }

    #[test]
    fn staging_files_are_created_owner_only() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("staging.tmp");
        let file = FileSystemStorage.create_staging_file(&path).unwrap();
        let mode = file.metadata().unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn staging_file_creation_refuses_to_reuse_an_existing_name() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("staging.tmp");
        fs::write(&path, b"existing").unwrap();
        let result = FileSystemStorage.create_staging_file(&path);
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    }

    #[test]
    fn opening_through_a_symbolic_link_reports_the_resolved_target() {
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("real.age");
        fs::write(&target, b"age-encryption.org/v1\n").unwrap();
        let link = folder.path().join("link.age");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let opened = read_encrypted_file(&link).unwrap();
        assert_eq!(opened.path, fs::canonicalize(&target).unwrap());
        assert_eq!(opened.opened_via.as_deref(), Some(link.as_path()));
        assert!(opened
            .notices
            .iter()
            .any(|n| matches!(n, OpenNotice::OpenedThroughSymbolicLink { .. })));
    }

    #[test]
    fn hard_linked_files_are_reported() {
        let folder = tempfile::tempdir().unwrap();
        let original = folder.path().join("a.age");
        fs::write(&original, b"x").unwrap();
        fs::hard_link(&original, folder.path().join("b.age")).unwrap();
        let opened = read_encrypted_file(&original).unwrap();
        assert!(opened
            .notices
            .contains(&OpenNotice::MultipleHardLinks { count: 2 }));
    }

    #[test]
    fn read_only_files_are_reported() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("ro.age");
        fs::write(&path, b"x").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        let opened = read_encrypted_file(&path).unwrap();
        assert!(opened.notices.contains(&OpenNotice::ReadOnly {
            reason: ReadOnlyReason::FilePermissions
        }));
    }

    #[test]
    fn directories_are_rejected() {
        let folder = tempfile::tempdir().unwrap();
        let result = read_encrypted_file(folder.path());
        assert!(matches!(result, Err(EditorError::NotARegularFile { .. })));
    }
}
