//! The save transaction.
//!
//! `docs/save-protocol.md` describes these same stages in the same order;
//! keep them in step when either changes. The "How saving works" text in the
//! Security Inspector is generated from [`SaveStage::mechanism_step`], so it
//! cannot drift from the stage list.
//!
//! Guarantee: before [`SaveStage::ReplacingOriginal`] succeeds, the previous
//! encrypted file is never opened for writing, truncated, or renamed. Every
//! failure before that point leaves it byte-for-byte unchanged.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::crypto::{encrypt_document, EncryptionFormat, PassphraseVerifier};
use crate::diagnostics::{DiagnosticEvent, DiagnosticLog};
use crate::error::EditorError;
use crate::secrets::{Credential, Plaintext};
use crate::storage::{
    staging_path_for, CiphertextFingerprint, DirectoryDurability, FileDurability, FileIdentity,
    StorageBackend, NEW_FILE_MODE,
};

/// The stages of a save, in execution order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SaveStage {
    CheckingDestination,
    ConfirmingPassphrase,
    Encrypting,
    CreatingStagingFile,
    WritingCiphertext,
    Flushing,
    ReplacingOriginal,
    CleaningUp,
    Complete,
}

impl SaveStage {
    pub const ALL: [SaveStage; 9] = [
        SaveStage::CheckingDestination,
        SaveStage::ConfirmingPassphrase,
        SaveStage::Encrypting,
        SaveStage::CreatingStagingFile,
        SaveStage::WritingCiphertext,
        SaveStage::Flushing,
        SaveStage::ReplacingOriginal,
        SaveStage::CleaningUp,
        SaveStage::Complete,
    ];

    /// Name used in failure details and diagnostics.
    pub fn display_name(&self) -> &'static str {
        match self {
            SaveStage::CheckingDestination => "Checking destination",
            SaveStage::ConfirmingPassphrase => "Confirming passphrase",
            SaveStage::Encrypting => "Encryption",
            SaveStage::CreatingStagingFile => "Creating staging file",
            SaveStage::WritingCiphertext => "Writing encrypted data",
            SaveStage::Flushing => "Flushing to disk",
            SaveStage::ReplacingOriginal => "Atomic replacement",
            SaveStage::CleaningUp => "Cleanup",
            SaveStage::Complete => "Complete",
        }
    }

    /// One step of the user-facing "How saving works" explanation.
    pub fn mechanism_step(&self) -> Option<&'static str> {
        match self {
            SaveStage::CheckingDestination => Some(
                "The encrypted file on disk is checked to make sure it has not changed since it was opened.",
            ),
            SaveStage::ConfirmingPassphrase => Some(
                "If the password is not kept in memory, the password you enter is checked against the document before anything is written.",
            ),
            SaveStage::Encrypting => Some("The current text is encrypted in application memory."),
            SaveStage::CreatingStagingFile => Some(
                "A new staging file, readable only by you while it is written, is created beside the document.",
            ),
            SaveStage::WritingCiphertext => {
                Some("The encrypted result is written to the staging file.")
            }
            SaveStage::Flushing => Some("The staging file is flushed to the disk."),
            SaveStage::ReplacingOriginal => Some(
                "The file on disk is checked once more, the staging file is given the original file’s permissions, and the original encrypted file is atomically replaced by the staging file.",
            ),
            SaveStage::CleaningUp => Some("The staging file is removed if it still exists."),
            SaveStage::Complete => None,
        }
    }
}

/// The closing sentence of the "How saving works" explanation.
pub const MECHANISM_PLAINTEXT_STATEMENT: &str =
    "Plaintext document content is not intentionally written to a temporary file during this process.";

/// The numbered "How saving works" steps, generated from the stage list.
pub fn save_mechanism_steps() -> Vec<&'static str> {
    SaveStage::ALL
        .iter()
        .filter_map(|stage| stage.mechanism_step())
        .collect()
}

/// What the transaction expects to find at the destination.
#[derive(Debug, Clone)]
pub enum DestinationExpectation {
    /// A normal save: the file must still be exactly the version that was
    /// opened (or last saved). Anything else is an external modification.
    UnchangedSince { fingerprint: CiphertextFingerprint },
    /// Save As or a first save: the user picked this path in a save dialog,
    /// which already asked before replacing an existing file.
    UserChoseDestination,
}

