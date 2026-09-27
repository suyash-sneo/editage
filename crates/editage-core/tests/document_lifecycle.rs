//! The document state machine and passphrase policies.

mod common;

use std::time::Duration;

use common::*;
use editage_core::diagnostics::{DiagnosticEvent, DiagnosticLog};
use editage_core::document::{
    AutoLockDecision, DocumentState, EditorCleared, LockReadiness, LockReason, PassphrasePolicy,
    PlaintextPresence, SaveCredential, SaveCredentialNeed, SaveStart, SaveTarget, UndoHistory,
    UnsavedChangesDecision,
};
use editage_core::error::EditorError;
use editage_core::storage::FileSystemStorage;
use editage_core::{open_encrypted_document, run_save_transaction, DocumentSession};

const CLEARED: EditorCleared = EditorCleared {
    undo_history_cleared: true,
};

fn document_on_disk() -> (tempfile::TempDir, std::path::PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("notes.txt.age");
    write_encrypted(&path, DUMMY_TEXT, PASSPHRASE);
    (folder, path)
}

#[test]
fn opening_a_document_leaves_it_locked_until_a_passphrase_is_given() {
    let (_folder, path) = document_on_disk();
    let session = open_encrypted_document(
        &path,
        PassphrasePolicy::KeepUntilLocked,
        DiagnosticLog::new(),
    )
    .unwrap();
    assert_eq!(session.state(), DocumentState::Locked);
    assert_eq!(session.plaintext_presence(), PlaintextPresence::NoPlaintext);
    assert!(!session.has_retained_passphrase());
}

#[test]
fn opening_successful_document_transitions_to_unlocked() {
    let (_folder, path) = document_on_disk();
    let (session, text) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert_eq!(session.state(), DocumentState::UnlockedClean);
    assert_eq!(text.as_str(), DUMMY_TEXT);
    assert_eq!(
        session.plaintext_presence(),
        PlaintextPresence::InEditor {
            bytes: DUMMY_TEXT.len()
        }
    );
}

#[test]
fn wrong_passphrase_leaves_the_document_locked_and_allows_retrying() {
    let (_folder, path) = document_on_disk();
    let mut session = open_encrypted_document(
        &path,
        PassphrasePolicy::KeepUntilLocked,
        DiagnosticLog::new(),
    )
    .unwrap();
    let job = session
        .begin_unlock(passphrase("wrong"), PassphrasePolicy::KeepUntilLocked)
        .unwrap();
    let result = session.finish_unlock(job.run());
    assert!(matches!(
        result,
        Err(EditorError::AuthenticationFailed { .. })
    ));
    assert_eq!(session.state(), DocumentState::Locked);
    assert!(!session.has_retained_passphrase());

    let job = session
        .begin_unlock(passphrase(PASSPHRASE), PassphrasePolicy::KeepUntilLocked)
        .unwrap();
    assert!(session.finish_unlock(job.run()).is_ok());
}

#[test]
fn editing_marks_dirty() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert!(!session.has_unsaved_changes());
    session.record_edit(99).unwrap();
    assert_eq!(session.state(), DocumentState::UnlockedModified);
    assert!(session.has_unsaved_changes());
    assert_eq!(session.undo_history(), UndoHistory::MayContainPlaintext);
}

#[test]
fn successful_save_marks_clean() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(5).unwrap();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "saved",
    );
    let completion = session.finish_save(result).unwrap();
    assert!(completion.committed);
    assert_eq!(session.state(), DocumentState::UnlockedClean);
    assert!(!session.has_unsaved_changes());
    assert!(session.last_save().is_some());
}

#[test]
fn edits_made_while_saving_keep_the_document_modified() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(5).unwrap();
    let SaveStart::Started(job) = session
        .begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            plaintext("snapshot"),
        )
        .unwrap()
    else {
        panic!()
    };
    // The user keeps typing while encryption runs.
    session.record_edit(6).unwrap();
    let result = run_save_transaction(job, &FileSystemStorage, &mut |_| {});
    session.finish_save(result).unwrap();
    assert_eq!(session.state(), DocumentState::UnlockedModified);
}

