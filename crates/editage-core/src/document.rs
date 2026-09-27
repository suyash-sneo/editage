//! The lifecycle of one open document.
//!
//! [`DocumentSession`] is the single source of truth for a document's
//! security-relevant state: whether it is locked, whether a passphrase is
//! retained, whether a save is running, what is on disk. Frontends drive it
//! through named transitions and render what it reports; they keep no
//! parallel booleans of their own.
//!
//! The plaintext itself lives in the frontend's native text control, not
//! here. Keeping a second full copy in Rust would double the plaintext held
//! in memory for no benefit. The session tracks that the plaintext is present
//! and how large it is, and receives a short-lived copy only while saving.
//!
//! Slow work (decryption, encryption, file I/O) is packaged into job values
//! (`UnlockJob`, `SaveJob`, `ExternalCheckJob`) that a frontend runs on a
//! background thread and hands back with a `finish_*` call on the UI thread.
//! The session is therefore only ever mutated on one thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use crate::crypto::{
    decrypt_document, detect_encryption_format, EncryptionFormat, PassphraseVerifier,
};
use crate::diagnostics::{DiagnosticEvent, DiagnosticLog};
use crate::error::EditorError;
use crate::save::{
    CleanupResult, DestinationExpectation, SaveFailure, SaveJob, SaveResult, SaveStage, SaveSuccess,
};
use crate::secrets::{Credential, Plaintext};
use crate::storage::{
    read_encrypted_file, CiphertextFingerprint, DirectoryDurability, EncryptedFile, FileDurability,
    FileIdentity, OpenNotice, StorageBackend, VolumeInfo,
};

/// Process-unique identifier of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentId(u64);

impl DocumentId {
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        DocumentId(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The lifecycle states of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentState {
    /// Encrypted on disk; no plaintext held.
    Locked,
    /// Decryption is running in the background.
    Unlocking,
    /// Plaintext is in the editor and matches the last save.
    UnlockedClean,
    /// Plaintext is in the editor and differs from the last save (or has
    /// never been saved).
    UnlockedModified,
    /// A save transaction is running. Editing continues during a save.
    Saving(SaveStage),
    /// The last save failed; the edits are still in the editor.
    SaveFailed,
    /// The frontend is clearing its editor; the plaintext may still be there.
    Locking,
    /// The session has ended.
    Closed,
}

/// When the application keeps the passphrase after using it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassphrasePolicy {
    /// Retain it in a secret container until the document locks or closes,
    /// and use it for saves.
    KeepUntilLocked,
    /// Discard it right after unlocking; ask again whenever encrypting.
    AskAgainWhenSaving,
}

/// Why a document is locked, for the unlock sheet's explanation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReason {
    /// Just opened; never unlocked in this session.
    NotYetUnlocked,
    /// The user chose Lock Document.
    LockedByUser,
    /// Locked after this many minutes without activity.
    Inactivity { minutes: u32 },
}

/// Where the document lives on disk, if anywhere.
#[derive(Debug, Clone)]
pub enum FileBinding {
    /// A new document that has never been saved. No file exists.
    NotYetSaved,
    OnDisk(Box<OnDiskFile>),
}

#[derive(Debug, Clone)]
pub struct OnDiskFile {
    /// The path that saves replace (symbolic links already resolved).
    pub path: PathBuf,
    /// The path the user opened, if it was a symbolic link to `path`.
    pub opened_via: Option<PathBuf>,
    pub format: EncryptionFormat,
    /// Identity of the version last read or written by this application.
    pub identity: Option<FileIdentity>,
    /// Fingerprint of the version last read or written by this application.
    pub fingerprint: CiphertextFingerprint,
    pub ciphertext_bytes: u64,
    pub volume: VolumeInfo,
    pub notices: Vec<OpenNotice>,
    /// When this application last read or wrote the file.
    pub last_synchronised: SystemTime,
}

/// Whether plaintext for this document is held in application memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaintextPresence {
    NoPlaintext,
    /// The native editor holds the text. `bytes` is the UTF-8 length last
    /// reported by the frontend.
    InEditor {
        bytes: usize,
    },
}

/// Whether the editor's undo history may contain plaintext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoHistory {
    /// No edits have been made since the text was loaded.
    Empty,
    /// Edits were made; the undo stack holds earlier versions of text.
    MayContainPlaintext,
    /// Cleared by locking.
    Cleared,
}

/// The passphrase state, as it actually is.
pub enum PassphraseState {
    NotRetained,
    Retained(Credential),
}

impl std::fmt::Debug for PassphraseState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassphraseState::NotRetained => f.write_str("NotRetained"),
            PassphraseState::Retained(_) => f.write_str("Retained(<redacted>)"),
        }
    }
}

/// Result of the most recent external-change check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalChangeStatus {
    NotChecked,
    Unchanged {
        checked_at: SystemTime,
    },
    Changed {
        detected_at: SystemTime,
    },
    Missing {
        detected_at: SystemTime,
    },
    /// The check itself failed (e.g. the volume was disconnected).
    CheckFailed {
        detected_at: SystemTime,
        reason: String,
    },
}

/// A staging file that is still on disk after a save, with the reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftoverStagingFile {
    pub path: PathBuf,
    /// Whether the save it belonged to committed.
    pub save_committed: bool,
    pub removal_error: String,
}

/// Facts about the last committed save, for the inspector.
#[derive(Debug, Clone)]
pub struct SaveRecord {
    pub at: SystemTime,
    pub path: PathBuf,
    pub file_durability: FileDurability,
    pub directory_durability: DirectoryDurability,
}

