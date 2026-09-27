//! User preferences and the only document metadata the application persists.
//!
//! Preferences are stored by the frontend in the platform's normal settings
//! store (on macOS, `NSUserDefaults`). They are converted to and from plain
//! string pairs here so that every frontend stores exactly the same, easily
//! inspectable keys.
//!
//! Nothing here contains plaintext or secrets. The only document-related
//! entries are file paths: the recent-documents list (when enabled) and the
//! list of documents to reopen at launch (when enabled).

use std::path::PathBuf;
use std::time::Duration;

use crate::clipboard::ClipboardClearPolicy;
use crate::document::PassphrasePolicy;

pub const RECENT_DOCUMENTS_LIMIT: usize = 10;
pub const FONT_SIZE_RANGE: std::ops::RangeInclusive<u32> = 9..=36;
pub const FONT_SIZE_CHOICES: [u32; 9] = [11, 12, 13, 14, 15, 16, 18, 20, 24];
pub const INACTIVITY_LOCK_CHOICES_MINUTES: [Option<u32>; 4] = [None, Some(5), Some(15), Some(30)];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorFont {
    SystemMonospaced,
    System,
    /// Palatino (as in the design's "Custom" choice).
    Palatino,
}

impl EditorFont {
    pub const CHOICES: [EditorFont; 3] = [
        EditorFont::SystemMonospaced,
        EditorFont::System,
        EditorFont::Palatino,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            EditorFont::SystemMonospaced => "System Monospaced",
            EditorFont::System => "System",
            EditorFont::Palatino => "Custom (Palatino)",
        }
    }

    fn key(&self) -> &'static str {
        match self {
            EditorFont::SystemMonospaced => "mono",
            EditorFont::System => "system",
            EditorFont::Palatino => "palatino",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    pub font: EditorFont,
    pub font_size: u32,
    pub wrap_lines: bool,
    pub show_toolbar: bool,
    pub check_spelling_while_typing: bool,
    pub reopen_documents_at_launch: bool,
    pub remember_recent_documents: bool,
    pub clipboard_clear: ClipboardClearPolicy,
    /// `None` means auto-lock is off.
    pub lock_after_inactivity_minutes: Option<u32>,
    /// The initial state of the "Remember password" checkbox on the unlock
    /// sheet.
    pub default_passphrase_policy: PassphrasePolicy,
    pub recent_documents: Vec<PathBuf>,
    /// Documents open at quit, reopened (locked) at the next launch. Only
    /// maintained while `reopen_documents_at_launch` is on.
    pub documents_to_reopen: Vec<PathBuf>,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            font: EditorFont::SystemMonospaced,
            font_size: 13,
            wrap_lines: true,
            show_toolbar: true,
            check_spelling_while_typing: true,
            reopen_documents_at_launch: false,
            remember_recent_documents: true,
            clipboard_clear: ClipboardClearPolicy::Never,
            lock_after_inactivity_minutes: Some(15),
            default_passphrase_policy: PassphrasePolicy::KeepUntilLocked,
            recent_documents: Vec::new(),
            documents_to_reopen: Vec::new(),
        }
    }
}

/// Every key the application writes to the preferences store.
pub mod keys {
    pub const FONT: &str = "EditorFont";
    pub const FONT_SIZE: &str = "EditorFontSize";
    pub const WRAP_LINES: &str = "WrapLines";
    pub const SHOW_TOOLBAR: &str = "ShowToolbar";
    pub const SPELLING: &str = "CheckSpellingWhileTyping";
    pub const REOPEN: &str = "ReopenDocumentsAtLaunch";
    pub const REMEMBER_RECENT: &str = "RememberRecentDocuments";
    pub const CLIPBOARD_CLEAR_SECONDS: &str = "ClearClipboardAfterSeconds";
    pub const LOCK_AFTER_MINUTES: &str = "LockAfterInactivityMinutes";
    pub const PASSPHRASE_POLICY: &str = "DefaultPassphrasePolicy";
    pub const RECENT_DOCUMENTS: &str = "RecentDocumentPaths";
    pub const DOCUMENTS_TO_REOPEN: &str = "DocumentPathsToReopen";

    pub const ALL: [&str; 12] = [
        FONT,
        FONT_SIZE,
        WRAP_LINES,
        SHOW_TOOLBAR,
        SPELLING,
        REOPEN,
        REMEMBER_RECENT,
        CLIPBOARD_CLEAR_SECONDS,
        LOCK_AFTER_MINUTES,
        PASSPHRASE_POLICY,
        RECENT_DOCUMENTS,
        DOCUMENTS_TO_REOPEN,
    ];
}

