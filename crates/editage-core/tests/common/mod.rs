//! Helpers shared by the integration test files.
#![allow(dead_code)] // Each test file uses a different subset.

use std::fs;
use std::path::{Path, PathBuf};

use editage_core::crypto::{encrypt_document, EncryptionFormat};
use editage_core::diagnostics::DiagnosticLog;
use editage_core::document::{PassphrasePolicy, SaveCredential, SaveStart, SaveTarget};
use editage_core::save::{run_save_transaction, SaveResult};
use editage_core::secrets::{Credential, Passphrase, Plaintext};
use editage_core::storage::StorageBackend;
use editage_core::{open_encrypted_document, DocumentSession};

pub const PASSPHRASE: &str = "dummy test passphrase";
pub const DUMMY_TEXT: &str = "Dummy entry\nusername: example\npassword: example\n";

pub fn passphrase(text: &str) -> Credential {
    Credential::Passphrase(Passphrase::from_string(text.to_owned()))
}

pub fn plaintext(text: &str) -> Plaintext {
    Plaintext::from_string(text.to_owned())
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

/// Writes an encrypted file the way the application would.
pub fn write_encrypted(path: &Path, text: &str, secret: &str) -> Vec<u8> {
    let ciphertext = encrypt_document(
        &plaintext(text),
        EncryptionFormat::for_new_documents(),
        passphrase(secret),
    )
    .expect("encrypt");
    fs::write(path, &ciphertext).expect("write fixture");
    ciphertext
}

/// Opens and unlocks synchronously (the app runs the job in the background).
pub fn open_and_unlock(
    path: &Path,
    secret: &str,
    policy: PassphrasePolicy,
) -> (DocumentSession, Plaintext) {
    let mut session = open_encrypted_document(path, policy, DiagnosticLog::new()).expect("open");
    let job = session
        .begin_unlock(passphrase(secret), policy)
        .expect("begin unlock");
    let text = session.finish_unlock(job.run()).expect("unlock");
    (session, text)
}

/// Runs a whole save synchronously against a storage backend.
#[allow(clippy::result_large_err)] // Mirrors `run_save_transaction`.
pub fn save_with(
    session: &mut DocumentSession,
    storage: &dyn StorageBackend,
    target: SaveTarget,
    credential: SaveCredential,
    text: &str,
) -> SaveResult {
    let job = match session
        .begin_save(target, credential, plaintext(text))
        .expect("begin save")
    {
        SaveStart::Started(job) => job,
        SaveStart::QueuedBehindRunningSave => panic!("unexpected queue"),
    };
    run_save_transaction(job, storage, &mut |_| {})
}

/// Decrypts a file on disk with the application's own decryption path.
pub fn decrypt_file(path: &Path, secret: &str) -> String {
    let ciphertext = fs::read(path).expect("read");
    let format = editage_core::crypto::detect_encryption_format(&ciphertext).expect("format");
    editage_core::crypto::decrypt_document(&ciphertext, format, passphrase(secret))
        .expect("decrypt")
        .as_str()
        .to_owned()
}

/// Files in `folder` other than `except`.
pub fn other_files(folder: &Path, except: &[&Path]) -> Vec<PathBuf> {
    fs::read_dir(folder)
        .expect("read dir")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| !except.iter().any(|keep| keep == path))
        .collect()
}
