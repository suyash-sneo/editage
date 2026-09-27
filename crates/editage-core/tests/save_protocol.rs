//! The save transaction, tested against real temporary files with failures
//! injected at each stage. These tests are the executable form of
//! docs/save-protocol.md.

mod common;

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;

use common::*;
use editage_core::document::{DocumentState, PassphrasePolicy, SaveCredential, SaveTarget};
use editage_core::error::EditorError;
use editage_core::save::{CleanupResult, OriginalFileState, SaveStage};
use editage_core::storage::{
    ConcurrentChange, FaultInjectingStorage, FaultPoint, FileSystemStorage,
};

struct Fixture {
    _folder: tempfile::TempDir,
    folder: std::path::PathBuf,
    path: std::path::PathBuf,
    original_ciphertext: Vec<u8>,
}

fn fixture_document(name: &str) -> Fixture {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join(name);
    let original_ciphertext = write_encrypted(&path, DUMMY_TEXT, PASSPHRASE);
    Fixture {
        folder: folder.path().to_owned(),
        _folder: folder,
        path,
        original_ciphertext,
    }
}

fn assert_original_intact(fixture: &Fixture) {
    assert_eq!(
        fs::read(&fixture.path).unwrap(),
        fixture.original_ciphertext,
        "the original encrypted file must be byte-for-byte unchanged"
    );
    assert_eq!(decrypt_file(&fixture.path, PASSPHRASE), DUMMY_TEXT);
}

fn assert_no_staging_files_left(fixture: &Fixture) {
    let leftovers = other_files(&fixture.folder, &[fixture.path.as_path()]);
    assert!(
        leftovers.is_empty(),
        "staging files left behind: {leftovers:?}"
    );
}

fn failing_save(fault: FaultPoint) -> (Fixture, editage_core::save::SaveFailure) {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(10).unwrap();
    let storage = FaultInjectingStorage::failing_at(fault);
    let failure = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "edited dummy text",
    )
    .expect_err("save must fail");
    (fixture, failure)
}

#[test]
fn original_survives_staging_file_creation_failure() {
    let (fixture, failure) = failing_save(FaultPoint::CreateStagingFile);
    assert_eq!(failure.stage, SaveStage::CreatingStagingFile);
    assert!(matches!(
        failure.error,
        EditorError::StagingFileCreate { .. }
    ));
    assert_eq!(failure.original, OriginalFileState::Unchanged);
    assert_original_intact(&fixture);
    assert_no_staging_files_left(&fixture);
}

#[test]
fn original_survives_staging_write_failure_and_partial_staging_file_is_removed() {
    let (fixture, failure) = failing_save(FaultPoint::WriteCiphertextPartially);
    assert_eq!(failure.stage, SaveStage::WritingCiphertext);
    let staging = failure.staging_file.as_ref().expect("staging report");
    assert!(staging.removal_attempted);
    assert!(!staging.still_exists);
    assert_original_intact(&fixture);
    assert_no_staging_files_left(&fixture);
}

#[test]
fn original_survives_flush_failure() {
    let (fixture, failure) = failing_save(FaultPoint::SyncFile);
    assert_eq!(failure.stage, SaveStage::Flushing);
    assert!(matches!(failure.error, EditorError::Flush { .. }));
    assert_original_intact(&fixture);
    assert_no_staging_files_left(&fixture);
}

#[test]
fn original_survives_replacement_failure() {
    let (fixture, failure) = failing_save(FaultPoint::ReplaceDestination);
    assert_eq!(failure.stage, SaveStage::ReplacingOriginal);
    assert!(matches!(failure.error, EditorError::AtomicReplace { .. }));
    assert_eq!(failure.original, OriginalFileState::Unchanged);
    assert_original_intact(&fixture);
    assert_no_staging_files_left(&fixture);
}

