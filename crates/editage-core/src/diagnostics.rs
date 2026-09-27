//! A bounded, in-memory history of operational events.
//!
//! Nothing here is written to disk. Events are typed so that they *cannot*
//! carry plaintext, passphrases, clipboard contents, selections or search
//! strings: no variant has a field that could hold them. File paths and byte
//! counts are recorded; they are metadata the user can already see.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use crate::save::SaveStage;

/// How many events are kept. Older events are discarded.
pub const DIAGNOSTIC_HISTORY_LIMIT: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticEvent {
    OpenRequested { path: PathBuf },
    CiphertextRead { bytes: usize },
    FormatRecognised { format: &'static str },
    OpenFailed { reason: String },
    UnlockRequested,
    DecryptionSucceeded,
    DecryptionFailed { reason: String },
    PlaintextBufferCreated { bytes: usize },
    PassphraseRetained,
    PassphraseNotRetained,
    PassphraseReleased,
    SaveRequested,
    SaveQueuedBehindRunningSave,
    SaveStageStarted(SaveStage),
    StagingFileCreated { path: PathBuf },
    AtomicReplacementSucceeded,
    SaveCompleted { ciphertext_bytes: usize },
    SaveFailed { stage: SaveStage, reason: String },
    CleanupFailed { path: PathBuf },
    StagingFileRemovedOnRetry { path: PathBuf },
    ExternalChangeDetected,
    ExternalFileMissing,
    ExternalChangeResolved,
    ExternalCheckFoundNoChange,
    Locked,
    AutoLockPostponedUnsavedChanges,
    EditorBufferReleased,
    UndoHistoryCleared,
    Reloaded,
    Closed,
    ClipboardWritten,
    ClipboardClearedAutomatically,
    ClipboardClearedByUser,
    ClipboardLeftAloneChangedElsewhere,
}

impl DiagnosticEvent {
    /// One line of human-readable text.
    pub fn describe(&self) -> String {
        match self {
            DiagnosticEvent::OpenRequested { path } => {
                format!("Open requested: {}", path.display())
            }
            DiagnosticEvent::CiphertextRead { bytes } => {
                format!("Read {} bytes ciphertext", group_digits(*bytes as u64))
            }
            DiagnosticEvent::FormatRecognised { format } => format!("Format recognised: {format}"),
            DiagnosticEvent::OpenFailed { reason } => format!("Open failed: {reason}"),
            DiagnosticEvent::UnlockRequested => "Unlock requested".to_owned(),
            DiagnosticEvent::DecryptionSucceeded => "Decryption succeeded".to_owned(),
            DiagnosticEvent::DecryptionFailed { reason } => format!("Decryption failed: {reason}"),
            DiagnosticEvent::PlaintextBufferCreated { bytes } => format!(
                "Plaintext buffer created ({} bytes, application memory)",
                group_digits(*bytes as u64)
            ),
            DiagnosticEvent::PassphraseRetained => {
                "Passphrase retained in application memory until lock or close".to_owned()
            }
            DiagnosticEvent::PassphraseNotRetained => {
                "Passphrase not retained after use".to_owned()
            }
            DiagnosticEvent::PassphraseReleased => {
                "Retained passphrase released (zeroization requested)".to_owned()
            }
            DiagnosticEvent::SaveRequested => "Save requested".to_owned(),
            DiagnosticEvent::SaveQueuedBehindRunningSave => {
                "Save requested while saving; one more save queued".to_owned()
            }
            DiagnosticEvent::SaveStageStarted(stage) => {
                format!("Save stage: {}", stage.display_name())
            }
            DiagnosticEvent::StagingFileCreated { path } => {
                format!("Staging file created: {}", path.display())
            }
            DiagnosticEvent::AtomicReplacementSucceeded => {
                "Atomic replacement succeeded".to_owned()
            }
            DiagnosticEvent::SaveCompleted { ciphertext_bytes } => format!(
                "Save complete ({} bytes encrypted)",
                group_digits(*ciphertext_bytes as u64)
            ),
            DiagnosticEvent::SaveFailed { stage, reason } => {
                format!("Save failed at {}: {reason}", stage.display_name())
            }
            DiagnosticEvent::CleanupFailed { path } => {
                format!("Staging file could not be removed: {}", path.display())
            }
            DiagnosticEvent::StagingFileRemovedOnRetry { path } => {
                format!("Staging file removed: {}", path.display())
            }
            DiagnosticEvent::ExternalChangeDetected => "File changed on disk externally".to_owned(),
            DiagnosticEvent::ExternalFileMissing => {
                "File no longer at its path (moved, deleted or replaced)".to_owned()
            }
            DiagnosticEvent::ExternalChangeResolved => {
                "File on disk matches this app's last version again".to_owned()
            }
            DiagnosticEvent::ExternalCheckFoundNoChange => {
                "Checked file on disk: unchanged".to_owned()
            }
            DiagnosticEvent::Locked => "Document locked".to_owned(),
            DiagnosticEvent::AutoLockPostponedUnsavedChanges => {
                "Auto-lock postponed: unsaved changes".to_owned()
            }
            DiagnosticEvent::EditorBufferReleased => "Editor buffer released".to_owned(),
            DiagnosticEvent::UndoHistoryCleared => "Undo history cleared".to_owned(),
            DiagnosticEvent::Reloaded => "Document reloaded from disk".to_owned(),
            DiagnosticEvent::Closed => "Document closed".to_owned(),
            DiagnosticEvent::ClipboardWritten => "Text copied to the clipboard".to_owned(),
            DiagnosticEvent::ClipboardClearedAutomatically => {
                "Clipboard cleared automatically".to_owned()
            }
            DiagnosticEvent::ClipboardClearedByUser => "Clipboard cleared".to_owned(),
            DiagnosticEvent::ClipboardLeftAloneChangedElsewhere => {
                "Clipboard not cleared: it now holds content from elsewhere".to_owned()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticEntry {
    pub at: SystemTime,
    /// The document's file name, or "Untitled"; `None` for app-wide events.
    pub document: Option<String>,
    pub event: DiagnosticEvent,
}

/// Shared handle to the application's diagnostic history. Cloning the handle
/// shares the same history; it is safe to use from background threads.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticLog {
    entries: Arc<Mutex<VecDeque<DiagnosticEntry>>>,
}

impl DiagnosticLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, document: &str, event: DiagnosticEvent) {
        self.push(Some(document.to_owned()), event);
    }

    pub fn record_app_event(&self, event: DiagnosticEvent) {
        self.push(None, event);
    }

    fn push(&self, document: Option<String>, event: DiagnosticEvent) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if entries.len() == DIAGNOSTIC_HISTORY_LIMIT {
            entries.pop_front();
        }
        entries.push_back(DiagnosticEntry {
            at: SystemTime::now(),
            document,
            event,
        });
    }

    pub fn entries(&self) -> Vec<DiagnosticEntry> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        entries.iter().cloned().collect()
    }

    pub fn clear(&self) {
        self.entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
    }

    /// Plain-text export for "Copy". `format_time` is supplied by the
    /// frontend so times appear in the user's locale.
    pub fn export_text(&self, format_time: &dyn Fn(SystemTime) -> String) -> String {
        let mut text = String::new();
        for entry in self.entries() {
            text.push_str(&format_time(entry.at));
            text.push(' ');
            if let Some(document) = &entry.document {
                text.push('[');
                text.push_str(document);
                text.push_str("] ");
            }
            text.push_str(&entry.event.describe());
            text.push('\n');
        }
        text
    }
}

/// Formats 4821 as "4,821".
pub fn group_digits(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded() {
        let log = DiagnosticLog::new();
        for _ in 0..(DIAGNOSTIC_HISTORY_LIMIT + 20) {
            log.record_app_event(DiagnosticEvent::SaveRequested);
        }
        assert_eq!(log.entries().len(), DIAGNOSTIC_HISTORY_LIMIT);
    }

    #[test]
    fn digits_are_grouped() {
        assert_eq!(group_digits(4821), "4,821");
        assert_eq!(group_digits(12), "12");
        assert_eq!(group_digits(1_234_567), "1,234,567");
    }
}