/// Everything a save needs, moved onto a background thread as one value.
///
/// Built by [`crate::document::DocumentSession::begin_save`].
pub struct SaveJob {
    pub(crate) document_name: String,
    pub(crate) destination: PathBuf,
    pub(crate) expectation: DestinationExpectation,
    pub(crate) format: EncryptionFormat,
    pub(crate) plaintext: Plaintext,
    pub(crate) credential: Credential,
    /// Present when the passphrase was typed for this save and must match the
    /// document's current passphrase.
    pub(crate) passphrase_check: Option<(PassphraseVerifier, Credential)>,
    pub(crate) edit_generation: u64,
    pub(crate) diagnostics: DiagnosticLog,
}

impl SaveJob {
    pub fn destination(&self) -> &Path {
        &self.destination
    }
}

impl std::fmt::Debug for SaveJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveJob")
            .field("destination", &self.destination)
            .field("expectation", &self.expectation)
            .field("format", &self.format)
            .field("plaintext", &self.plaintext)
            .field("credential", &self.credential)
            .field("edit_generation", &self.edit_generation)
            .finish_non_exhaustive()
    }
}

/// What happened to the staging file after the transaction.
#[derive(Debug)]
pub enum CleanupResult {
    /// The rename consumed the staging file; nothing was left to remove.
    NothingLeft,
    /// A leftover staging file was found and removed.
    Removed { path: PathBuf },
    /// A leftover staging file could not be removed. It contains ciphertext
    /// only. The save itself succeeded.
    Failed { path: PathBuf, error: EditorError },
}

/// A committed save.
#[derive(Debug)]
pub struct SaveSuccess {
    pub destination: PathBuf,
    pub fingerprint: CiphertextFingerprint,
    /// `None` if the file could not be re-read after the rename. The save is
    /// still committed; the next external-change check will use the
    /// fingerprint alone.
    pub identity: Option<FileIdentity>,
    pub verifier: PassphraseVerifier,
    pub ciphertext_bytes: usize,
    pub file_durability: FileDurability,
    pub directory_durability: DirectoryDurability,
    pub cleanup: CleanupResult,
    pub completed_at: SystemTime,
    pub(crate) edit_generation: u64,
}

/// The state of the previous file after a failed save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginalFileState {
    /// The file that was there before is still there, unchanged by this
    /// application.
    Unchanged,
    /// There was no file at the destination (first save or a new Save As
    /// location), and there still is not.
    DidNotExist,
    /// The file changed on disk because of something else. This application
    /// did not modify it.
    ChangedByAnotherProgram,
}

/// Whether an encrypted staging file is left on disk after a failure.
#[derive(Debug)]
pub struct StagingFileReport {
    pub path: PathBuf,
    pub still_exists: bool,
    pub removal_attempted: bool,
    /// Why removal failed, if it did.
    pub removal_error: Option<EditorError>,
}

/// A save that did not commit.
#[derive(Debug)]
pub struct SaveFailure {
    pub stage: SaveStage,
    pub destination: PathBuf,
    pub error: EditorError,
    pub original: OriginalFileState,
    pub staging_file: Option<StagingFileReport>,
    pub failed_at: SystemTime,
}

pub type SaveResult = Result<SaveSuccess, SaveFailure>;

