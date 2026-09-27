//! Clipboard policy: what this application copied, and when (if ever) to
//! clear it.
//!
//! The frontend performs the actual pasteboard operations. This module only
//! decides. It identifies "our" clipboard item by the pasteboard's change
//! counter (macOS `NSPasteboard.changeCount`, and equivalents elsewhere),
//! never by comparing contents, so it never needs to read clipboard text.
//!
//! Copying is a plaintext disclosure to every process that can read the
//! clipboard. Clearing later reduces how long the text stays there; it cannot
//! undo that disclosure (clipboard managers may already have a copy).

use std::time::{Duration, SystemTime};

use crate::document::DocumentId;

/// When to clear text this application copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardClearPolicy {
    Never,
    After(Duration),
}

impl ClipboardClearPolicy {
    /// The choices offered in Settings, in display order.
    pub const CHOICES: [ClipboardClearPolicy; 4] = [
        ClipboardClearPolicy::Never,
        ClipboardClearPolicy::After(Duration::from_secs(30)),
        ClipboardClearPolicy::After(Duration::from_secs(60)),
        ClipboardClearPolicy::After(Duration::from_secs(300)),
    ];

    pub fn label(&self) -> String {
        match self {
            ClipboardClearPolicy::Never => "Never".to_owned(),
            ClipboardClearPolicy::After(duration) => {
                let seconds = duration.as_secs();
                if seconds % 60 == 0 && seconds >= 120 {
                    format!("After {} minutes", seconds / 60)
                } else {
                    format!("After {seconds} seconds")
                }
            }
        }
    }
}

/// The clipboard item this application wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrackedClipboardItem {
    /// Pasteboard change count immediately after our write.
    pub change_count: i64,
    pub copied_at: SystemTime,
    pub source_document: DocumentId,
    /// When it will be cleared, if a clear policy was active at copy time.
    pub clear_at: Option<SystemTime>,
}

/// What the frontend should do on a clipboard timer tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardAction {
    Nothing,
    /// Clear the pasteboard: it still holds exactly our item and the deadline
    /// passed.
    ClearNow,
}

/// What the clipboard holds, as far as this application knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardStatus {
    /// Nothing this application copied is on the clipboard.
    NotTracked,
    /// Text copied from a document is on the clipboard.
    ContainsCopiedText(TrackedClipboardItem),
}

#[derive(Debug, Clone)]
pub struct ClipboardTracker {
    policy: ClipboardClearPolicy,
    tracked: Option<TrackedClipboardItem>,
}

impl ClipboardTracker {
    pub fn new(policy: ClipboardClearPolicy) -> Self {
        ClipboardTracker {
            policy,
            tracked: None,
        }
    }

    pub fn policy(&self) -> ClipboardClearPolicy {
        self.policy
    }

    /// Changing the policy applies to future copies. An item already tracked
    /// keeps the deadline it was given, except that switching to Never
    /// cancels a pending clear.
    pub fn set_policy(&mut self, policy: ClipboardClearPolicy) {
        self.policy = policy;
        if policy == ClipboardClearPolicy::Never {
            if let Some(item) = &mut self.tracked {
                item.clear_at = None;
            }
        }
    }

    /// Call right after writing document text to the pasteboard.
    pub fn record_copy(&mut self, change_count: i64, source_document: DocumentId, now: SystemTime) {
        let clear_at = match self.policy {
            ClipboardClearPolicy::Never => None,
            ClipboardClearPolicy::After(delay) => Some(now + delay),
        };
        self.tracked = Some(TrackedClipboardItem {
            change_count,
            copied_at: now,
            source_document,
            clear_at,
        });
    }

    /// Updates tracking from the pasteboard's current change count. If
    /// anything else has written to the clipboard since our copy, we stop
    /// tracking and will never clear it.
    pub fn status(&mut self, current_change_count: i64) -> ClipboardStatus {
        match self.tracked {
            Some(item) if item.change_count == current_change_count => {
                ClipboardStatus::ContainsCopiedText(item)
            }
            Some(_) => {
                self.tracked = None;
                ClipboardStatus::NotTracked
            }
            None => ClipboardStatus::NotTracked,
        }
    }