impl Preferences {
    /// Serialises to string pairs for the platform store.
    pub fn to_entries(&self) -> Vec<(&'static str, String)> {
        vec![
            (keys::FONT, self.font.key().to_owned()),
            (keys::FONT_SIZE, self.font_size.to_string()),
            (keys::WRAP_LINES, self.wrap_lines.to_string()),
            (keys::SHOW_TOOLBAR, self.show_toolbar.to_string()),
            (keys::SPELLING, self.check_spelling_while_typing.to_string()),
            (keys::REOPEN, self.reopen_documents_at_launch.to_string()),
            (
                keys::REMEMBER_RECENT,
                self.remember_recent_documents.to_string(),
            ),
            (
                keys::CLIPBOARD_CLEAR_SECONDS,
                match self.clipboard_clear {
                    ClipboardClearPolicy::Never => "0".to_owned(),
                    ClipboardClearPolicy::After(delay) => delay.as_secs().to_string(),
                },
            ),
            (
                keys::LOCK_AFTER_MINUTES,
                self.lock_after_inactivity_minutes.unwrap_or(0).to_string(),
            ),
            (
                keys::PASSPHRASE_POLICY,
                match self.default_passphrase_policy {
                    PassphrasePolicy::KeepUntilLocked => "keep-until-locked".to_owned(),
                    PassphrasePolicy::AskAgainWhenSaving => "ask-again-when-saving".to_owned(),
                },
            ),
            (keys::RECENT_DOCUMENTS, join_paths(&self.recent_documents)),
            (
                keys::DOCUMENTS_TO_REOPEN,
                join_paths(&self.documents_to_reopen),
            ),
        ]
    }

    /// Reads preferences, falling back to defaults for missing or invalid
    /// values.
    pub fn from_entries(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = Preferences::default();
        let flag = |key: &str, default: bool| match lookup(key).as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => default,
        };
        let number = |key: &str| lookup(key).and_then(|value| value.parse::<u32>().ok());

        Preferences {
            font: match lookup(keys::FONT).as_deref() {
                Some("system") => EditorFont::System,
                Some("palatino") => EditorFont::Palatino,
                Some("mono") => EditorFont::SystemMonospaced,
                _ => defaults.font,
            },
            font_size: number(keys::FONT_SIZE)
                .filter(|size| FONT_SIZE_RANGE.contains(size))
                .unwrap_or(defaults.font_size),
            wrap_lines: flag(keys::WRAP_LINES, defaults.wrap_lines),
            show_toolbar: flag(keys::SHOW_TOOLBAR, defaults.show_toolbar),
            check_spelling_while_typing: flag(keys::SPELLING, defaults.check_spelling_while_typing),
            reopen_documents_at_launch: flag(keys::REOPEN, defaults.reopen_documents_at_launch),
            remember_recent_documents: flag(
                keys::REMEMBER_RECENT,
                defaults.remember_recent_documents,
            ),
            clipboard_clear: match number(keys::CLIPBOARD_CLEAR_SECONDS) {
                Some(0) => ClipboardClearPolicy::Never,
                Some(seconds) => ClipboardClearPolicy::After(Duration::from_secs(seconds.into())),
                None => defaults.clipboard_clear,
            },
            lock_after_inactivity_minutes: match number(keys::LOCK_AFTER_MINUTES) {
                Some(0) => None,
                Some(minutes) => Some(minutes),
                None => defaults.lock_after_inactivity_minutes,
            },
            default_passphrase_policy: match lookup(keys::PASSPHRASE_POLICY).as_deref() {
                Some("ask-again-when-saving") => PassphrasePolicy::AskAgainWhenSaving,
                Some("keep-until-locked") => PassphrasePolicy::KeepUntilLocked,
                _ => defaults.default_passphrase_policy,
            },
            recent_documents: split_paths(lookup(keys::RECENT_DOCUMENTS)),
            documents_to_reopen: split_paths(lookup(keys::DOCUMENTS_TO_REOPEN)),
        }
    }

    pub fn lock_after_inactivity(&self) -> Option<Duration> {
        self.lock_after_inactivity_minutes
            .map(|minutes| Duration::from_secs(u64::from(minutes) * 60))
    }

    /// Records that a document was opened or saved, if the user allows it.
    pub fn note_recent_document(&mut self, path: PathBuf) {
        if !self.remember_recent_documents {
            return;
        }
        self.recent_documents.retain(|existing| *existing != path);
        self.recent_documents.insert(0, path);
        self.recent_documents.truncate(RECENT_DOCUMENTS_LIMIT);
    }

    /// Turning the setting off also forgets the list.
    pub fn set_remember_recent_documents(&mut self, remember: bool) {
        self.remember_recent_documents = remember;
        if !remember {
            self.recent_documents.clear();
        }
    }

    pub fn set_reopen_documents_at_launch(&mut self, reopen: bool) {
        self.reopen_documents_at_launch = reopen;
        if !reopen {
            self.documents_to_reopen.clear();
        }
    }

    /// Replaces the reopen list with the currently open documents' paths, if
    /// the user allows it.
    pub fn set_open_documents(&mut self, paths: Vec<PathBuf>) {
        self.documents_to_reopen = if self.reopen_documents_at_launch {
            paths
        } else {
            Vec::new()
        };
    }

    /// Plain description of what these preferences store, for Settings.
    pub fn stored_metadata_description(&self) -> Vec<String> {
        let mut lines =
            vec!["Window sizes and positions, fonts, and the settings shown here.".to_owned()];
        if self.remember_recent_documents {
            lines.push(format!(
                "Paths of up to {RECENT_DOCUMENTS_LIMIT} recently opened documents."
            ));
        }
        if self.reopen_documents_at_launch {
            lines.push(
                "Paths of documents open when the app quits, to reopen them locked.".to_owned(),
            );
        }
        lines.push("Never document text, passwords, or clipboard contents.".to_owned());
        lines
    }
}