/// Runs a complete save transaction. Blocking; call it off the UI thread.
///
/// `report_stage` is called as each stage begins, so the frontend and the
/// diagnostics can say what is currently happening.
// A save produces one result per user action, so the size of the error
// variant is irrelevant; keeping `SaveFailure` unboxed keeps matching simple.
#[allow(clippy::result_large_err)]
pub fn run_save_transaction(
    job: SaveJob,
    storage: &dyn StorageBackend,
    report_stage: &mut dyn FnMut(SaveStage),
) -> SaveResult {
    let SaveJob {
        document_name,
        destination,
        expectation,
        format,
        plaintext,
        credential,
        passphrase_check,
        edit_generation,
        diagnostics,
    } = job;

    let mut enter = |stage: SaveStage| {
        diagnostics.record(&document_name, DiagnosticEvent::SaveStageStarted(stage));
        report_stage(stage);
    };
    let fail = |stage: SaveStage,
                error: EditorError,
                original: OriginalFileState,
                staging_file: Option<StagingFileReport>| {
        diagnostics.record(
            &document_name,
            DiagnosticEvent::SaveFailed {
                stage,
                reason: error.to_string(),
            },
        );
        SaveFailure {
            stage,
            destination: destination.clone(),
            error,
            original,
            staging_file,
            failed_at: SystemTime::now(),
        }
    };

    // Stage 1: validate the destination and detect external modification.
    enter(SaveStage::CheckingDestination);
    let folder = destination.parent().unwrap_or(Path::new(".")).to_owned();
    match std::fs::metadata(&folder) {
        Ok(metadata) if metadata.is_dir() => {}
        folder_problem => {
            let error = EditorError::DestinationDirectoryUnavailable {
                path: folder,
                source: folder_problem.err(),
            };
            // Nothing was touched. For a normal save the original is simply
            // wherever it was; for a new location there was nothing there.
            let original = match expectation {
                DestinationExpectation::UnchangedSince { .. } => OriginalFileState::Unchanged,
                DestinationExpectation::UserChoseDestination => OriginalFileState::DidNotExist,
            };
            return Err(fail(SaveStage::CheckingDestination, error, original, None));
        }
    }
    let existing = match check_destination(storage, &destination, &expectation) {
        Ok(existing) => existing,
        Err((error, original)) => {
            return Err(fail(SaveStage::CheckingDestination, error, original, None));
        }
    };
    let original_if_we_fail = if existing.is_some() {
        OriginalFileState::Unchanged
    } else {
        OriginalFileState::DidNotExist
    };
    let final_mode = existing
        .as_ref()
        .map(|identity| identity.mode)
        .unwrap_or(NEW_FILE_MODE);

    // Stage 2 (only when the passphrase was typed for this save).
    if let Some((verifier, typed)) = passphrase_check {
        enter(SaveStage::ConfirmingPassphrase);
        if let Err(error) = verifier.confirm(typed) {
            return Err(fail(
                SaveStage::ConfirmingPassphrase,
                error,
                original_if_we_fail,
                None,
            ));
        }
    }

    // Stage 3: encrypt in memory. Nothing has been written yet.
    enter(SaveStage::Encrypting);
    let ciphertext = match encrypt_document(&plaintext, format, credential) {
        Ok(ciphertext) => ciphertext,
        Err(error) => {
            return Err(fail(
                SaveStage::Encrypting,
                error,
                original_if_we_fail,
                None,
            ))
        }
    };
    // The transaction's copy of the plaintext is no longer needed. Dropping
    // it here zeroizes it before any file is touched.
    drop(plaintext);
    let fingerprint = CiphertextFingerprint::of(&ciphertext);
    let verifier = match PassphraseVerifier::from_ciphertext(&ciphertext, format) {
        Ok(verifier) => verifier,
        Err(error) => {
            return Err(fail(
                SaveStage::Encrypting,
                error,
                original_if_we_fail,
                None,
            ))
        }
    };

    // Stage 4: create the staging file beside the destination.
    enter(SaveStage::CreatingStagingFile);
    let staging_path = match staging_path_for(&destination) {
        Ok(path) => path,
        Err(source) => {
            let error = EditorError::StagingFileCreate {
                path: folder.clone(),
                source,
            };
            return Err(fail(
                SaveStage::CreatingStagingFile,
                error,
                original_if_we_fail,
                None,
            ));
        }
    };
    let mut staging_file = match storage.create_staging_file(&staging_path) {
        Ok(file) => file,
        Err(source) => {
            let error = EditorError::StagingFileCreate {
                path: staging_path.clone(),
                source,
            };
            // Normally nothing was created, but check rather than assume.
            let staging_report = remove_leftover_staging(storage, &staging_path);
            return Err(fail(
                SaveStage::CreatingStagingFile,
                error,
                original_if_we_fail,
                staging_report,
            ));
        }
    };
    diagnostics.record(
        &document_name,
        DiagnosticEvent::StagingFileCreated {
            path: staging_path.clone(),
        },
    );

    // From here on a staging file exists. Every failure path removes it (or
    // reports why it could not).

    // Stage 5: write the complete ciphertext.
    enter(SaveStage::WritingCiphertext);
    if let Err(source) = storage.write_ciphertext(&mut staging_file, &ciphertext) {
        drop(staging_file);
        let error = EditorError::StagingFileWrite {
            path: staging_path.clone(),
            source,
        };
        let staging_report = remove_leftover_staging(storage, &staging_path);
        return Err(fail(
            SaveStage::WritingCiphertext,
            error,
            original_if_we_fail,
            staging_report,
        ));
    }

    // Stage 6: flush the staging file before it can replace anything.
    enter(SaveStage::Flushing);
    let file_durability = match storage.sync_file(&staging_file) {
        Ok(durability) => durability,
        Err(source) => {
            drop(staging_file);
            let error = EditorError::Flush {
                path: staging_path.clone(),
                source,
            };
            let staging_report = remove_leftover_staging(storage, &staging_path);
            return Err(fail(
                SaveStage::Flushing,
                error,
                original_if_we_fail,
                staging_report,
            ));
        }
    };

    // Stage 7: final check, permissions, then atomic replacement. The rename
    // is the commit point.
    enter(SaveStage::ReplacingOriginal);
    // Checked again here, as late as possible, because encrypting can take
    // about a second and a sync client may have written in the meantime. A
    // small window between this check and the rename remains; see
    // docs/save-protocol.md.
    if let Err((error, original)) = check_destination(storage, &destination, &expectation) {
        drop(staging_file);
        let staging_report = remove_leftover_staging(storage, &staging_path);
        return Err(fail(
            SaveStage::ReplacingOriginal,
            error,
            original,
            staging_report,
        ));
    }
    // The staging file stayed owner-only while it was written. Only now,
    // complete and flushed, does it get the permissions the replaced file
    // had, so the replacement does not silently change who can read it.
    if let Err(source) = storage.apply_final_permissions(&staging_file, final_mode) {
        drop(staging_file);
        let error = EditorError::Permissions {
            path: staging_path.clone(),
            source,
        };
        let staging_report = remove_leftover_staging(storage, &staging_path);
        return Err(fail(
            SaveStage::ReplacingOriginal,
            error,
            original_if_we_fail,
            staging_report,
        ));
    }
    close_file(staging_file);
    if let Err(source) = storage.replace_destination(&staging_path, &destination) {
        let error = EditorError::AtomicReplace {
            staging: staging_path.clone(),
            destination: destination.clone(),
            source,
        };
        let staging_report = remove_leftover_staging(storage, &staging_path);
        return Err(fail(
            SaveStage::ReplacingOriginal,
            error,
            original_if_we_fail,
            staging_report,
        ));
    }
    diagnostics.record(&document_name, DiagnosticEvent::AtomicReplacementSucceeded);

    // The save is committed. Nothing below can turn it into a failure.
    let directory_durability = match storage.sync_directory(&folder) {
        Ok(()) => DirectoryDurability::Synced,
        Err(error) => DirectoryDurability::SyncFailed {
            error: error.to_string(),
        },
    };
    let identity = storage
        .inspect_destination(&destination)
        .ok()
        .flatten()
        .filter(|snapshot| snapshot.fingerprint == fingerprint)
        .map(|snapshot| snapshot.identity);

    // Stage 8: cleanup.
    enter(SaveStage::CleaningUp);
    let cleanup = if storage.staging_file_exists(&staging_path) {
        match storage.remove_staging_file(&staging_path) {
            Ok(()) => CleanupResult::Removed {
                path: staging_path.clone(),
            },
            Err(source) => {
                diagnostics.record(
                    &document_name,
                    DiagnosticEvent::CleanupFailed {
                        path: staging_path.clone(),
                    },
                );
                CleanupResult::Failed {
                    path: staging_path.clone(),
                    error: EditorError::Cleanup {
                        path: staging_path.clone(),
                        source,
                    },
                }
            }
        }
    } else {
        CleanupResult::NothingLeft
    };

    enter(SaveStage::Complete);
    diagnostics.record(
        &document_name,
        DiagnosticEvent::SaveCompleted {
            ciphertext_bytes: ciphertext.len(),
        },
    );
    Ok(SaveSuccess {
        destination,
        fingerprint,
        identity,
        verifier,
        ciphertext_bytes: ciphertext.len(),
        file_durability,
        directory_durability,
        cleanup,
        completed_at: SystemTime::now(),
        edit_generation,
    })
}

