//! End-to-end behaviour with real files, and interoperability with the
//! reference age implementation.
//!
//! The fixtures in `tests/fixtures/` were produced by the Go `age` CLI (see
//! `tests/fixtures/generate-fixtures.sh`), not by this application, so these
//! tests do not only prove compatibility with ourselves.

mod common;

use std::fs;
use std::process::Command;

use common::*;
use editage_core::crypto::{AgeEncoding, EncryptionFormat};
use editage_core::diagnostics::DiagnosticLog;
use editage_core::document::{PassphrasePolicy, SaveCredential, SaveTarget};
use editage_core::error::EditorError;
use editage_core::storage::{FaultInjectingStorage, FaultPoint, FileSystemStorage};
use editage_core::{open_encrypted_document, DocumentSession};

const FIXTURE_PASSPHRASE: &str = "fixture-passphrase";

fn fixture_text(name: &str) -> String {
    fs::read_to_string(fixture(name)).unwrap()
}

#[test]
fn file_encrypted_by_the_reference_age_cli_opens() {
    let (session, text) = open_and_unlock(
        &fixture("interop-binary.txt.age"),
        FIXTURE_PASSPHRASE,
        PassphrasePolicy::KeepUntilLocked,
    );
    assert_eq!(text.as_str(), fixture_text("interop-plaintext.txt"));
    assert_eq!(session.format(), EncryptionFormat::Age(AgeEncoding::Binary));
}

#[test]
fn armored_file_from_the_reference_cli_opens_and_stays_armored_when_saved() {
    let folder = tempfile::tempdir().unwrap();
    let copy = folder.path().join("armored.txt.age");
    fs::copy(fixture("interop-armored.txt.age"), &copy).unwrap();
    let (mut session, text) =
        open_and_unlock(&copy, FIXTURE_PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert_eq!(text.as_str(), fixture_text("interop-plaintext.txt"));
    assert_eq!(
        session.format(),
        EncryptionFormat::Age(AgeEncoding::Armored)
    );
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "still armored",
    );
    session.finish_save(result).unwrap();
    assert!(fs::read(&copy)
        .unwrap()
        .starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----"));
    assert_eq!(decrypt_file(&copy, FIXTURE_PASSPHRASE), "still armored");
}

#[test]
fn unicode_fixture_from_the_reference_cli_opens() {
    let (_, text) = open_and_unlock(
        &fixture("unicode.txt.age"),
        FIXTURE_PASSPHRASE,
        PassphrasePolicy::KeepUntilLocked,
    );
    assert_eq!(text.as_str(), fixture_text("unicode-plaintext.txt"));
}

#[test]
fn non_utf8_contents_are_reported_without_replacing_bytes_and_source_is_untouched() {
    let path = fixture("not-utf8.bin.age");
    let before = fs::read(&path).unwrap();
    let mut session = open_encrypted_document(
        &path,
        PassphrasePolicy::KeepUntilLocked,
        DiagnosticLog::new(),
    )
    .unwrap();
    let job = session
        .begin_unlock(
            passphrase(FIXTURE_PASSPHRASE),
            PassphrasePolicy::KeepUntilLocked,
        )
        .unwrap();
    match session.finish_unlock(job.run()) {
        Err(EditorError::InvalidUtf8 { valid_up_to, .. }) => {
            assert_eq!(valid_up_to, "valid prefix ".len())
        }
        other => panic!("expected InvalidUtf8, got {other:?}"),
    }
    assert!(!session.is_unlocked());
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn recipient_encrypted_age_files_are_explained_not_misreported() {
    let result = open_encrypted_document(
        &fixture("recipient-encrypted.txt.age"),
        PassphrasePolicy::KeepUntilLocked,
        DiagnosticLog::new(),
    );
    assert!(matches!(
        result,
        Err(EditorError::UnsupportedProtection { .. })
    ));
}

#[test]
fn save_produces_decryptable_age_file_and_reopening_preserves_edits() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("roundtrip.txt.age");
    write_encrypted(&path, DUMMY_TEXT, PASSPHRASE);

    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    session.record_edit(1).unwrap();
    let edited = format!("{DUMMY_TEXT}added line\n");
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        &edited,
    );
    session.finish_save(result).unwrap();
    session
        .close(
            editage_core::document::UnsavedChangesDecision::NoUnsavedChanges,
            editage_core::document::EditorCleared {
                undo_history_cleared: true,
            },
        )
        .unwrap();

    let (_, reopened) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert_eq!(reopened.as_str(), edited);
}

#[test]
fn original_ciphertext_remains_valid_after_injected_failure() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("failure.txt.age");
    write_encrypted(&path, DUMMY_TEXT, PASSPHRASE);
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    let result = save_with(
        &mut session,
        &FaultInjectingStorage::failing_at(FaultPoint::ReplaceDestination),
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "never committed",
    );
    session.finish_save(result).unwrap();
    let (_, text) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    assert_eq!(text.as_str(), DUMMY_TEXT);
}