    /// Whether to clear now.
    pub fn action_on_tick(
        &mut self,
        current_change_count: i64,
        now: SystemTime,
    ) -> ClipboardAction {
        match self.status(current_change_count) {
            ClipboardStatus::ContainsCopiedText(TrackedClipboardItem {
                clear_at: Some(deadline),
                ..
            }) if now >= deadline => ClipboardAction::ClearNow,
            _ => ClipboardAction::Nothing,
        }
    }

    /// Whether "Clear Clipboard Now" should be enabled.
    pub fn can_clear_now(&mut self, current_change_count: i64) -> bool {
        matches!(
            self.status(current_change_count),
            ClipboardStatus::ContainsCopiedText(_)
        )
    }

    /// Call after the frontend cleared the pasteboard.
    pub fn record_cleared(&mut self) {
        self.tracked = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::DiagnosticLog;
    use crate::document::{DocumentSession, PassphrasePolicy};

    fn some_document() -> DocumentId {
        DocumentSession::new_untitled(PassphrasePolicy::KeepUntilLocked, DiagnosticLog::new()).id()
    }

    #[test]
    fn never_policy_never_clears() {
        let mut tracker = ClipboardTracker::new(ClipboardClearPolicy::Never);
        let now = SystemTime::now();
        tracker.record_copy(5, some_document(), now);
        let later = now + Duration::from_secs(3600);
        assert_eq!(tracker.action_on_tick(5, later), ClipboardAction::Nothing);
    }

    #[test]
    fn clears_only_after_the_deadline_while_our_item_is_still_there() {
        let mut tracker =
            ClipboardTracker::new(ClipboardClearPolicy::After(Duration::from_secs(30)));
        let now = SystemTime::now();
        tracker.record_copy(7, some_document(), now);
        assert_eq!(
            tracker.action_on_tick(7, now + Duration::from_secs(10)),
            ClipboardAction::Nothing
        );
        assert_eq!(
            tracker.action_on_tick(7, now + Duration::from_secs(31)),
            ClipboardAction::ClearNow
        );
    }

    #[test]
    fn content_written_by_another_application_is_never_cleared() {
        let mut tracker =
            ClipboardTracker::new(ClipboardClearPolicy::After(Duration::from_secs(30)));
        let now = SystemTime::now();
        tracker.record_copy(7, some_document(), now);
        // Another app copied something: the change count moved on.
        assert_eq!(
            tracker.action_on_tick(8, now + Duration::from_secs(60)),
            ClipboardAction::Nothing
        );
        assert_eq!(tracker.status(8), ClipboardStatus::NotTracked);
        // Even if the counter were somehow observed at our value again later,
        // tracking has already been dropped.
        assert_eq!(
            tracker.action_on_tick(7, now + Duration::from_secs(90)),
            ClipboardAction::Nothing
        );
    }

    #[test]
    fn switching_to_never_cancels_a_pending_clear() {
        let mut tracker =
            ClipboardTracker::new(ClipboardClearPolicy::After(Duration::from_secs(30)));
        let now = SystemTime::now();
        tracker.record_copy(7, some_document(), now);
        tracker.set_policy(ClipboardClearPolicy::Never);
        assert_eq!(
            tracker.action_on_tick(7, now + Duration::from_secs(60)),
            ClipboardAction::Nothing
        );
    }

    #[test]
    fn policy_labels() {
        let labels: Vec<String> = ClipboardClearPolicy::CHOICES
            .iter()
            .map(|p| p.label())
            .collect();
        assert_eq!(
            labels,
            [
                "Never",
                "After 30 seconds",
                "After 60 seconds",
                "After 5 minutes"
            ]
        );
    }
}