#[test]
fn staging_file_that_cannot_be_removed_after_a_failure_is_reported_with_its_path() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    // The final pre-replacement check fails (a sync client wrote the file)
    // and removing the staging file fails too.
    let mut storage = FaultInjectingStorage::failing_at(FaultPoint::RemoveStagingFile);
    storage.concurrent_change =
        ConcurrentChange::RewriteDestinationBeforeReplace(b"changed by a sync client".to_vec());
    let failure = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    )
    .expect_err("fails");
    let staging = failure.staging_file.as_ref().expect("report");
    assert!(staging.removal_attempted);
    assert!(staging.still_exists);
    assert!(staging.removal_error.is_some());
    assert!(staging.path.starts_with(&fixture.folder));
    let staged = fs::read(&staging.path).unwrap();
    assert!(
        !contains(&staged, b"example"),
        "staging file must hold ciphertext only"
    );

    session.finish_save(Err(failure)).unwrap();
    assert_eq!(session.leftover_staging_files().len(), 1);
    assert!(!session.leftover_staging_files()[0].save_committed);
}

#[test]
fn original_survives_a_failure_before_encryption_output_exists() {
    // The age library offers no way to make encryption itself fail on
    // demand. This test covers the equivalent guarantee: any failure before
    // encryption output exists (here, a wrong re-entered passphrase) returns
    // before a single storage operation. `stages_are_reported_in_order`
    // shows that no file operation precedes the Encrypting stage.
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) = open_and_unlock(
        &fixture.path,
        PASSPHRASE,
        PassphrasePolicy::AskAgainWhenSaving,
    );
    let failure = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::ReenteredCurrent(passphrase("not the passphrase")),
        "edited",
    )
    .expect_err("mismatch");
    assert_eq!(failure.stage, SaveStage::ConfirmingPassphrase);
    assert!(matches!(failure.error, EditorError::PassphraseMismatch));
    assert!(failure.staging_file.is_none());
    assert_original_intact(&fixture);
    assert_no_staging_files_left(&fixture);
}

#[test]
fn valid_new_file_exists_after_successful_replacement() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(20).unwrap();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "new dummy contents\n",
    );
    let success = result.expect("save succeeds");
    assert!(matches!(success.cleanup, CleanupResult::NothingLeft));
    assert_eq!(
        decrypt_file(&fixture.path, PASSPHRASE),
        "new dummy contents\n"
    );
    assert_no_staging_files_left(&fixture);
}

#[test]
fn complete_ciphertext_is_written_before_destination_is_replaced() {
    // If replacement fails, the staging file (before removal) must have held
    // a complete, decryptable document; we verify by making removal fail too
    // and decrypting what was left.
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let mut storage = FaultInjectingStorage::failing_at(FaultPoint::RemoveStagingFile);
    storage.concurrent_change =
        ConcurrentChange::RewriteDestinationBeforeReplace(b"sync client wrote this".to_vec());
    let failure = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "complete dummy document\n",
    )
    .expect_err("blocked by external change");
    let staging = failure.staging_file.expect("left behind");
    assert_eq!(
        decrypt_file(&staging.path, PASSPHRASE),
        "complete dummy document\n"
    );
}

#[test]
fn staging_file_contains_ciphertext_not_plaintext_buffer() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let recognisable_plaintext = "RECOGNISABLE-PLAINTEXT-MARKER dummy password: example";
    let mut storage = FaultInjectingStorage::failing_at(FaultPoint::RemoveStagingFile);
    storage.concurrent_change =
        ConcurrentChange::RewriteDestinationBeforeReplace(b"changed".to_vec());
    let failure = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        recognisable_plaintext,
    )
    .expect_err("blocked");
    let staged_bytes = fs::read(failure.staging_file.unwrap().path).unwrap();
    assert!(staged_bytes.starts_with(b"age-encryption.org/v1"));
    assert!(!contains(&staged_bytes, b"RECOGNISABLE-PLAINTEXT-MARKER"));
    assert!(!contains(&staged_bytes, b"password: example"));
}

#[test]
fn external_modification_prevents_overwrite() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    // Another program (e.g. a sync client) replaces the file after opening.
    let newer = write_encrypted(
        &fixture.path,
        "newer copy from another device\n",
        PASSPHRASE,
    );
    let failure = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "my edits",
    )
    .expect_err("must not overwrite");
    assert_eq!(failure.stage, SaveStage::CheckingDestination);
    assert!(matches!(
        failure.error,
        EditorError::ExternalModification { .. }
    ));
    assert_eq!(failure.original, OriginalFileState::ChangedByAnotherProgram);
    assert_eq!(fs::read(&fixture.path).unwrap(), newer);
}

