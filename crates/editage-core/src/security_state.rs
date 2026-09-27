//! The Security Inspector's content, derived from real state.
//!
//! Anything a frontend claims about sensitive state must come from here, and
//! everything here is computed from the same [`DocumentSession`] that
//! controls behaviour. There is no separate set of presentation flags and no
//! fixed reassuring text: if a row says "Retained", a passphrase is retained.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::clipboard::{ClipboardClearPolicy, ClipboardStatus};
use crate::diagnostics::group_digits;
use crate::document::{
    DocumentSession, DocumentState, ExternalChangeStatus, FileBinding, PassphrasePolicy,
    PlaintextPresence, UndoHistory,
};
use crate::preferences::Preferences;
use crate::save::{save_mechanism_steps, MECHANISM_PLAINTEXT_STATEMENT};
use crate::storage::{DirectoryDurability, FileDurability, OpenNotice, ReadOnlyReason};

#[derive(Debug, Clone, PartialEq)]
pub struct SecurityReport {
    pub document_name: String,
    pub sections: Vec<ReportSection>,
    /// The numbered "How saving works" steps.
    pub save_mechanism: Vec<&'static str>,
    pub save_mechanism_footer: &'static str,
    pub actions: Vec<InspectorAction>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportSection {
    pub title: &'static str,
    pub rows: Vec<ReportRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReportRow {
    pub label: &'static str,
    pub value: ReportValue,
    /// Rows that describe a problem or something the user should act on.
    pub needs_attention: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReportValue {
    Text(String),
    /// A path; frontends may abbreviate the home folder.
    Path(PathBuf),
    /// Text followed by a time, e.g. "Saved locally at" + 1:42 PM. The
    /// frontend formats the time in the user's locale.
    TextWithTime {
        text: String,
        at: SystemTime,
    },
}

/// An action the inspector offers, with whether it applies right now.
#[derive(Debug, Clone, PartialEq)]
pub enum InspectorAction {
    RevealEncryptedFile { enabled: bool },
    CopyFilePath { enabled: bool },
    LockDocument { enabled: bool },
    ForgetRetainedPassphrase { enabled: bool },
    ClearClipboardNow { enabled: bool },
    ViewDiagnostics,
    RevealStagingFile { path: PathBuf },
    RetryCleanup,
}

/// Facts the inspector needs that are owned by the application rather than
/// by the document.
#[derive(Debug, Clone, Copy)]
pub struct ApplicationFacts<'a> {
    pub preferences: &'a Preferences,
    /// Clipboard status as computed by the application's `ClipboardTracker`.
    pub clipboard: ClipboardStatus,
    /// Name of the platform's spelling service, e.g. "the macOS spelling
    /// service on this Mac".
    pub spelling_service_description: &'a str,
    /// Staging files that currently exist beside the document, as found by
    /// `storage::find_staging_files` (empty for a never-saved document).
    pub staging_files_on_disk: &'a [PathBuf],
    /// How the platform's find feature handles search text, if it shares it
    /// outside the application (macOS: the system find pasteboard).
    pub find_text_description: Option<&'a str>,
}

fn row(label: &'static str, text: impl Into<String>) -> ReportRow {
    ReportRow {
        label,
        value: ReportValue::Text(text.into()),
        needs_attention: false,
    }
}

fn attention(label: &'static str, text: impl Into<String>) -> ReportRow {
    ReportRow {
        label,
        value: ReportValue::Text(text.into()),
        needs_attention: true,
    }
}

fn path_row(label: &'static str, path: PathBuf) -> ReportRow {
    ReportRow {
        label,
        value: ReportValue::Path(path),
        needs_attention: false,
    }
}

fn timed(label: &'static str, text: impl Into<String>, at: SystemTime) -> ReportRow {
    ReportRow {
        label,
        value: ReportValue::TextWithTime {
            text: text.into(),
            at,
        },
        needs_attention: false,
    }
}

/// Builds the inspector report for one document.
pub fn inspect_document_state(
    session: &DocumentSession,
    facts: ApplicationFacts<'_>,
) -> SecurityReport {
    let unlocked = session.is_unlocked();
    let mut sections = Vec::new();

    // DOCUMENT
    let mut document = vec![row("File", session.display_name())];
    match session.binding() {
        FileBinding::NotYetSaved => document.push(row("Path", "Not saved yet")),
        FileBinding::OnDisk(file) => {
            document.push(path_row("Path", file.path.clone()));
            if let Some(link) = &file.opened_via {
                document.push(path_row("Opened through link", link.clone()));
            }
        }
    }
    document.push(row("Format", session.format().display_name()));
    document.push(row("Protection", session.format().protection_description()));
    document.push(row(
        "Key derivation",
        session.format().key_derivation_description(),
    ));
    document.push(row(
        "Saving strategy",
        "Encrypted staging file + atomic replacement",
    ));
    sections.push(ReportSection {
        title: "Document",
        rows: document,
    });

    // ON DISK
    let mut on_disk = Vec::new();
    match session.binding() {
        FileBinding::NotYetSaved => {
            on_disk.push(row("Encrypted document", "Not created yet"));
            on_disk.push(row("Plaintext application files", "None"));
        }
        FileBinding::OnDisk(file) => {
            on_disk.push(match session.external_change() {
                ExternalChangeStatus::Missing { .. } => attention(
                    "Original file",
                    "No longer at this path (moved, deleted or replaced)",
                ),
                ExternalChangeStatus::Changed { .. } => attention(
                    "Original file",
                    format!(
                        "Encrypted, but changed by another program since this app last read it (was {} bytes)",
                        group_digits(file.ciphertext_bytes)
                    ),
                ),
                _ => row(
                    "Original file",
                    format!("Encrypted ({} bytes)", group_digits(file.ciphertext_bytes)),
                ),
            });
            on_disk.push(row("Plaintext application files", "None"));
            if let Some(folder) = file.path.parent() {
                on_disk.push(path_row("Save staging directory", folder.to_owned()));
            }
            on_disk.push(staging_row(session, facts.staging_files_on_disk));
            on_disk.push(external_change_row(session.external_change()));
            for notice in &file.notices {
                if let Some(notice_row) = notice_row(notice) {
                    on_disk.push(notice_row);
                }
            }
            if let Some(filesystem) = &file.volume.filesystem_type {
                on_disk.push(row("Volume format", filesystem.clone()));
            }
        }
    }
    sections.push(ReportSection {
        title: "On disk",
        rows: on_disk,
    });

    // IN MEMORY
    let mut in_memory = Vec::new();
    match session.plaintext_presence() {
        PlaintextPresence::InEditor { bytes } => {
            in_memory.push(row(
                "Plaintext document",
                format!(
                    "Present in application memory — this document is currently unlocked ({} bytes)",
                    group_digits(bytes as u64)
                ),
            ));
        }
        PlaintextPresence::NoPlaintext => {
            in_memory.push(row("Plaintext document", "No active editor buffer"));
        }
    }
    in_memory.push(row("Written to application-managed files", "No"));
    in_memory.push(row(
        "Undo history",
        match session.undo_history() {
            UndoHistory::MayContainPlaintext if unlocked => {
                "Present in memory while document is unlocked"
            }
            UndoHistory::Empty if unlocked => "Empty",
            UndoHistory::Cleared => "Cleared",
            _ => "Empty",
        },
    ));
    in_memory.push(passphrase_row(session));
    in_memory.push(clipboard_row(session, &facts));
    in_memory.push(row(
        "Spelling checker",
        if !unlocked {
            "Not in use (document locked)".to_owned()
        } else if facts.preferences.check_spelling_while_typing
            && !facts.preferences.spell_checking_active(match session.plaintext_presence() {
                PlaintextPresence::InEditor { bytes } => bytes,
                PlaintextPresence::NoPlaintext => 0,
            })
        {
            "Paused for this document: it is larger than 1 MB, which the spelling service cannot check without stalling the editor".to_owned()
        } else if facts.preferences.check_spelling_while_typing {
            format!(
                "On — text is checked by {}",
                facts.spelling_service_description
            )
        } else {
            "Off".to_owned()
        },
    ));
    if let Some(description) = facts.find_text_description {
        in_memory.push(row("Find text", description));
    }
    sections.push(ReportSection {
        title: "In memory",
        rows: in_memory,
    });

    // DOCUMENT STATE
    let mut state = vec![row("State", state_text(session.state()))];
    state.push(row(
        "Modified",
        if session.has_unsaved_changes() {
            "Yes"
        } else {
            "No"
        },
    ));
    match session.last_save() {
        Some(record) => {
            state.push(timed(
                "Last successful encrypted save",
                "Saved locally at",
                record.at,
            ));
            state.push(durability_row(
                &record.file_durability,
                &record.directory_durability,
            ));
        }
        None => state.push(row(
            "Last successful encrypted save",
            "None in this session",
        )),
    }
    if let Some(failure) = session.last_failure() {
        state.push(ReportRow {
            label: "Last save attempt",
            value: ReportValue::TextWithTime {
                text: format!("Failed at stage “{}” —", failure.stage.display_name()),
                at: failure.failed_at,
            },
            needs_attention: true,
        });
    }
    state.push(auto_lock_row(session, facts.preferences));
    sections.push(ReportSection {
        title: "Document state",
        rows: state,
    });

    SecurityReport {
        document_name: session.display_name().to_owned(),
        sections,
        save_mechanism: save_mechanism_steps(),
        save_mechanism_footer: MECHANISM_PLAINTEXT_STATEMENT,
        actions: actions(session, &facts),
    }
}

fn staging_row(session: &DocumentSession, on_disk: &[PathBuf]) -> ReportRow {
    let leftovers = session.leftover_staging_files();
    let mut lines: Vec<String> = Vec::new();
    for leftover in leftovers {
        let context = if leftover.save_committed {
            "left after a successful save"
        } else {
            "left after a failed save"
        };
        lines.push(format!("{} — {context}", leftover.path.display()));
    }
    for path in on_disk {
        if leftovers.iter().any(|leftover| &leftover.path == path) {
            continue;
        }
        let context = if session.is_saving() {
            "being written by the current save"
        } else {
            "found beside the document with this app's staging-file name; possibly left by an interrupted save"
        };
        lines.push(format!("{} — {context}", path.display()));
    }
    if lines.is_empty() {
        return row("Staging files", "None found beside the document");
    }
    let saving_only = session.is_saving() && leftovers.is_empty();
    ReportRow {
        label: "Staging files",
        value: ReportValue::Text(format!(
            "{} (this app writes only encrypted data to staging files)",
            lines.join("; ")
        )),
        needs_attention: !saving_only,
    }
}

fn external_change_row(status: &ExternalChangeStatus) -> ReportRow {
    match status {
        ExternalChangeStatus::NotChecked => row("Disk file changed externally", "Not checked yet"),
        ExternalChangeStatus::Unchanged { checked_at } => timed(
            "Disk file changed externally",
            "No — last checked",
            *checked_at,
        ),
        ExternalChangeStatus::Changed { detected_at } => ReportRow {
            label: "Disk file changed externally",
            value: ReportValue::TextWithTime {
                text: "Yes — saving is blocked until you reload or use Save As. Detected"
                    .to_owned(),
                at: *detected_at,
            },
            needs_attention: true,
        },
        ExternalChangeStatus::Missing { detected_at } => ReportRow {
            label: "Disk file changed externally",
            value: ReportValue::TextWithTime {
                text: "The file is no longer at this path. Detected".to_owned(),
                at: *detected_at,
            },
            needs_attention: true,
        },
        ExternalChangeStatus::CheckFailed { reason, .. } => attention(
            "Disk file changed externally",
            format!("Could not check: {reason}"),
        ),
    }
}

fn notice_row(notice: &OpenNotice) -> Option<ReportRow> {
    match notice {
        OpenNotice::OpenedThroughSymbolicLink { .. } => None, // shown as "Opened through link"
        OpenNotice::MultipleHardLinks { count } => Some(attention(
            "Hard links",
            format!("{count} links — saving replaces only this one; the others keep the previous encrypted version"),
        )),
        OpenNotice::LargeDocument { .. } => None,
        OpenNotice::ReadOnly { reason } => Some(attention(
            "Writable",
            match reason {
                ReadOnlyReason::FilePermissions => "No — the file is read-only; saving is disabled",
                ReadOnlyReason::ReadOnlyVolume => "No — the disk is read-only; saving is disabled",
            },
        )),
        OpenNotice::NetworkFilesystem { filesystem_type } => Some(attention(
            "Network volume",
            format!("{filesystem_type} — atomic replacement and flushing depend on the server"),
        )),
    }
}

fn passphrase_row(session: &DocumentSession) -> ReportRow {
    let text = if session.is_never_saved() {
        "None yet. You will create one when you first save."
    } else if session.has_retained_passphrase() {
        "Retained in application memory until document locks or closes."
    } else if session.is_unlocked() {
        "Not retained after unlock. You will be asked again when saving."
    } else {
        "Not held. It is asked for only to unlock."
    };
    row("Passphrase", text)
}

fn clipboard_row(session: &DocumentSession, facts: &ApplicationFacts<'_>) -> ReportRow {
    match facts.clipboard {
        ClipboardStatus::ContainsCopiedText(item) if item.source_document == session.id() => {
            match item.clear_at {
                Some(deadline) => ReportRow {
                    label: "Clipboard",
                    value: ReportValue::TextWithTime {
                        text: "Contains text copied from this document. It will be cleared if unchanged at"
                            .to_owned(),
                        at: deadline,
                    },
                    needs_attention: true,
                },
                None => attention(
                    "Clipboard",
                    "Contains text copied from this document. Automatic clearing is off.",
                ),
            }
        }
        ClipboardStatus::ContainsCopiedText(_) => {
            row("Clipboard", "Contains text copied from another document in this app")
        }
        ClipboardStatus::NotTracked => row(
            "Clipboard",
            match facts.preferences.clipboard_clear {
                ClipboardClearPolicy::Never => {
                    "No text copied by this application is currently tracked".to_owned()
                }
                policy => format!(
                    "No text copied by this application is currently tracked. Copied text is cleared {}.",
                    policy.label().to_lowercase()
                ),
            },
        ),
    }
}

fn state_text(state: DocumentState) -> String {
    match state {
        DocumentState::Locked => "Locked".to_owned(),
        DocumentState::Unlocking => "Unlocking".to_owned(),
        DocumentState::UnlockedClean | DocumentState::UnlockedModified => "Unlocked".to_owned(),
        DocumentState::Saving(stage) => format!("Saving — {}", stage.display_name()),
        DocumentState::SaveFailed => "Unlocked — last save failed".to_owned(),
        DocumentState::Locking => "Locking".to_owned(),
        DocumentState::Closed => "Closed".to_owned(),
    }
}

fn durability_row(file: &FileDurability, directory: &DirectoryDurability) -> ReportRow {
    let file_text = match file {
        FileDurability::FullFsync => "flushed with F_FULLFSYNC".to_owned(),
        FileDurability::FsyncOnly { full_fsync_refused } => {
            format!("flushed with fsync only (F_FULLFSYNC refused: {full_fsync_refused})")
        }
        FileDurability::Fsync => "flushed with fsync".to_owned(),
    };
    match directory {
        DirectoryDurability::Synced => row(
            "Last save durability",
            format!("Staging file {file_text}; folder flushed after replacement"),
        ),
        DirectoryDurability::SyncFailed { error } => attention(
            "Last save durability",
            format!("Staging file {file_text}; folder flush failed after replacement: {error}"),
        ),
    }
}

fn auto_lock_row(session: &DocumentSession, preferences: &Preferences) -> ReportRow {
    match preferences.lock_after_inactivity_minutes {
        None => row("Auto-lock", "Off"),
        Some(_) if !session.is_unlocked() => {
            row("Auto-lock", "Not active while the document is locked")
        }
        Some(_) if session.is_never_saved() => {
            row("Auto-lock", "Not applicable until the document is saved")
        }
        Some(minutes) if session.auto_lock_postponed() => attention(
            "Auto-lock",
            format!(
                "Postponed: unsaved changes (after {} of inactivity)",
                minutes_text(minutes)
            ),
        ),
        Some(minutes) => row(
            "Auto-lock",
            format!("After {} of inactivity", minutes_text(minutes)),
        ),
    }
}

pub(crate) fn minutes_text(minutes: u32) -> String {
    if minutes == 1 {
        "1 minute".to_owned()
    } else {
        format!("{minutes} minutes")
    }
}

fn actions(session: &DocumentSession, facts: &ApplicationFacts<'_>) -> Vec<InspectorAction> {
    let on_disk = session.path().is_some();
    let clipboard_from_this_document = matches!(
        facts.clipboard,
        ClipboardStatus::ContainsCopiedText(item) if item.source_document == session.id()
    );
    let mut actions = vec![
        InspectorAction::RevealEncryptedFile { enabled: on_disk },
        InspectorAction::CopyFilePath { enabled: on_disk },
        InspectorAction::LockDocument {
            enabled: session.is_unlocked() && !session.is_never_saved() && !session.is_saving(),
        },
        InspectorAction::ForgetRetainedPassphrase {
            enabled: session.has_retained_passphrase()
                && session.passphrase_policy() == PassphrasePolicy::KeepUntilLocked,
        },
        InspectorAction::ClearClipboardNow {
            enabled: clipboard_from_this_document,
        },
        InspectorAction::ViewDiagnostics,
    ];
    let first_staging = session
        .leftover_staging_files()
        .first()
        .map(|leftover| leftover.path.clone())
        .or_else(|| facts.staging_files_on_disk.first().cloned());
    if let Some(path) = first_staging {
        if !session.is_saving() {
            actions.push(InspectorAction::RevealStagingFile { path });
        }
    }
    if !session.leftover_staging_files().is_empty() {
        actions.push(InspectorAction::RetryCleanup);
    }
    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::DiagnosticLog;

    fn value_of<'a>(report: &'a SecurityReport, label: &str) -> &'a ReportValue {
        report
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| row.label == label)
            .map(|row| &row.value)
            .unwrap_or_else(|| panic!("no row {label}"))
    }

    #[test]
    fn a_new_document_reports_that_no_encrypted_file_exists_yet() {
        let session =
            DocumentSession::new_untitled(PassphrasePolicy::KeepUntilLocked, DiagnosticLog::new());
        let preferences = Preferences::default();
        let report = inspect_document_state(
            &session,
            ApplicationFacts {
                preferences: &preferences,
                clipboard: ClipboardStatus::NotTracked,
                spelling_service_description: "the system spelling service",
                staging_files_on_disk: &[],
                find_text_description: None,
            },
        );
        assert_eq!(
            value_of(&report, "Encrypted document"),
            &ReportValue::Text("Not created yet".to_owned())
        );
        assert_eq!(
            value_of(&report, "Plaintext application files"),
            &ReportValue::Text("None".to_owned())
        );
        assert_eq!(
            value_of(&report, "Passphrase"),
            &ReportValue::Text("None yet. You will create one when you first save.".to_owned())
        );
    }
}
