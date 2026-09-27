//! The presentation boundary: the one place structured errors and notices
//! become sentences.
//!
//! Every frontend renders these reports as-is, so the wording (and its
//! honesty) is shared and reviewed in one place. Reports never include
//! plaintext or passphrases; details come only from the structured error.

use std::io;
use std::path::{Path, PathBuf};

use crate::document::{DocumentSession, LockReason};
use crate::error::EditorError;
use crate::save::{OriginalFileState, SaveFailure, SaveStage};
use crate::storage::{OpenNotice, ReadOnlyReason};

/// A failure explained for people: a short message first, details on request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureReport {
    pub title: String,
    /// Short explanation shown without expanding details.
    pub message: Vec<String>,
    /// Label/value rows shown under "Show Details".
    pub details: Vec<(String, String)>,
    pub actions: Vec<FailureAction>,
}

impl FailureReport {
    /// Everything as plain text, for "Copy Details".
    pub fn as_plain_text(&self) -> String {
        let mut text = self.title.clone();
        text.push('\n');
        for line in &self.message {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        for (label, value) in &self.details {
            text.push('\n');
            text.push_str(label);
            text.push_str(": ");
            text.push_str(value);
        }
        text.push('\n');
        text
    }
}

/// Next steps a failure sheet offers. Frontends map each to a button in their
/// platform's usual order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureAction {
    TryAgain,
    SaveAs,
    ReloadFromDisk,
    RevealStagingFile(PathBuf),
    RetryCleanup,
    Cancel,
    Dismiss,
}

fn quoted_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    format!("“{name}”")
}

fn yes_no(value: bool) -> String {
    if value { "Yes" } else { "No" }.to_owned()
}

/// A one-sentence explanation of an OS error, in terms of the document.
fn describe_io_cause(error: &io::Error, document: &Path) -> String {
    let name = quoted_name(document);
    match error.raw_os_error() {
        Some(libc::EROFS) => format!("The disk containing {name} is not writable."),
        Some(libc::EACCES) | Some(libc::EPERM) => {
            format!("You don’t have permission to write in the folder containing {name}.")
        }
        Some(libc::ENOSPC) | Some(libc::EDQUOT) => format!("The disk containing {name} is full."),
        Some(libc::ENOENT) => format!("The folder containing {name} is no longer available."),
        Some(libc::EXDEV) => {
            "The staging file and the document are on different disks, so they cannot be swapped in one step."
                .to_owned()
        }
        Some(libc::EIO) => format!("The disk containing {name} reported an input/output error."),
        _ => match error.kind() {
            io::ErrorKind::PermissionDenied => {
                format!("You don’t have permission to write in the folder containing {name}.")
            }
            _ => format!("The system reported: {error}"),
        },
    }
}

fn system_error_text(error: &EditorError) -> String {
    match error.io_error() {
        Some(io_error) => io_error.to_string(),
        None => {
            let mut text = error.to_string();
            let mut source = std::error::Error::source(error);
            while let Some(cause) = source {
                text.push_str(": ");
                text.push_str(&cause.to_string());
                source = cause.source();
            }
            text
        }
    }
}