/// How a save obtains its credential.
pub enum SaveCredential {
    /// Use the passphrase retained since unlocking.
    Retained,
    /// The user re-entered the document's current passphrase. It is checked
    /// against the document before anything is written.
    ReenteredCurrent(Credential),
    /// A new passphrase (first save of a new document, or Change Encryption
    /// Password). The UI has already asked for it twice.
    New(Credential),
}

/// Where a save goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveTarget {
    /// Replace the file this document is bound to.
    CurrentFile,
    /// A path the user chose in a save dialog.
    ChosenPath(PathBuf),
}

/// What a frontend must ask for before calling [`DocumentSession::begin_save`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveCredentialNeed {
    /// A retained passphrase is available.
    UseRetained,
    /// Ask for the document's current passphrase once.
    AskForCurrentPassphrase,
    /// Ask for a new passphrase and its confirmation.
    AskForNewPassphrase,
}

/// Outcome of asking to save.
#[derive(Debug)]
pub enum SaveStart {
    /// Run this job off the UI thread, then call `finish_save`.
    Started(SaveJob),
    /// A save is already running; one more save will be requested when it
    /// ends (see [`SaveCompletion::follow_up_save_requested`]).
    QueuedBehindRunningSave,
}

/// What the frontend should show after a save finishes.
#[derive(Debug)]
pub struct SaveCompletion {
    pub committed: bool,
    /// True if a save was requested while this one ran and there are edits
    /// newer than the ones just saved. The frontend should start one more
    /// save.
    pub follow_up_save_requested: bool,
}

/// Whether the document can be locked right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockReadiness {
    Ready,
    /// The user must choose: save first, or discard the edits.
    HasUnsavedChanges,
    SaveInProgress,
    /// A never-saved document cannot be locked (there is nothing to unlock).
    NeverSaved,
    AlreadyLocked,
}

/// How the user resolved unsaved changes when locking or closing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsavedChangesDecision {
    /// There were none (or they were saved first).
    NoUnsavedChanges,
    DiscardUnsavedChanges,
}

/// Confirmation from the frontend that it removed the text from its editor
/// and cleared the editor's undo history.
#[derive(Debug, Clone, Copy)]
pub struct EditorCleared {
    pub undo_history_cleared: bool,
}

/// What auto-lock should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoLockDecision {
    NotDue,
    LockNow,
    /// Due, but the document has unsaved changes; locking would lose them
    /// and saving without being asked is not allowed.
    PostponedUnsavedChanges,
    PostponedSaveInProgress,
    NotApplicable,
}

/// Why an unlock or reload was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnlockPurpose {
    Unlock,
    /// Revert to Saved, or Reload From Disk after an external change.
    ReloadFromDisk,
}

/// Decryption work to run off the UI thread.
pub struct UnlockJob {
    document_name: String,
    path: PathBuf,
    purpose: UnlockPurpose,
    /// Ciphertext that was already read and validated at open time, or
    /// `None` to read the current file from disk.
    already_read: Option<EncryptedFile>,
    credential: Credential,
    policy: PassphrasePolicy,
    diagnostics: DiagnosticLog,
}

impl std::fmt::Debug for UnlockJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnlockJob")
            .field("path", &self.path)
            .field("purpose", &self.purpose)
            .field("credential", &self.credential)
            .finish_non_exhaustive()
    }
}

/// The result of an [`UnlockJob`], to pass to `finish_unlock`/`finish_reload`.
pub struct UnlockOutcome {
    purpose: UnlockPurpose,
    policy: PassphrasePolicy,
    result: Result<DecryptedFile, (EditorError, Option<EncryptedFile>)>,
}

struct DecryptedFile {
    file: EncryptedFile,
    format: EncryptionFormat,
    plaintext: Plaintext,
    verifier: PassphraseVerifier,
    /// Returned so the session can retain it according to policy.
    credential: Credential,
}

impl UnlockJob {
    /// Reads (if needed) and decrypts. Blocking.
    pub fn run(self) -> UnlockOutcome {
        let UnlockJob {
            document_name,
            path,
            purpose,
            already_read,
            credential,
            policy,
            diagnostics,
        } = self;

        let file = match already_read {
            Some(file) => file,
            None => match read_encrypted_file(&path) {
                Ok(file) => {
                    diagnostics.record(
                        &document_name,
                        DiagnosticEvent::CiphertextRead {
                            bytes: file.ciphertext.len(),
                        },
                    );
                    file
                }
                Err(error) => {
                    return UnlockOutcome {
                        purpose,
                        policy,
                        result: Err((error, None)),
                    }
                }
            },
        };

        let format = match detect_encryption_format(&file.ciphertext) {
            Ok(format) => format,
            Err(error) => {
                return UnlockOutcome {
                    purpose,
                    policy,
                    result: Err((error, Some(file))),
                }
            }
        };
        let verifier = match PassphraseVerifier::from_ciphertext(&file.ciphertext, format) {
            Ok(verifier) => verifier,
            Err(error) => {
                return UnlockOutcome {
                    purpose,
                    policy,
                    result: Err((error, Some(file))),
                }
            }
        };

        let for_decryption = credential.duplicate_for_operation();
        match decrypt_document(&file.ciphertext, format, for_decryption) {
            Ok(plaintext) => {
                diagnostics.record(&document_name, DiagnosticEvent::DecryptionSucceeded);
                UnlockOutcome {
                    purpose,
                    policy,
                    result: Ok(DecryptedFile {
                        file,
                        format,
                        plaintext,
                        verifier,
                        credential,
                    }),
                }
            }
            Err(error) => {
                diagnostics.record(
                    &document_name,
                    DiagnosticEvent::DecryptionFailed {
                        reason: error.to_string(),
                    },
                );
                UnlockOutcome {
                    purpose,
                    policy,
                    result: Err((error, Some(file))),
                }
            }
        }
    }
}