#[test]
fn stale_fingerprint_blocks_replacement_when_file_changes_during_the_save() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let storage = FaultInjectingStorage::with_concurrent_change(
        ConcurrentChange::RewriteDestinationBeforeReplace(b"written mid-save".to_vec()),
    );
    let failure = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "mine",
    )
    .expect_err("blocked");
    assert_eq!(failure.stage, SaveStage::ReplacingOriginal);
    assert!(matches!(
        failure.error,
        EditorError::ExternalModification { .. }
    ));
    assert_eq!(fs::read(&fixture.path).unwrap(), b"written mid-save");
    assert_no_staging_files_left(&fixture);
}

#[test]
fn cleanup_failure_does_not_report_save_as_failed() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(5).unwrap();
    let storage =
        FaultInjectingStorage::failing_at(FaultPoint::ReplaceLeavesStagingFileAndRemovalFails);
    let result = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "saved text",
    );
    let success = result.expect("the save itself succeeded");
    let CleanupResult::Failed { path, .. } = &success.cleanup else {
        panic!("expected a cleanup failure, got {:?}", success.cleanup);
    };
    assert!(path.exists());
    assert_eq!(decrypt_file(&fixture.path, PASSPHRASE), "saved text");

    let completion = session.finish_save(Ok(success)).unwrap();
    assert!(completion.committed);
    assert_eq!(session.state(), DocumentState::UnlockedClean);
    assert_eq!(session.leftover_staging_files().len(), 1);
    assert!(session.leftover_staging_files()[0].save_committed);
}

#[test]
fn directory_flush_failure_after_commit_is_recorded_not_reported_as_failure() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let storage = FaultInjectingStorage::failing_at(FaultPoint::SyncDirectory);
    let success = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "t",
    )
    .expect("committed");
    assert!(matches!(
        success.directory_durability,
        editage_core::storage::DirectoryDurability::SyncFailed { .. }
    ));
}

#[test]
fn interrupted_save_before_commit_leaves_original_valid() {
    // Simulate a crash between writing the staging file and replacing: the
    // process "dies" after flushing (the staging file is never renamed).
    // Whatever is left, the destination must still be the old valid file.
    for fault in [
        FaultPoint::CreateStagingFile,
        FaultPoint::WriteCiphertextPartially,
        FaultPoint::SyncFile,
        FaultPoint::ReplaceDestination,
    ] {
        let (fixture, _) = failing_save(fault);
        assert_original_intact(&fixture);
    }
}

#[test]
fn failed_save_remains_dirty_and_keeps_edits_in_memory() {
    let fixture = fixture_document("passwords.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(8).unwrap();
    let storage = FaultInjectingStorage::failing_at(FaultPoint::ReplaceDestination);
    let result = save_with(
        &mut session,
        &storage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "edits",
    );
    let completion = session.finish_save(result).unwrap();
    assert!(!completion.committed);
    assert_eq!(session.state(), DocumentState::SaveFailed);
    assert!(session.has_unsaved_changes());
    assert!(session.is_unlocked());
    session.acknowledge_save_failure();
    assert_eq!(session.state(), DocumentState::UnlockedModified);
}

#[test]
fn path_with_unicode_and_spaces_works() {
    let fixture = fixture_document("Pässwörter und Notizen 🔐 v2.txt.age");
    let (mut session, text) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert_eq!(text.as_str(), DUMMY_TEXT);
    save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "ü",
    )
    .expect("save");
    assert_eq!(decrypt_file(&fixture.path, PASSPHRASE), "ü");
}

#[test]
fn empty_file_works() {
    let fixture = fixture_document("empty.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "",
    )
    .expect("save");
    assert_eq!(decrypt_file(&fixture.path, PASSPHRASE), "");
}