/// Explains a failed save.
pub fn save_failure_report(failure: &SaveFailure, session: &DocumentSession) -> FailureReport {
    let destination = &failure.destination;
    let name = quoted_name(destination);
    let edits_in_memory = session.is_unlocked();
    let staging_left = failure
        .staging_file
        .as_ref()
        .filter(|staging| staging.still_exists)
        .map(|staging| staging.path.clone());

    let (title, mut message, mut actions) = match &failure.error {
        EditorError::ExternalModification { .. } => (
            format!("{name} changed on disk after you opened it."),
            vec![
                "Saving now could overwrite a newer copy.".to_owned(),
                "Nothing was written. Your edits are still open in memory.".to_owned(),
            ],
            vec![FailureAction::ReloadFromDisk, FailureAction::SaveAs, FailureAction::Cancel],
        ),
        EditorError::DestinationMissing { .. } => (
            format!("{name} is no longer at its original location."),
            vec![
                "It may have been moved, renamed or deleted, or its disk may have been disconnected. \
                 Nothing was written. Your edits are still open in memory."
                    .to_owned(),
            ],
            vec![FailureAction::SaveAs, FailureAction::Cancel],
        ),
        EditorError::PassphraseMismatch => (
            "The password you entered does not match this document’s password.".to_owned(),
            vec!["Nothing was written. Your edits are still open in memory.".to_owned()],
            vec![FailureAction::TryAgain, FailureAction::Cancel],
        ),
        EditorError::NotARegularFile { .. } => (
            "The document could not be saved.".to_owned(),
            vec![
                format!("{name} has been replaced by a folder, a link or another non-file item, so it was not overwritten."),
                "Nothing was written. Your edits are still open in memory.".to_owned(),
            ],
            vec![FailureAction::SaveAs, FailureAction::Cancel],
        ),
        EditorError::DestinationReadOnly { .. } => (
            "The document could not be saved.".to_owned(),
            vec![
                "Your current edits are still open in memory. The encrypted file on disk was not replaced."
                    .to_owned(),
                format!("{name} is read-only."),
            ],
            vec![FailureAction::SaveAs, FailureAction::Cancel],
        ),
        other => {
            let mut message = vec![match failure.original {
                OriginalFileState::DidNotExist => {
                    "Your current edits are still open in memory. No encrypted file was created."
                        .to_owned()
                }
                _ => "Your current edits are still open in memory. The encrypted file on disk was not replaced."
                    .to_owned(),
            }];
            if let EditorError::Permissions { .. } = other {
                message.push(
                    "The encrypted staging file was complete, but its permissions could not be set to match the original file, so the original was not replaced."
                        .to_owned(),
                );
            } else if failure.stage == SaveStage::ReplacingOriginal && other.io_error().is_some() {
                message.insert(
                    0,
                    "Encryption completed successfully, but the encrypted staging file could not replace the existing file."
                        .to_owned(),
                );
            }
            if let (Some(io_error), false) =
                (other.io_error(), matches!(other, EditorError::Permissions { .. }))
            {
                message.push(describe_io_cause(io_error, destination));
            }
            (
                "The document could not be saved.".to_owned(),
                message,
                vec![FailureAction::SaveAs, FailureAction::Cancel, FailureAction::TryAgain],
            )
        }
    };

    if let Some(path) = &staging_left {
        message.push(format!(
            "An encrypted staging file was left beside the document and could not be removed. It contains encrypted data only: {}",
            path.display()
        ));
        actions.insert(
            actions.len().saturating_sub(1),
            FailureAction::RevealStagingFile(path.clone()),
        );
    }

    let mut details = vec![
        (
            "Failure stage".to_owned(),
            failure.stage.display_name().to_owned(),
        ),
        ("Destination".to_owned(), destination.display().to_string()),
    ];
    match &failure.staging_file {
        Some(staging) => {
            let status = if staging.still_exists {
                "still on disk (encrypted data only)"
            } else if staging.removal_attempted {
                "removed"
            } else {
                "not removed"
            };
            details.push((
                "Encrypted staging file".to_owned(),
                format!("{} — {status}", staging.path.display()),
            ));
            if let Some(error) = &staging.removal_error {
                details.push(("Cleanup error".to_owned(), system_error_text(error)));
            }
        }
        None => details.push((
            "Encrypted staging file".to_owned(),
            "None was created".to_owned(),
        )),
    }
    details.push((
        "Original preserved".to_owned(),
        match failure.original {
            OriginalFileState::Unchanged => "Yes — unchanged by this application".to_owned(),
            OriginalFileState::DidNotExist => "No file existed at this location".to_owned(),
            OriginalFileState::ChangedByAnotherProgram => {
                "Not modified by this application (it was changed by something else)".to_owned()
            }
        },
    ));
    details.push((
        "Plaintext edits still in memory".to_owned(),
        yes_no(edits_in_memory),
    ));
    details.push(("System error".to_owned(), system_error_text(&failure.error)));

    FailureReport {
        title,
        message,
        details,
        actions,
    }
}