/// Work for an external-change check, to run off the UI thread.
#[derive(Debug)]
pub struct ExternalCheckJob {
    path: PathBuf,
    expected_identity: Option<FileIdentity>,
    expected_fingerprint: CiphertextFingerprint,
}

#[derive(Debug)]
pub struct ExternalCheckOutcome {
    status: ExternalChangeStatus,
    checked_fingerprint: CiphertextFingerprint,
}

impl ExternalCheckJob {
    /// Compares the file on disk with the version this application last read
    /// or wrote.
    ///
    /// If device, inode, size and modification time are all unchanged, the
    /// contents are assumed unchanged without re-reading them (this check
    /// runs whenever the window becomes active). Otherwise the contents are
    /// hashed. The check made immediately before a save always hashes.
    pub fn run(self, storage: &dyn StorageBackend) -> ExternalCheckOutcome {
        let now = SystemTime::now();
        if let (Some(expected), Ok(metadata)) =
            (&self.expected_identity, std::fs::metadata(&self.path))
        {
            if FileIdentity::from_metadata(&metadata) == *expected {
                return ExternalCheckOutcome {
                    status: ExternalChangeStatus::Unchanged { checked_at: now },
                    checked_fingerprint: self.expected_fingerprint,
                };
            }
        }
        let status = match storage.inspect_destination(&self.path) {
            Ok(None) => ExternalChangeStatus::Missing { detected_at: now },
            // A folder, link or other non-file now at the path: the document
            // file is no longer there.
            Ok(Some(snapshot)) if snapshot.is_symbolic_link || !snapshot.is_regular_file => {
                ExternalChangeStatus::Missing { detected_at: now }
            }
            Ok(Some(snapshot)) if snapshot.fingerprint == self.expected_fingerprint => {
                ExternalChangeStatus::Unchanged { checked_at: now }
            }
            Ok(Some(_)) => ExternalChangeStatus::Changed { detected_at: now },
            Err(error) => ExternalChangeStatus::CheckFailed {
                detected_at: now,
                reason: error.to_string(),
            },
        };
        ExternalCheckOutcome {
            status,
            checked_fingerprint: self.expected_fingerprint,
        }
    }
}

/// One open document.
pub struct DocumentSession {
    id: DocumentId,
    display_name: String,
    binding: FileBinding,
    state: DocumentState,
    lock_reason: LockReason,
    plaintext: PlaintextPresence,
    undo_history: UndoHistory,
    passphrase: PassphraseState,
    policy: PassphrasePolicy,
    verifier: Option<PassphraseVerifier>,
    /// The file read at open time, kept (as ciphertext) until the first
    /// unlock so the bytes that were validated are the bytes decrypted.
    unopened_file: Option<EncryptedFile>,
    /// Incremented by every edit; compared with `saved_generation`.
    edit_generation: u64,
    saved_generation: u64,
    save_queued: bool,
    /// A new passphrase in use by the running save, retained on success if
    /// the policy says so.
    pending_new_credential: Option<Credential>,
    last_save: Option<SaveRecord>,
    last_failure: Option<SaveFailure>,
    leftover_staging_files: Vec<LeftoverStagingFile>,
    external_change: ExternalChangeStatus,
    auto_lock_postponed: bool,
    diagnostics: DiagnosticLog,
}