/// Paths containing a newline cannot be stored in this newline-separated
/// format and are skipped rather than stored incorrectly.
fn join_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| !path.contains('\n'))
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_paths(stored: Option<String>) -> Vec<PathBuf> {
    stored
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn preferences_round_trip_through_string_entries() {
        let mut preferences = Preferences {
            font: EditorFont::Palatino,
            font_size: 16,
            clipboard_clear: ClipboardClearPolicy::After(Duration::from_secs(300)),
            lock_after_inactivity_minutes: None,
            default_passphrase_policy: PassphrasePolicy::AskAgainWhenSaving,
            ..Preferences::default()
        };
        preferences.note_recent_document(PathBuf::from("/tmp/a b/ü.txt.age"));
        let stored: HashMap<&str, String> = preferences.to_entries().into_iter().collect();
        let restored = Preferences::from_entries(|key| stored.get(key).cloned());
        assert_eq!(restored, preferences);
    }

    #[test]
    fn defaults_are_conservative() {
        let defaults = Preferences::default();
        assert_eq!(defaults.clipboard_clear, ClipboardClearPolicy::Never);
        assert!(!defaults.reopen_documents_at_launch);
        assert_eq!(
            defaults.default_passphrase_policy,
            PassphrasePolicy::KeepUntilLocked
        );
    }

    #[test]
    fn disabling_recent_documents_forgets_the_list_and_stops_recording() {
        let mut preferences = Preferences::default();
        preferences.note_recent_document(PathBuf::from("/a.age"));
        preferences.set_remember_recent_documents(false);
        assert!(preferences.recent_documents.is_empty());
        preferences.note_recent_document(PathBuf::from("/b.age"));
        assert!(preferences.recent_documents.is_empty());
    }

    #[test]
    fn reopen_list_is_empty_while_reopen_is_off() {
        let mut preferences = Preferences::default();
        preferences.set_open_documents(vec![PathBuf::from("/a.age")]);
        assert!(preferences.documents_to_reopen.is_empty());
        preferences.set_reopen_documents_at_launch(true);
        preferences.set_open_documents(vec![PathBuf::from("/a.age")]);
        assert_eq!(preferences.documents_to_reopen.len(), 1);
    }

    #[test]
    fn recent_list_is_bounded_and_most_recent_first() {
        let mut preferences = Preferences::default();
        for index in 0..15 {
            preferences.note_recent_document(PathBuf::from(format!("/{index}.age")));
        }
        assert_eq!(preferences.recent_documents.len(), RECENT_DOCUMENTS_LIMIT);
        assert_eq!(preferences.recent_documents[0], PathBuf::from("/14.age"));
    }
}