/// Explains a committed save whose staging file could not be removed.
pub fn cleanup_warning_report(staging_path: &Path, error_text: &str) -> FailureReport {
    FailureReport {
        title: "Your document was saved successfully.".to_owned(),
        message: vec![
            "The encrypted staging file could not be removed:".to_owned(),
            staging_path.display().to_string(),
            "The staging file contains encrypted data only.".to_owned(),
        ],
        details: vec![
            (
                "Staging file".to_owned(),
                staging_path.display().to_string(),
            ),
            ("System error".to_owned(), error_text.to_owned()),
        ],
        actions: vec![
            FailureAction::RevealStagingFile(staging_path.to_owned()),
            FailureAction::RetryCleanup,
            FailureAction::Dismiss,
        ],
    }
}

/// The short line shown inside the unlock sheet when unlocking fails.
pub fn unlock_failure_message(error: &EditorError) -> String {
    match error {
        EditorError::AuthenticationFailed { .. } => {
            "The password is incorrect, or this file could not be decrypted.".to_owned()
        }
        EditorError::InvalidUtf8 { .. } => {
            "The file was decrypted successfully, but its contents are not valid UTF-8 text.".to_owned()
        }
        EditorError::Decryption { .. } => {
            "The password was accepted, but the encrypted contents are damaged or incomplete.".to_owned()
        }
        EditorError::InvalidAgeFile { .. } => {
            "The file’s age header is damaged or invalid, so it could not be decrypted.".to_owned()
        }
        EditorError::ExcessiveWorkFactor { .. } => {
            "The file asks for far more password-hashing work than this Mac allows, so it was not attempted."
                .to_owned()
        }
        EditorError::FileOpen { .. } | EditorError::FileRead { .. } => {
            "The encrypted file could not be read from disk.".to_owned()
        }
        other => format!("The file could not be decrypted: {other}."),
    }
}

/// Technical detail rows for an unlock or open failure ("Show Details").
pub fn error_details(error: &EditorError) -> Vec<(String, String)> {
    let mut details = Vec::new();
    match error {
        EditorError::InvalidUtf8 {
            valid_up_to,
            byte_len,
        } => {
            details.push((
                "First invalid byte".to_owned(),
                format!("offset {valid_up_to} of {byte_len}"),
            ));
            details.push(("Encrypted file".to_owned(), "Unchanged".to_owned()));
        }
        EditorError::AuthenticationFailed { .. } => details.push((
            "Why this is uncertain".to_owned(),
            "With passphrase encryption, a wrong password and a damaged key stanza fail in the same way."
                .to_owned(),
        )),
        _ => {}
    }
    details.push(("Error".to_owned(), system_error_text(error)));
    details
}

/// Explains why a file could not be opened (before any password is asked).
pub fn open_failure_report(path: &Path, error: &EditorError) -> FailureReport {
    let name = quoted_name(path);
    let message = match error {
        EditorError::CloudPlaceholder { .. } => vec![
            "The file is stored in the cloud and has not been downloaded to this Mac.".to_owned(),
            "Download it first (for example with “Download Now” in Finder), then open it again."
                .to_owned(),
        ],
        EditorError::FileTooLarge { bytes, limit, .. } => vec![format!(
            "It is {} MB. Documents larger than {} MB cannot be opened in this version.",
            bytes / 1_000_000,
            limit / 1_000_000
        )],
        EditorError::UnsupportedFormat { detected } => vec![format!(
            "It is not an age encrypted file. Detected: {detected}."
        )],
        EditorError::UnsupportedProtection { description } => vec![description.clone()],
        EditorError::InvalidAgeFile { .. } => {
            vec!["It looks like an age file, but its header is damaged or invalid.".to_owned()]
        }
        EditorError::NotARegularFile { .. } => vec!["It is not a regular file.".to_owned()],
        EditorError::FileOpen { source, .. } | EditorError::FileRead { source, .. } => {
            vec![format!("The system reported: {source}")]
        }
        other => vec![other.to_string()],
    };
    FailureReport {
        title: format!("{name} could not be opened."),
        message,
        details: error_details(error),
        actions: vec![FailureAction::Dismiss],
    }
}

/// Text for a notice that needs confirmation before unlocking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeText {
    pub title: String,
    pub message: Vec<String>,
    pub confirm_button: &'static str,
}