#[test]
fn a_second_save_request_during_a_save_is_queued_not_run_concurrently() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(5).unwrap();
    let SaveStart::Started(job) = session
        .begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            plaintext("first"),
        )
        .unwrap()
    else {
        panic!()
    };
    session.record_edit(6).unwrap();
    let second = session
        .begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            plaintext("second"),
        )
        .unwrap();
    assert!(matches!(second, SaveStart::QueuedBehindRunningSave));
    let result = run_save_transaction(job, &FileSystemStorage, &mut |_| {});
    let completion = session.finish_save(result).unwrap();
    assert!(completion.follow_up_save_requested);
}

#[test]
fn lock_clears_document_session_state() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert!(session.has_retained_passphrase());
    session
        .begin_lock(
            UnsavedChangesDecision::NoUnsavedChanges,
            LockReason::LockedByUser,
        )
        .unwrap();
    assert_eq!(session.state(), DocumentState::Locking);
    session.finish_lock(CLEARED).unwrap();
    assert_eq!(session.state(), DocumentState::Locked);
    assert_eq!(session.plaintext_presence(), PlaintextPresence::NoPlaintext);
    assert_eq!(session.undo_history(), UndoHistory::Cleared);
    assert!(!session.has_retained_passphrase());
    assert_eq!(session.lock_reason(), LockReason::LockedByUser);
}

#[test]
fn locked_document_can_be_unlocked_again_reading_the_current_file() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session
        .begin_lock(
            UnsavedChangesDecision::NoUnsavedChanges,
            LockReason::LockedByUser,
        )
        .unwrap();
    session.finish_lock(CLEARED).unwrap();
    write_encrypted(&path, "changed while locked", PASSPHRASE);
    let job = session
        .begin_unlock(passphrase(PASSPHRASE), PassphrasePolicy::KeepUntilLocked)
        .unwrap();
    let text = session.finish_unlock(job.run()).unwrap();
    assert_eq!(text.as_str(), "changed while locked");
}

#[test]
fn locking_with_unsaved_changes_requires_an_explicit_decision() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(3).unwrap();
    assert_eq!(session.lock_readiness(), LockReadiness::HasUnsavedChanges);
    assert!(session
        .begin_lock(
            UnsavedChangesDecision::NoUnsavedChanges,
            LockReason::LockedByUser
        )
        .is_err());
    assert!(session.is_unlocked());
    session
        .begin_lock(
            UnsavedChangesDecision::DiscardUnsavedChanges,
            LockReason::LockedByUser,
        )
        .unwrap();
    session.finish_lock(CLEARED).unwrap();
    assert_eq!(session.state(), DocumentState::Locked);
}

#[test]
fn invalid_state_transitions_are_rejected() {
    let (_folder, path) = document_on_disk();
    let mut session = open_encrypted_document(
        &path,
        PassphrasePolicy::KeepUntilLocked,
        DiagnosticLog::new(),
    )
    .unwrap();
    // Locked documents cannot be edited, saved, or locked again.
    assert!(matches!(
        session.record_edit(1),
        Err(EditorError::InvalidTransition { .. })
    ));
    assert!(matches!(
        session.begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            plaintext("x")
        ),
        Err(EditorError::InvalidTransition { .. })
    ));
    assert!(session
        .begin_lock(
            UnsavedChangesDecision::DiscardUnsavedChanges,
            LockReason::LockedByUser
        )
        .is_err());
    assert!(matches!(
        session.finish_lock(CLEARED),
        Err(EditorError::InvalidTransition { .. })
    ));

    // An unlocked document cannot be unlocked again.
    let job = session
        .begin_unlock(passphrase(PASSPHRASE), PassphrasePolicy::KeepUntilLocked)
        .unwrap();
    session.finish_unlock(job.run()).unwrap();
    assert!(matches!(
        session.begin_unlock(passphrase(PASSPHRASE), PassphrasePolicy::KeepUntilLocked),
        Err(EditorError::InvalidTransition { .. })
    ));
}