impl std::fmt::Debug for DocumentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let plaintext = match self.plaintext {
            PlaintextPresence::NoPlaintext => "<none>".to_owned(),
            PlaintextPresence::InEditor { bytes } => format!("<redacted: {bytes} bytes in editor>"),
        };
        f.debug_struct("DocumentSession")
            .field("id", &self.id)
            .field("name", &self.display_name)
            .field("path", &self.path())
            .field("state", &self.state)
            .field("plaintext", &plaintext)
            .field("passphrase", &self.passphrase)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl DocumentSession {
    // ----- Creating sessions ---------------------------------------------

    /// A new, empty, never-saved document. Its text exists only in memory.
    pub fn new_untitled(policy: PassphrasePolicy, diagnostics: DiagnosticLog) -> Self {
        let mut session = Self::blank(
            "Untitled".to_owned(),
            FileBinding::NotYetSaved,
            policy,
            diagnostics,
        );
        session.state = DocumentState::UnlockedClean;
        session.plaintext = PlaintextPresence::InEditor { bytes: 0 };
        session.undo_history = UndoHistory::Empty;
        session
    }

    fn blank(
        display_name: String,
        binding: FileBinding,
        policy: PassphrasePolicy,
        diagnostics: DiagnosticLog,
    ) -> Self {
        DocumentSession {
            id: DocumentId::next(),
            display_name,
            binding,
            state: DocumentState::Locked,
            lock_reason: LockReason::NotYetUnlocked,
            plaintext: PlaintextPresence::NoPlaintext,
            undo_history: UndoHistory::Empty,
            passphrase: PassphraseState::NotRetained,
            policy,
            verifier: None,
            unopened_file: None,
            edit_generation: 0,
            saved_generation: 0,
            save_queued: false,
            pending_new_credential: None,
            last_save: None,
            last_failure: None,
            leftover_staging_files: Vec::new(),
            external_change: ExternalChangeStatus::NotChecked,
            auto_lock_postponed: false,
            diagnostics,
        }
    }

    // ----- Accessors (all inspector text is derived from these) ---------

    pub fn id(&self) -> DocumentId {
        self.id
    }
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    pub fn state(&self) -> DocumentState {
        self.state
    }
    pub fn binding(&self) -> &FileBinding {
        &self.binding
    }
    pub fn path(&self) -> Option<&Path> {
        match &self.binding {
            FileBinding::NotYetSaved => None,
            FileBinding::OnDisk(file) => Some(&file.path),
        }
    }
    pub fn format(&self) -> EncryptionFormat {
        match &self.binding {
            FileBinding::NotYetSaved => EncryptionFormat::for_new_documents(),
            FileBinding::OnDisk(file) => file.format,
        }
    }
    pub fn lock_reason(&self) -> LockReason {
        self.lock_reason
    }
    pub fn plaintext_presence(&self) -> PlaintextPresence {
        self.plaintext
    }
    pub fn undo_history(&self) -> UndoHistory {
        self.undo_history
    }
    pub fn passphrase_policy(&self) -> PassphrasePolicy {
        self.policy
    }
    pub fn has_retained_passphrase(&self) -> bool {
        matches!(self.passphrase, PassphraseState::Retained(_))
    }
    pub fn last_save(&self) -> Option<&SaveRecord> {
        self.last_save.as_ref()
    }
    pub fn last_failure(&self) -> Option<&SaveFailure> {
        self.last_failure.as_ref()
    }
    pub fn leftover_staging_files(&self) -> &[LeftoverStagingFile] {
        &self.leftover_staging_files
    }
    pub fn external_change(&self) -> &ExternalChangeStatus {
        &self.external_change
    }
    pub fn auto_lock_postponed(&self) -> bool {
        self.auto_lock_postponed
    }
    pub fn diagnostics(&self) -> &DiagnosticLog {
        &self.diagnostics
    }
    pub fn notices(&self) -> &[OpenNotice] {
        match &self.binding {
            FileBinding::NotYetSaved => &[],
            FileBinding::OnDisk(file) => &file.notices,
        }
    }
    pub fn is_unlocked(&self) -> bool {
        matches!(
            self.state,
            DocumentState::UnlockedClean
                | DocumentState::UnlockedModified
                | DocumentState::Saving(_)
                | DocumentState::SaveFailed
        )
    }
    pub fn is_saving(&self) -> bool {
        matches!(self.state, DocumentState::Saving(_))
    }
    /// True if the editor holds edits newer than the last successful save.
    pub fn has_unsaved_changes(&self) -> bool {
        self.is_unlocked() && self.edit_generation != self.saved_generation
    }
    pub fn is_never_saved(&self) -> bool {
        matches!(self.binding, FileBinding::NotYetSaved)
    }
    /// Whether the file or volume is read-only, making Save impossible.
    pub fn is_read_only(&self) -> bool {
        self.notices()
            .iter()
            .any(|notice| matches!(notice, OpenNotice::ReadOnly { .. }))
    }

    fn invalid(&self, attempted: &'static str) -> EditorError {
        EditorError::InvalidTransition {
            from: self.state,
            attempted,
        }
    }

    fn file_name_for(path: &Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    }

    // ----- Unlocking ------------------------------------------------------

    /// Starts decrypting. The credential's kind must match
    /// [`EncryptionFormat::required_credential`] for this document.
    pub fn begin_unlock(
        &mut self,
        credential: Credential,
        policy: PassphrasePolicy,
    ) -> Result<UnlockJob, EditorError> {
        if self.state != DocumentState::Locked {
            return Err(self.invalid("Unlock"));
        }
        let FileBinding::OnDisk(file) = &self.binding else {
            return Err(self.invalid("Unlock"));
        };
        let path = file.path.clone();
        self.state = DocumentState::Unlocking;
        self.diagnostics
            .record(&self.display_name, DiagnosticEvent::UnlockRequested);
        Ok(UnlockJob {
            document_name: self.display_name.clone(),
            path,
            purpose: UnlockPurpose::Unlock,
            already_read: self.unopened_file.take(),
            credential,
            policy,
            diagnostics: self.diagnostics.clone(),
        })
    }

    /// Completes an unlock. On success returns the plaintext for the
    /// frontend to place in its editor (and then drop). On failure the
    /// document stays locked.
    pub fn finish_unlock(&mut self, outcome: UnlockOutcome) -> Result<Plaintext, EditorError> {
        if self.state != DocumentState::Unlocking || outcome.purpose != UnlockPurpose::Unlock {
            return Err(self.invalid("Finish unlock"));
        }
        match outcome.result {
            Ok(decrypted) => {
                self.policy = outcome.policy;
                let plaintext = self.accept_decrypted(decrypted);
                self.state = DocumentState::UnlockedClean;
                self.undo_history = UndoHistory::Empty;
                Ok(plaintext)
            }
            Err((error, file)) => {
                // Keep the validated bytes for the next attempt.
                if self.unopened_file.is_none() {
                    self.unopened_file = file;
                }
                self.state = DocumentState::Locked;
                Err(error)
            }
        }
    }

    fn accept_decrypted(&mut self, decrypted: DecryptedFile) -> Plaintext {
        let DecryptedFile {
            file,
            format,
            plaintext,
            verifier,
            credential,
        } = decrypted;

        self.bind_to_read_file(file, format);
        self.verifier = Some(verifier);
        self.passphrase = match self.policy {
            PassphrasePolicy::KeepUntilLocked => {
                self.diagnostics
                    .record(&self.display_name, DiagnosticEvent::PassphraseRetained);
                PassphraseState::Retained(credential)
            }
            PassphrasePolicy::AskAgainWhenSaving => {
                drop(credential);
                self.diagnostics
                    .record(&self.display_name, DiagnosticEvent::PassphraseNotRetained);
                PassphraseState::NotRetained
            }
        };
        self.plaintext = PlaintextPresence::InEditor {
            bytes: plaintext.byte_len(),
        };
        self.diagnostics.record(
            &self.display_name,
            DiagnosticEvent::PlaintextBufferCreated {
                bytes: plaintext.byte_len(),
            },
        );
        self.edit_generation = 0;
        self.saved_generation = 0;
        self.save_queued = false;
        self.last_failure = None;
        self.auto_lock_postponed = false;
        plaintext
    }

    fn bind_to_read_file(&mut self, file: EncryptedFile, format: EncryptionFormat) {
        let EncryptedFile {
            path,
            opened_via,
            ciphertext,
            identity,
            fingerprint,
            volume,
            notices,
        } = file;
        self.binding = FileBinding::OnDisk(Box::new(OnDiskFile {
            path,
            opened_via,
            format,
            identity: Some(identity),
            fingerprint,
            ciphertext_bytes: ciphertext.len() as u64,
            volume,
            notices,
            last_synchronised: SystemTime::now(),
        }));
        self.external_change = ExternalChangeStatus::Unchanged {
            checked_at: SystemTime::now(),
        };
    }

    // ----- Reloading (Revert to Saved / Reload From Disk) ----------------

    /// Starts re-reading and decrypting the file on disk, replacing the
    /// editor contents. Used for Revert to Saved and for resolving an
    /// external change. The credential is the retained passphrase or one
    /// the user just typed.
    pub fn begin_reload(&mut self, credential: SaveCredential) -> Result<UnlockJob, EditorError> {
        if !matches!(
            self.state,
            DocumentState::UnlockedClean
                | DocumentState::UnlockedModified
                | DocumentState::SaveFailed
        ) {
            return Err(self.invalid("Reload from disk"));
        }
        let FileBinding::OnDisk(file) = &self.binding else {
            return Err(self.invalid("Reload from disk"));
        };
        let credential = match credential {
            SaveCredential::Retained => match &self.passphrase {
                PassphraseState::Retained(retained) => retained.duplicate_for_operation(),
                PassphraseState::NotRetained => return Err(EditorError::PassphraseRequired),
            },
            SaveCredential::ReenteredCurrent(typed) | SaveCredential::New(typed) => typed,
        };
        Ok(UnlockJob {
            document_name: self.display_name.clone(),
            path: file.path.clone(),
            purpose: UnlockPurpose::ReloadFromDisk,
            already_read: None,
            credential,
            policy: self.policy,
            diagnostics: self.diagnostics.clone(),
        })
    }

    /// Completes a reload. On success the frontend must replace its editor
    /// text with the returned plaintext and clear its undo history. On
    /// failure the current edits stay as they are.
    pub fn finish_reload(&mut self, outcome: UnlockOutcome) -> Result<Plaintext, EditorError> {
        if outcome.purpose != UnlockPurpose::ReloadFromDisk
            || !self.is_unlocked()
            || self.is_saving()
        {
            return Err(self.invalid("Finish reload"));
        }
        match outcome.result {
            Ok(decrypted) => {
                // A reload with a typed passphrase keeps the existing
                // retention state: retain only if the policy says so.
                let plaintext = self.accept_decrypted(decrypted);
                self.state = DocumentState::UnlockedClean;
                self.undo_history = UndoHistory::Cleared;
                self.diagnostics
                    .record(&self.display_name, DiagnosticEvent::Reloaded);
                Ok(plaintext)
            }
            Err((error, _)) => Err(error),
        }
    }

    // ----- Editing --------------------------------------------------------

    /// Records that the user changed the text. `current_bytes` is the new
    /// UTF-8 length, used only for display.
    ///
    /// Undoing back to the saved text still counts as modified: the session
    /// does not keep a copy of the saved text to compare against.
    pub fn record_edit(&mut self, current_bytes: usize) -> Result<(), EditorError> {
        match self.state {
            DocumentState::UnlockedClean => self.state = DocumentState::UnlockedModified,
            DocumentState::UnlockedModified
            | DocumentState::Saving(_)
            | DocumentState::SaveFailed => {}
            _ => return Err(self.invalid("Edit")),
        }
        self.edit_generation += 1;
        self.plaintext = PlaintextPresence::InEditor {
            bytes: current_bytes,
        };
        self.undo_history = UndoHistory::MayContainPlaintext;
        Ok(())
    }

    // ----- Saving ---------------------------------------------------------

    /// What credential a save requires (the same for Save and Save As: a
    /// Save As copy keeps the document's current passphrase).
    pub fn save_credential_need(&self) -> SaveCredentialNeed {
        if self.is_never_saved() {
            return SaveCredentialNeed::AskForNewPassphrase;
        }
        if self.has_retained_passphrase() {
            SaveCredentialNeed::UseRetained
        } else {
            SaveCredentialNeed::AskForCurrentPassphrase
        }
    }

    /// Starts a save.
    ///
    /// `plaintext` is a copy of the editor's current text, made by the
    /// frontend immediately before this call. It is moved into the job and
    /// zeroized as soon as encryption finishes.
    pub fn begin_save(
        &mut self,
        target: SaveTarget,
        credential: SaveCredential,
        plaintext: Plaintext,
    ) -> Result<SaveStart, EditorError> {
        if self.is_saving() {
            self.save_queued = true;
            self.diagnostics.record(
                &self.display_name,
                DiagnosticEvent::SaveQueuedBehindRunningSave,
            );
            return Ok(SaveStart::QueuedBehindRunningSave);
        }
        if !matches!(
            self.state,
            DocumentState::UnlockedClean
                | DocumentState::UnlockedModified
                | DocumentState::SaveFailed
        ) {
            return Err(self.invalid("Save"));
        }

        let (destination, expectation) = match (&target, &self.binding) {
            (SaveTarget::CurrentFile, FileBinding::OnDisk(file)) => (
                file.path.clone(),
                DestinationExpectation::UnchangedSince {
                    fingerprint: file.fingerprint,
                },
            ),
            (SaveTarget::CurrentFile, FileBinding::NotYetSaved) => {
                return Err(self.invalid("Save without choosing a location"));
            }
            (SaveTarget::ChosenPath(path), FileBinding::OnDisk(file)) if *path == file.path => (
                file.path.clone(),
                DestinationExpectation::UnchangedSince {
                    fingerprint: file.fingerprint,
                },
            ),
            (SaveTarget::ChosenPath(path), _) => {
                (path.clone(), DestinationExpectation::UserChoseDestination)
            }
        };

        let (encrypt_with, passphrase_check, new_credential) = match credential {
            SaveCredential::Retained => match &self.passphrase {
                PassphraseState::Retained(retained) => {
                    (retained.duplicate_for_operation(), None, None)
                }
                PassphraseState::NotRetained => return Err(EditorError::PassphraseRequired),
            },
            SaveCredential::ReenteredCurrent(typed) => {
                let Some(verifier) = self.verifier.clone() else {
                    // A never-saved document has no current passphrase.
                    return Err(EditorError::PassphraseRequired);
                };
                let check_copy = typed.duplicate_for_operation();
                (typed, Some((verifier, check_copy)), None)
            }
            SaveCredential::New(new) => {
                let for_job = new.duplicate_for_operation();
                (for_job, None, Some(new))
            }
        };

        self.pending_new_credential = new_credential;
        self.state = DocumentState::Saving(SaveStage::CheckingDestination);
        self.save_queued = false;
        self.diagnostics
            .record(&self.display_name, DiagnosticEvent::SaveRequested);

        Ok(SaveStart::Started(SaveJob {
            document_name: self.display_name.clone(),
            destination,
            expectation,
            format: self.format(),
            plaintext,
            credential: encrypt_with,
            passphrase_check,
            edit_generation: self.edit_generation,
            diagnostics: self.diagnostics.clone(),
        }))
    }

    /// Records the stage a running save has reached (reported from the
    /// background thread through the frontend).
    pub fn record_save_stage(&mut self, stage: SaveStage) {
        if self.is_saving() {
            self.state = DocumentState::Saving(stage);
        }
    }

    /// Completes a save with the transaction's result.
    pub fn finish_save(&mut self, result: SaveResult) -> Result<SaveCompletion, EditorError> {
        if !self.is_saving() {
            return Err(self.invalid("Finish save"));
        }
        let pending_new_credential = self.pending_new_credential.take();
        match result {
            Ok(success) => {
                self.accept_committed_save(success, pending_new_credential);
                let follow_up = self.save_queued && self.has_unsaved_changes();
                self.save_queued = false;
                Ok(SaveCompletion {
                    committed: true,
                    follow_up_save_requested: follow_up,
                })
            }
            Err(failure) => {
                // A new passphrase that never reached disk is discarded; the
                // document is still encrypted under its previous passphrase
                // (or not at all, if it was never saved).
                drop(pending_new_credential);
                if let Some(staging) = &failure.staging_file {
                    if staging.still_exists {
                        self.leftover_staging_files.push(LeftoverStagingFile {
                            path: staging.path.clone(),
                            save_committed: false,
                            removal_error: staging
                                .removal_error
                                .as_ref()
                                .map(|error| error.to_string())
                                .unwrap_or_default(),
                        });
                    }
                }
                match failure.error {
                    EditorError::ExternalModification { .. } => {
                        self.external_change = ExternalChangeStatus::Changed {
                            detected_at: failure.failed_at,
                        };
                    }
                    EditorError::DestinationMissing { .. } => {
                        self.external_change = ExternalChangeStatus::Missing {
                            detected_at: failure.failed_at,
                        };
                    }
                    _ => {}
                }
                self.last_failure = Some(failure);
                self.state = DocumentState::SaveFailed;
                self.save_queued = false;
                Ok(SaveCompletion {
                    committed: false,
                    follow_up_save_requested: false,
                })
            }
        }
    }

    fn accept_committed_save(&mut self, success: SaveSuccess, new_credential: Option<Credential>) {
        let SaveSuccess {
            destination,
            fingerprint,
            identity,
            verifier,
            ciphertext_bytes,
            file_durability,
            directory_durability,
            cleanup,
            completed_at,
            edit_generation,
        } = success;

        let (opened_via, volume, notices) = match &self.binding {
            FileBinding::OnDisk(file) if file.path == destination => (
                file.opened_via.clone(),
                file.volume.clone(),
                file.notices
                    .iter()
                    .filter(|notice| !matches!(notice, OpenNotice::LargeDocument { .. }))
                    .cloned()
                    .collect(),
            ),
            _ => (
                None,
                crate::storage::inspect_volume(&destination),
                Vec::new(),
            ),
        };
        self.display_name = Self::file_name_for(&destination);
        self.binding = FileBinding::OnDisk(Box::new(OnDiskFile {
            path: destination.clone(),
            opened_via,
            format: self.format(),
            identity,
            fingerprint,
            ciphertext_bytes: ciphertext_bytes as u64,
            volume,
            notices,
            last_synchronised: completed_at,
        }));
        self.verifier = Some(verifier);
        if let Some(new_credential) = new_credential {
            self.passphrase = match self.policy {
                PassphrasePolicy::KeepUntilLocked => PassphraseState::Retained(new_credential),
                PassphrasePolicy::AskAgainWhenSaving => PassphraseState::NotRetained,
            };
        }
        self.saved_generation = edit_generation;
        self.state = if self.edit_generation == edit_generation {
            DocumentState::UnlockedClean
        } else {
            DocumentState::UnlockedModified
        };
        self.last_failure = None;
        self.external_change = ExternalChangeStatus::Unchanged {
            checked_at: completed_at,
        };
        self.last_save = Some(SaveRecord {
            at: completed_at,
            path: destination,
            file_durability,
            directory_durability,
        });
        if let CleanupResult::Failed { path, error } = cleanup {
            self.leftover_staging_files.push(LeftoverStagingFile {
                path,
                save_committed: true,
                removal_error: error.to_string(),
            });
        }
    }

    /// The user dismissed the save-failure sheet. The document remains
    /// modified; the failure record is kept for the inspector.
    pub fn acknowledge_save_failure(&mut self) {
        if self.state == DocumentState::SaveFailed {
            self.state = DocumentState::UnlockedModified;
        }
    }

    /// Tries again to remove leftover staging files. Returns the paths that
    /// are still present.
    pub fn retry_staging_cleanup(&mut self, storage: &dyn StorageBackend) -> Vec<PathBuf> {
        let mut still_present = Vec::new();
        for leftover in std::mem::take(&mut self.leftover_staging_files) {
            if !storage.staging_file_exists(&leftover.path) {
                continue;
            }
            match storage.remove_staging_file(&leftover.path) {
                Ok(()) => self.diagnostics.record(
                    &self.display_name,
                    DiagnosticEvent::StagingFileRemovedOnRetry {
                        path: leftover.path.clone(),
                    },
                ),
                Err(error) => {
                    still_present.push(leftover.path.clone());
                    self.leftover_staging_files.push(LeftoverStagingFile {
                        removal_error: error.to_string(),
                        ..leftover
                    });
                }
            }
        }
        still_present
    }

    // ----- External changes -----------------------------------------------

    /// Prepares a check of the file on disk, or `None` if there is no file.
    pub fn begin_external_check(&self) -> Option<ExternalCheckJob> {
        match &self.binding {
            FileBinding::OnDisk(file) if self.state != DocumentState::Closed => {
                Some(ExternalCheckJob {
                    path: file.path.clone(),
                    expected_identity: file.identity.clone(),
                    expected_fingerprint: file.fingerprint,
                })
            }
            _ => None,
        }
    }

    /// Records a check's result. Returns true if the file changed or
    /// disappeared (and that was not already known).
    pub fn finish_external_check(&mut self, outcome: ExternalCheckOutcome) -> bool {
        // Ignore stale results: a save may have committed while the check
        // was running.
        let current_fingerprint = match &self.binding {
            FileBinding::OnDisk(file) => file.fingerprint,
            FileBinding::NotYetSaved => return false,
        };
        if outcome.checked_fingerprint != current_fingerprint {
            return false;
        }
        let was_known = matches!(
            self.external_change,
            ExternalChangeStatus::Changed { .. } | ExternalChangeStatus::Missing { .. }
        );
        let newly_changed = matches!(
            outcome.status,
            ExternalChangeStatus::Changed { .. } | ExternalChangeStatus::Missing { .. }
        ) && !was_known;
        if newly_changed {
            let event = match outcome.status {
                ExternalChangeStatus::Missing { .. } => DiagnosticEvent::ExternalFileMissing,
                _ => DiagnosticEvent::ExternalChangeDetected,
            };
            self.diagnostics.record(&self.display_name, event);
        }
        // "Unchanged" means the file on disk is again exactly the version this
        // application last read or wrote (same fingerprint), so a previously
        // detected change no longer applies.
        if was_known && matches!(outcome.status, ExternalChangeStatus::Unchanged { .. }) {
            self.diagnostics
                .record(&self.display_name, DiagnosticEvent::ExternalChangeResolved);
        }
        self.external_change = outcome.status;
        newly_changed
    }

    // ----- Passphrase -----------------------------------------------------

    /// Releases the retained passphrase. Future saves will ask for it.
    pub fn forget_retained_passphrase(&mut self) {
        if let PassphraseState::Retained(credential) =
            std::mem::replace(&mut self.passphrase, PassphraseState::NotRetained)
        {
            drop(credential);
            self.diagnostics
                .record(&self.display_name, DiagnosticEvent::PassphraseReleased);
        }
        self.policy = PassphrasePolicy::AskAgainWhenSaving;
    }

    // ----- Locking --------------------------------------------------------

    pub fn lock_readiness(&self) -> LockReadiness {
        match self.state {
            DocumentState::Locked | DocumentState::Unlocking | DocumentState::Locking => {
                LockReadiness::AlreadyLocked
            }
            DocumentState::Closed => LockReadiness::AlreadyLocked,
            DocumentState::Saving(_) => LockReadiness::SaveInProgress,
            _ if self.is_never_saved() => LockReadiness::NeverSaved,
            _ if self.has_unsaved_changes() => LockReadiness::HasUnsavedChanges,
            _ => LockReadiness::Ready,
        }
    }

    /// First half of locking: the session stops treating the document as
    /// unlocked. The frontend must then clear its editor and undo history
    /// and call [`Self::finish_lock`].
    pub fn begin_lock(
        &mut self,
        decision: UnsavedChangesDecision,
        reason: LockReason,
    ) -> Result<(), EditorError> {
        match self.lock_readiness() {
            LockReadiness::Ready => {}
            LockReadiness::HasUnsavedChanges
                if decision == UnsavedChangesDecision::DiscardUnsavedChanges => {}
            _ => return Err(self.invalid("Lock")),
        }
        self.state = DocumentState::Locking;
        self.lock_reason = reason;
        Ok(())
    }

    /// Second half of locking: releases the passphrase and plaintext state.
    pub fn finish_lock(&mut self, cleared: EditorCleared) -> Result<(), EditorError> {
        if self.state != DocumentState::Locking {
            return Err(self.invalid("Finish lock"));
        }
        self.release_secrets(cleared);
        self.state = DocumentState::Locked;
        self.diagnostics
            .record(&self.display_name, DiagnosticEvent::Locked);
        Ok(())
    }

    fn release_secrets(&mut self, cleared: EditorCleared) {
        if let PassphraseState::Retained(credential) =
            std::mem::replace(&mut self.passphrase, PassphraseState::NotRetained)
        {
            drop(credential);
            self.diagnostics
                .record(&self.display_name, DiagnosticEvent::PassphraseReleased);
        }
        self.pending_new_credential = None;
        if self.plaintext != PlaintextPresence::NoPlaintext {
            self.diagnostics
                .record(&self.display_name, DiagnosticEvent::EditorBufferReleased);
        }
        self.plaintext = PlaintextPresence::NoPlaintext;
        if cleared.undo_history_cleared {
            self.undo_history = UndoHistory::Cleared;
            self.diagnostics
                .record(&self.display_name, DiagnosticEvent::UndoHistoryCleared);
        }
        self.edit_generation = 0;
        self.saved_generation = 0;
        self.save_queued = false;
        self.auto_lock_postponed = false;
    }

    /// Decides what inactivity auto-lock should do, and records a
    /// postponement so the inspector can show it.
    pub fn auto_lock_decision(
        &mut self,
        idle: Duration,
        lock_after: Option<Duration>,
    ) -> AutoLockDecision {
        let Some(lock_after) = lock_after else {
            self.auto_lock_postponed = false;
            return AutoLockDecision::NotApplicable;
        };
        let decision = match self.lock_readiness() {
            LockReadiness::AlreadyLocked | LockReadiness::NeverSaved => {
                AutoLockDecision::NotApplicable
            }
            _ if idle < lock_after => AutoLockDecision::NotDue,
            LockReadiness::Ready => AutoLockDecision::LockNow,
            LockReadiness::HasUnsavedChanges => AutoLockDecision::PostponedUnsavedChanges,
            LockReadiness::SaveInProgress => AutoLockDecision::PostponedSaveInProgress,
        };
        let postponed = decision == AutoLockDecision::PostponedUnsavedChanges;
        if postponed && !self.auto_lock_postponed {
            self.diagnostics.record(
                &self.display_name,
                DiagnosticEvent::AutoLockPostponedUnsavedChanges,
            );
        }
        self.auto_lock_postponed = postponed;
        decision
    }

    // ----- Closing --------------------------------------------------------

    /// Whether closing needs the user's decision about unsaved changes.
    pub fn close_needs_decision(&self) -> bool {
        self.has_unsaved_changes()
    }

    /// Ends the session. The frontend must have cleared (or be about to
    /// destroy) its editor.
    pub fn close(
        &mut self,
        decision: UnsavedChangesDecision,
        cleared: EditorCleared,
    ) -> Result<(), EditorError> {
        if self.is_saving() {
            return Err(self.invalid("Close"));
        }
        if self.has_unsaved_changes() && decision != UnsavedChangesDecision::DiscardUnsavedChanges {
            return Err(self.invalid("Close with unsaved changes"));
        }
        self.release_secrets(cleared);
        self.unopened_file = None;
        self.state = DocumentState::Closed;
        self.diagnostics
            .record(&self.display_name, DiagnosticEvent::Closed);
        Ok(())
    }
}