pub fn open_notice_text(notice: &OpenNotice) -> NoticeText {
    match notice {
        OpenNotice::OpenedThroughSymbolicLink { target, .. } => NoticeText {
            title: "This document was opened through a symbolic link.".to_owned(),
            message: vec![
                "The application will save to the resolved target:".to_owned(),
                target.display().to_string(),
            ],
            confirm_button: "Continue",
        },
        OpenNotice::MultipleHardLinks { count } => NoticeText {
            title: format!("This file has {count} hard links."),
            message: vec![
                "Saving replaces this file with a new encrypted file. The other links will keep pointing to the previous encrypted version."
                    .to_owned(),
            ],
            confirm_button: "Continue",
        },
        OpenNotice::LargeDocument { bytes } => NoticeText {
            title: "This encrypted document will decrypt to a large text buffer.".to_owned(),
            message: vec![format!(
                "It is {:.1} MB on disk. Continue opening it?",
                *bytes as f64 / 1_000_000.0
            )],
            confirm_button: "Continue",
        },
        OpenNotice::ReadOnly { reason } => NoticeText {
            title: "This document is read-only.".to_owned(),
            message: vec![match reason {
                ReadOnlyReason::FilePermissions => "You can read it, but saving is disabled because the file is read-only.".to_owned(),
                ReadOnlyReason::ReadOnlyVolume => "You can read it, but saving is disabled because its disk is read-only.".to_owned(),
            }],
            confirm_button: "OK",
        },
        OpenNotice::NetworkFilesystem { filesystem_type } => NoticeText {
            title: "This document is on a network volume.".to_owned(),
            message: vec![format!(
                "Atomic replacement and flushing to disk depend on the {filesystem_type} server."
            )],
            confirm_button: "OK",
        },
    }
}

/// The explanation shown above the password field on the unlock sheet.
pub fn lock_reason_text(reason: LockReason) -> Option<String> {
    match reason {
        LockReason::NotYetUnlocked => None,
        LockReason::LockedByUser => Some(
            "Locked. The decrypted text was removed from the editor, its undo history was cleared, and any retained password was released."
                .to_owned(),
        ),
        LockReason::Inactivity { minutes } => Some(format!(
            "Locked after {} of inactivity. The decrypted text was removed from the editor, its undo history was cleared, and any retained password was released.",
            crate::security_state::minutes_text(minutes)
        )),
    }
}

/// The external-change conflict shown when a focus check finds a change.
pub fn external_change_report(session: &DocumentSession) -> FailureReport {
    let name = format!("“{}”", session.display_name());
    if matches!(
        session.external_change(),
        crate::document::ExternalChangeStatus::Missing { .. }
    ) {
        return FailureReport {
            title: format!("{name} is no longer at its original location."),
            message: vec![
                "It may have been moved, renamed, deleted or replaced by a folder or link, or its disk may have been disconnected. Saving to this path is blocked so that nothing is recreated there without your choice."
                    .to_owned(),
                if session.has_unsaved_changes() {
                    "Your unsaved edits are still open in memory. Use Save As… to save them.".to_owned()
                } else {
                    "The text is still open in memory. Use Save As… to save it elsewhere.".to_owned()
                },
            ],
            details: session
                .path()
                .map(|path| vec![("Original path".to_owned(), path.display().to_string())])
                .unwrap_or_default(),
            actions: vec![FailureAction::SaveAs, FailureAction::Cancel],
        };
    }
    FailureReport {
        title: format!("{name} changed on disk while it was open."),
        message: vec![
            "Saving now could overwrite a newer copy, so saving to this file is blocked."
                .to_owned(),
            if session.has_unsaved_changes() {
                "You can reload the version on disk (your unsaved edits here would be discarded), or save your version as a separate file.".to_owned()
            } else {
                "You can reload the version on disk (you have no unsaved edits here), or save this window's text as a separate file.".to_owned()
            },
        ],
        details: session
            .path()
            .map(|path| vec![("File".to_owned(), path.display().to_string())])
            .unwrap_or_default(),
        actions: vec![
            FailureAction::ReloadFromDisk,
            FailureAction::SaveAs,
            FailureAction::Cancel,
        ],
    }
}