#[test]
fn a_never_saved_document_cannot_be_locked_or_saved_without_a_location() {
    let mut session =
        DocumentSession::new_untitled(PassphrasePolicy::KeepUntilLocked, DiagnosticLog::new());
    assert_eq!(session.lock_readiness(), LockReadiness::NeverSaved);
    assert_eq!(
        session.save_credential_need(),
        SaveCredentialNeed::AskForNewPassphrase
    );
    assert!(session
        .begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::New(passphrase("p")),
            plaintext("x")
        )
        .is_err());
}

#[test]
fn closing_releases_retained_secret_container() {
    let log = DiagnosticLog::new();
    let (_folder, path) = document_on_disk();
    let mut session =
        open_encrypted_document(&path, PassphrasePolicy::KeepUntilLocked, log.clone()).unwrap();
    let job = session
        .begin_unlock(passphrase(PASSPHRASE), PassphrasePolicy::KeepUntilLocked)
        .unwrap();
    session.finish_unlock(job.run()).unwrap();
    assert!(session.has_retained_passphrase());
    session
        .close(UnsavedChangesDecision::NoUnsavedChanges, CLEARED)
        .unwrap();
    assert_eq!(session.state(), DocumentState::Closed);
    assert!(!session.has_retained_passphrase());
    assert!(log
        .entries()
        .iter()
        .any(|entry| entry.event == DiagnosticEvent::PassphraseReleased));
}

#[test]
fn closing_with_unsaved_changes_requires_discard_decision() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(1).unwrap();
    assert!(session.close_needs_decision());
    assert!(session
        .close(UnsavedChangesDecision::NoUnsavedChanges, CLEARED)
        .is_err());
    session
        .close(UnsavedChangesDecision::DiscardUnsavedChanges, CLEARED)
        .unwrap();
}

#[test]
fn retained_policy_preserves_access_until_lock() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert_eq!(
        session.save_credential_need(),
        SaveCredentialNeed::UseRetained
    );
    for text in ["first save", "second save"] {
        session.record_edit(1).unwrap();
        let result = save_with(
            &mut session,
            &FileSystemStorage,
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            text,
        );
        session.finish_save(result).unwrap();
        assert_eq!(decrypt_file(&path, PASSPHRASE), text);
    }
    session
        .begin_lock(
            UnsavedChangesDecision::NoUnsavedChanges,
            LockReason::LockedByUser,
        )
        .unwrap();
    session.finish_lock(CLEARED).unwrap();
    assert!(!session.has_retained_passphrase());
}

#[test]
fn prompt_every_save_policy_does_not_retain_passphrase_after_unlock() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::AskAgainWhenSaving);
    assert!(!session.has_retained_passphrase());
    assert_eq!(
        session.save_credential_need(),
        SaveCredentialNeed::AskForCurrentPassphrase
    );
    // Saving with "use retained" is refused rather than silently doing
    // something else.
    assert!(matches!(
        session.begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            plaintext("x")
        ),
        Err(EditorError::PassphraseRequired)
    ));
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::ReenteredCurrent(passphrase(PASSPHRASE)),
        "saved with re-entered passphrase",
    );
    session.finish_save(result).unwrap();
    assert!(!session.has_retained_passphrase());
    assert_eq!(
        decrypt_file(&path, PASSPHRASE),
        "saved with re-entered passphrase"
    );
}

#[test]
fn forgetting_the_retained_passphrase_requires_it_for_the_next_save() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.forget_retained_passphrase();
    assert!(!session.has_retained_passphrase());
    assert_eq!(
        session.save_credential_need(),
        SaveCredentialNeed::AskForCurrentPassphrase
    );
}

#[test]
fn changing_the_password_re_encrypts_the_whole_document() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::New(passphrase("a brand new passphrase")),
        DUMMY_TEXT,
    );
    session.finish_save(result).unwrap();
    assert_eq!(decrypt_file(&path, "a brand new passphrase"), DUMMY_TEXT);
    // The newly set passphrase is the one retained for later saves.
    session.record_edit(1).unwrap();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "later",
    );
    session.finish_save(result).unwrap();
    assert_eq!(decrypt_file(&path, "a brand new passphrase"), "later");
}