/// Reads and validates an encrypted file and creates a locked session for it.
///
/// This reads the ciphertext and parses the header (recognising the format
/// and checking it is passphrase-protected). It does not decrypt. Blocking;
/// call it off the UI thread.
pub fn open_encrypted_document(
    path: &Path,
    policy: PassphrasePolicy,
    diagnostics: DiagnosticLog,
) -> Result<DocumentSession, EditorError> {
    let name = DocumentSession::file_name_for(path);
    diagnostics.record(
        &name,
        DiagnosticEvent::OpenRequested {
            path: path.to_owned(),
        },
    );

    let file = read_encrypted_file(path).inspect_err(|error| {
        diagnostics.record(
            &name,
            DiagnosticEvent::OpenFailed {
                reason: error.to_string(),
            },
        );
    })?;
    diagnostics.record(
        &name,
        DiagnosticEvent::CiphertextRead {
            bytes: file.ciphertext.len(),
        },
    );
    let format = detect_encryption_format(&file.ciphertext).inspect_err(|error| {
        diagnostics.record(
            &name,
            DiagnosticEvent::OpenFailed {
                reason: error.to_string(),
            },
        );
    })?;
    diagnostics.record(
        &name,
        DiagnosticEvent::FormatRecognised {
            format: format.display_name(),
        },
    );

    let display_name = DocumentSession::file_name_for(&file.path);
    let mut session =
        DocumentSession::blank(display_name, FileBinding::NotYetSaved, policy, diagnostics);
    session.binding = FileBinding::OnDisk(Box::new(OnDiskFile {
        path: file.path.clone(),
        opened_via: file.opened_via.clone(),
        format,
        identity: Some(file.identity.clone()),
        fingerprint: file.fingerprint,
        ciphertext_bytes: file.ciphertext.len() as u64,
        volume: file.volume.clone(),
        notices: file.notices.clone(),
        last_synchronised: SystemTime::now(),
    }));
    session.external_change = ExternalChangeStatus::Unchanged {
        checked_at: SystemTime::now(),
    };
    session.unopened_file = Some(file);
    Ok(session)
}