/// Checks the destination against the expectation. Returns the identity of
/// the file currently there (if any).
fn check_destination(
    storage: &dyn StorageBackend,
    destination: &Path,
    expectation: &DestinationExpectation,
) -> Result<Option<FileIdentity>, (EditorError, OriginalFileState)> {
    let snapshot = storage.inspect_destination(destination).map_err(|source| {
        (
            EditorError::DestinationInspect {
                path: destination.to_owned(),
                source,
            },
            OriginalFileState::Unchanged,
        )
    })?;

    match (expectation, snapshot) {
        (DestinationExpectation::UnchangedSince { .. }, None) => Err((
            EditorError::DestinationMissing {
                path: destination.to_owned(),
            },
            OriginalFileState::ChangedByAnotherProgram,
        )),
        (DestinationExpectation::UnchangedSince { fingerprint }, Some(snapshot)) => {
            if snapshot.is_symbolic_link || !snapshot.is_regular_file {
                return Err((
                    EditorError::NotARegularFile {
                        path: destination.to_owned(),
                    },
                    OriginalFileState::ChangedByAnotherProgram,
                ));
            }
            // The content fingerprint is decisive. Metadata such as the
            // modification time can change without the contents changing
            // (e.g. a sync client touching the file), which is harmless.
            if snapshot.fingerprint != *fingerprint {
                return Err((
                    EditorError::ExternalModification {
                        path: destination.to_owned(),
                    },
                    OriginalFileState::ChangedByAnotherProgram,
                ));
            }
            if !snapshot.writable {
                return Err((
                    EditorError::DestinationReadOnly {
                        path: destination.to_owned(),
                    },
                    OriginalFileState::Unchanged,
                ));
            }
            Ok(Some(snapshot.identity))
        }
        (DestinationExpectation::UserChoseDestination, None) => Ok(None),
        (DestinationExpectation::UserChoseDestination, Some(snapshot)) => {
            if snapshot.is_symbolic_link || !snapshot.is_regular_file {
                return Err((
                    EditorError::NotARegularFile {
                        path: destination.to_owned(),
                    },
                    OriginalFileState::Unchanged,
                ));
            }
            if !snapshot.writable {
                return Err((
                    EditorError::DestinationReadOnly {
                        path: destination.to_owned(),
                    },
                    OriginalFileState::Unchanged,
                ));
            }
            Ok(Some(snapshot.identity))
        }
    }
}