#[test]
fn read_only_destination_produces_correct_error_stage() {
    let fixture = fixture_document("readonly.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o400)).unwrap();
    let failure = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    )
    .expect_err("read-only");
    assert_eq!(failure.stage, SaveStage::CheckingDestination);
    assert!(matches!(
        failure.error,
        EditorError::DestinationReadOnly { .. }
    ));
    assert_eq!(failure.original, OriginalFileState::Unchanged);
    assert_original_intact(&fixture);
}

#[test]
fn read_only_folder_fails_at_staging_file_creation_with_permission_error() {
    let fixture = fixture_document("in-locked-folder.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    fs::set_permissions(&fixture.folder, fs::Permissions::from_mode(0o500)).unwrap();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    );
    fs::set_permissions(&fixture.folder, fs::Permissions::from_mode(0o700)).unwrap();
    let failure = result.expect_err("folder not writable");
    assert_eq!(failure.stage, SaveStage::CreatingStagingFile);
    match &failure.error {
        EditorError::StagingFileCreate { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::PermissionDenied)
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_original_intact(&fixture);
}

#[test]
fn replaced_file_keeps_the_original_permissions() {
    let fixture = fixture_document("perms.txt.age");
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o640)).unwrap();
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    )
    .expect("save");
    let mode = fs::metadata(&fixture.path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);
}

#[test]
fn original_survives_permission_failure_before_replacement() {
    let (fixture, failure) = failing_save(FaultPoint::ApplyFinalPermissions);
    assert_eq!(failure.stage, SaveStage::ReplacingOriginal);
    assert!(matches!(failure.error, EditorError::Permissions { .. }));
    assert_original_intact(&fixture);
    assert_no_staging_files_left(&fixture);
}

#[test]
fn special_permission_bits_are_not_carried_over_to_the_replacement() {
    let fixture = fixture_document("sticky.txt.age");
    fs::set_permissions(&fixture.path, fs::Permissions::from_mode(0o4640)).unwrap();
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    )
    .expect("save");
    let mode = fs::metadata(&fixture.path).unwrap().permissions().mode() & 0o7777;
    assert_eq!(mode, 0o640);
}

#[test]
fn new_documents_are_created_readable_by_the_owner_only() {
    let folder = tempfile::tempdir().unwrap();
    let destination = folder.path().join("new.txt.age");
    let mut session = editage_core::DocumentSession::new_untitled(
        PassphrasePolicy::KeepUntilLocked,
        editage_core::diagnostics::DiagnosticLog::new(),
    );
    session.record_edit(3).unwrap();
    save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::ChosenPath(destination.clone()),
        SaveCredential::New(passphrase(PASSPHRASE)),
        "new",
    )
    .expect("save");
    let mode = fs::metadata(&destination).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn stages_are_reported_in_order() {
    let fixture = fixture_document("stages.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let job = match session
        .begin_save(
            SaveTarget::CurrentFile,
            SaveCredential::Retained,
            plaintext("x"),
        )
        .unwrap()
    {
        editage_core::document::SaveStart::Started(job) => job,
        _ => unreachable!(),
    };
    let mut seen = Vec::new();
    editage_core::run_save_transaction(job, &FileSystemStorage, &mut |stage| seen.push(stage))
        .unwrap();
    assert_eq!(
        seen,
        vec![
            SaveStage::CheckingDestination,
            SaveStage::Encrypting,
            SaveStage::CreatingStagingFile,
            SaveStage::WritingCiphertext,
            SaveStage::Flushing,
            SaveStage::ReplacingOriginal,
            SaveStage::CleaningUp,
            SaveStage::Complete,
        ]
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn destination_replaced_by_a_directory_is_refused_and_reported_as_changed_elsewhere() {
    let fixture = fixture_document("replaced.txt.age");
    let (mut session, _) =
        open_and_unlock(&fixture.path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    fs::remove_file(&fixture.path).unwrap();
    fs::create_dir(&fixture.path).unwrap();
    let failure = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "x",
    )
    .expect_err("refused");
    assert_eq!(failure.stage, SaveStage::CheckingDestination);
    assert!(matches!(failure.error, EditorError::NotARegularFile { .. }));
    assert_eq!(failure.original, OriginalFileState::ChangedByAnotherProgram);
    assert!(fixture.path.is_dir());
}