#[test]
fn external_modification_is_caught() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("synced.txt.age");
    write_encrypted(&path, DUMMY_TEXT, PASSPHRASE);
    let (mut session, _) = open_and_unlock(&path, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);
    write_encrypted(&path, "edited on another device", PASSPHRASE);
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::CurrentFile,
        SaveCredential::Retained,
        "local edits",
    );
    assert!(matches!(
        result.as_ref().map_err(|failure| &failure.error),
        Err(EditorError::ExternalModification { .. })
    ));
    session.finish_save(result).unwrap();
    assert_eq!(decrypt_file(&path, PASSPHRASE), "edited on another device");
}

#[test]
fn save_as_creates_a_separate_valid_encrypted_file_and_only_then_rebinds() {
    let folder = tempfile::tempdir().unwrap();
    let original = folder.path().join("original.txt.age");
    let copy = folder.path().join("copy.txt.age");
    let original_bytes = write_encrypted(&original, DUMMY_TEXT, PASSPHRASE);
    let (mut session, _) =
        open_and_unlock(&original, PASSPHRASE, PassphrasePolicy::KeepUntilLocked);

    // A failing Save As leaves the association unchanged.
    let result = save_with(
        &mut session,
        &FaultInjectingStorage::failing_at(FaultPoint::SyncFile),
        SaveTarget::ChosenPath(copy.clone()),
        SaveCredential::Retained,
        "copy text",
    );
    session.finish_save(result).unwrap();
    assert_eq!(session.path(), Some(original.as_path()));
    assert!(!copy.exists());
    session.acknowledge_save_failure();

    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::ChosenPath(copy.clone()),
        SaveCredential::Retained,
        "copy text",
    );
    session.finish_save(result).unwrap();
    assert_eq!(session.path(), Some(copy.as_path()));
    assert_eq!(session.display_name(), "copy.txt.age");
    assert_eq!(decrypt_file(&copy, PASSPHRASE), "copy text");
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
}

#[test]
fn first_save_of_a_new_document_writes_no_intermediate_plaintext_file() {
    let folder = tempfile::tempdir().unwrap();
    let destination = folder.path().join("brand new.txt.age");
    let mut session =
        DocumentSession::new_untitled(PassphrasePolicy::KeepUntilLocked, DiagnosticLog::new());
    session.record_edit(10).unwrap();
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::ChosenPath(destination.clone()),
        SaveCredential::New(passphrase("new passphrase")),
        "NEW-DOCUMENT-MARKER",
    );
    session.finish_save(result).unwrap();
    let files = other_files(folder.path(), &[]);
    assert_eq!(files, vec![destination.clone()]);
    let bytes = fs::read(&destination).unwrap();
    assert!(!bytes.windows(19).any(|w| w == b"NEW-DOCUMENT-MARKER"));
    assert!(session.has_retained_passphrase());
    assert_eq!(
        decrypt_file(&destination, "new passphrase"),
        "NEW-DOCUMENT-MARKER"
    );
}

/// Decrypts a file with the reference `age` CLI. age reads passphrases only
/// from a terminal, so `expect` supplies it.
fn decrypt_with_reference_cli(path: &std::path::Path, secret: &str) -> String {
    let output_path = path.with_extension("decrypted");
    let script = format!(
        "log_user 0\nspawn age --decrypt --output {{{}}} {{{}}}\nexpect \"Enter passphrase*\"\nsend \"{}\\r\"\nexpect eof\n",
        output_path.display(),
        path.display(),
        secret
    );
    let status = Command::new("expect")
        .arg("-c")
        .arg(script)
        .status()
        .expect("expect is available");
    assert!(status.success());
    fs::read_to_string(&output_path).expect("age produced output")
}

/// Run with `cargo test -- --ignored` on a machine with the `age` CLI (CI
/// installs it).
#[test]
#[ignore = "requires the reference `age` CLI and `expect`"]
fn application_output_decrypts_with_the_reference_age_cli() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("from-editage.txt.age");
    let mut session =
        DocumentSession::new_untitled(PassphrasePolicy::KeepUntilLocked, DiagnosticLog::new());
    session.record_edit(1).unwrap();
    let text = "Written by Editage, read by age.\nGrüße 🔐\n";
    let result = save_with(
        &mut session,
        &FileSystemStorage,
        SaveTarget::ChosenPath(path.clone()),
        SaveCredential::New(passphrase("interop passphrase")),
        text,
    );
    session.finish_save(result).unwrap();
    assert_eq!(
        decrypt_with_reference_cli(&path, "interop passphrase"),
        text
    );
}