/// After a failure: removes the staging file if it exists and reports what
/// happened, so the user can be told exactly what is left on disk.
fn remove_leftover_staging(
    storage: &dyn StorageBackend,
    staging_path: &Path,
) -> Option<StagingFileReport> {
    if !storage.staging_file_exists(staging_path) {
        return None;
    }
    match storage.remove_staging_file(staging_path) {
        Ok(()) => Some(StagingFileReport {
            path: staging_path.to_owned(),
            still_exists: storage.staging_file_exists(staging_path),
            removal_attempted: true,
            removal_error: None,
        }),
        Err(source) => Some(StagingFileReport {
            path: staging_path.to_owned(),
            still_exists: storage.staging_file_exists(staging_path),
            removal_attempted: true,
            removal_error: Some(EditorError::Cleanup {
                path: staging_path.to_owned(),
                source,
            }),
        }),
    }
}

/// Explicitly closes the staging handle before the rename. Close errors after
/// a successful sync cannot lose data that was already flushed, so they are
/// not treated as save failures.
fn close_file(file: File) {
    drop(file);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mechanism_steps_follow_stage_order_and_mention_the_atomic_replacement() {
        let steps = save_mechanism_steps();
        assert_eq!(steps.len(), SaveStage::ALL.len() - 1);
        assert!(steps[2].contains("encrypted in application memory"));
        assert!(steps
            .iter()
            .any(|step| step.contains("atomically replaced")));
    }
}