#[test]
fn failed_password_change_keeps_the_previous_retained_passphrase() {
    let (folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let storage = editage_core::storage::FaultInjectingStorage::failing_at(
        editage_core::storage::FaultPoint::ReplaceDestination,
    );
    let result = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::New(passphrase("never committed")),
        DUMMY_TEXT,
    );
    session.finish_save(result).unwrap();
    session.acknowledge_save_failure();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    );
    session.finish_save(result).unwrap();
    assert_eq!(decrypt_file(&path, PASSPHRASE), "x");
    drop(folder);
}

#[test]
fn auto_lock_is_postponed_while_there_are_unsaved_changes() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let after = Some(Duration::from_secs(15 * 60));
    assert_eq!(
        session.auto_lock_decision(Duration::from_secs(60), after),
        AutoLockDecision::NotDue
    );
    assert_eq!(
        session.auto_lock_decision(Duration::from_secs(16 * 60), after),
        AutoLockDecision::LockNow
    );
    session.record_edit(1).unwrap();
    assert_eq!(
        session.auto_lock_decision(Duration::from_secs(16 * 60), after),
        AutoLockDecision::PostponedUnsavedChanges
    );
    assert!(session.auto_lock_postponed());
    assert_eq!(
        session.auto_lock_decision(Duration::from_secs(16 * 60), None),
        AutoLockDecision::NotApplicable
    );
}

#[test]
fn reload_from_disk_replaces_text_and_marks_clean() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(1).unwrap();
    write_encrypted(&path, "newer version on disk", PASSPHRASE);
    let job = session.begin_reload(SaveCredential::Retained).unwrap();
    let text = session.finish_reload(job.run()).unwrap();
    assert_eq!(text.as_str(), "newer version on disk");
    assert_eq!(session.state(), DocumentState::UnlockedClean);
    // Saving now targets the reloaded version and succeeds.
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "merged by hand",
    );
    assert!(result.is_ok());
}

#[test]
fn external_change_check_detects_a_changed_file() {
    let (_folder, path) = document_on_disk();
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let job = session.begin_external_check().unwrap();
    assert!(!session.finish_external_check(job.run(&FileSystemStorage)));
    write_encrypted(&path, "changed elsewhere", PASSPHRASE);
    let job = session.begin_external_check().unwrap();
    assert!(session.finish_external_check(job.run(&FileSystemStorage)));
    assert!(matches!(
        session.external_change(),
        editage_core::document::ExternalChangeStatus::Changed { .. }
    ));
}

#[test]
fn diagnostics_never_contain_plaintext_or_passphrase() {
    let log = DiagnosticLog::new();
    let (_folder, path) = document_on_disk();
    let mut session =
        open_encrypted_document(&path, PassphrasePolicy::KeepUntilLocked, log.clone()).unwrap();
    let job = session
        .begin_unlock(passphrase(PASSPHRASE), PassphrasePolicy::KeepUntilLocked)
        .unwrap();
    session.finish_unlock(job.run()).unwrap();
    session.record_edit(1).unwrap();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "SECRET-EDIT-MARKER",
    );
    session.finish_save(result).unwrap();
    let exported = log.export_text(&|_| "t".to_owned());
    assert!(exported.contains("Save complete"));
    for forbidden in [PASSPHRASE, "SECRET-EDIT-MARKER", "username: example"] {
        assert!(
            !exported.contains(forbidden),
            "diagnostics leaked {forbidden:?}"
        );
    }
}

#[test]
fn debug_formatting_a_session_does_not_dump_the_passphrase() {
    let (_folder, path) = document_on_disk();
    let (session, text) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let printed = format!("{session:?} {text:?}");
    assert!(!printed.contains(PASSPHRASE));
    assert!(!printed.contains("username: example"));
    assert!(printed.contains("redacted"));
}
