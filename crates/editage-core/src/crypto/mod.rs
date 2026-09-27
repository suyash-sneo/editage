//! Encryption and decryption of whole documents.
//!
//! This module is the only place that knows which encryption formats exist.
//! v1 supports one: the standard age format with passphrase (scrypt)
//! protection, in either binary or ASCII-armored encoding.
//!
//! Adding another method (for example OpenPGP) means:
//! 1. a new [`EncryptionFormat`] variant and detection rule in
//!    [`detect_encryption_format`],
//! 2. a new [`crate::secrets::Credential`] variant if it needs a different
//!    kind of secret,
//! 3. a sibling module to `age_format` with `decrypt`/`encrypt`/verifier
//!    functions, dispatched from the `match` statements below.
//!
//! Nothing outside this module matches on the format's internals, so the save
//! protocol, the document state machine and the frontends stay unchanged.
//!
//! No cryptographic primitive is implemented here; the `age` crate does all of
//! the cryptography.

mod age_format;

pub use age_format::{ENCRYPTION_WORK_FACTOR, MAXIMUM_ACCEPTED_WORK_FACTOR};

use crate::error::EditorError;
use crate::secrets::{Credential, CredentialKind, Plaintext};

/// The on-disk encryption format of a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncryptionFormat {
    /// age v1 (<https://age-encryption.org/v1>), passphrase-protected.
    Age(AgeEncoding),
}

/// How an age file is encoded on disk. Saving keeps the encoding the file was
/// opened with, so an armored file stays armored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgeEncoding {
    Binary,
    Armored,
}

impl EncryptionFormat {
    /// The format used when a new document is saved for the first time.
    pub const fn for_new_documents() -> Self {
        EncryptionFormat::Age(AgeEncoding::Binary)
    }

    /// Which secret the user has to provide for this format.
    pub fn required_credential(&self) -> CredentialKind {
        match self {
            EncryptionFormat::Age(_) => CredentialKind::Passphrase,
        }
    }

    /// Short format name shown in the Info popover and Security Inspector.
    pub fn display_name(&self) -> &'static str {
        match self {
            EncryptionFormat::Age(AgeEncoding::Binary) => "age v1",
            EncryptionFormat::Age(AgeEncoding::Armored) => "age v1 (ASCII armored)",
        }
    }

    /// How the file is protected, in words.
    pub fn protection_description(&self) -> &'static str {
        match self {
            EncryptionFormat::Age(_) => "Passphrase",
        }
    }

    /// The key-derivation parameters used when this application encrypts.
    pub fn key_derivation_description(&self) -> String {
        match self {
            EncryptionFormat::Age(_) => format!(
                "scrypt, work factor 2^{ENCRYPTION_WORK_FACTOR} (the reference age default); files up to 2^{MAXIMUM_ACCEPTED_WORK_FACTOR} are accepted"
            ),
        }
    }

    /// A command the user could run to decrypt the file without this app.
    pub fn independent_decrypt_command(&self) -> &'static str {
        match self {
            EncryptionFormat::Age(_) => "age --decrypt",
        }
    }
}

/// Identifies the format of `ciphertext` and checks that this version can open
/// it, without decrypting anything and without needing a secret.
pub fn detect_encryption_format(ciphertext: &[u8]) -> Result<EncryptionFormat, EditorError> {
    if let Some(encoding) = age_format::recognise(ciphertext) {
        age_format::check_header_is_passphrase_protected(ciphertext)?;
        return Ok(EncryptionFormat::Age(encoding));
    }
    if looks_like_openpgp(ciphertext) {
        return Err(EditorError::UnsupportedFormat {
            detected: "OpenPGP (not supported in this version)".to_owned(),
        });
    }
    Err(EditorError::UnsupportedFormat {
        detected: "unrecognised data (not an age file)".to_owned(),
    })
}

/// Decrypts a complete document and checks that it is UTF-8 text.
///
/// No partial plaintext is ever returned: either the whole file decrypts and
/// authenticates, or an error is returned and any partially decrypted bytes
/// are zeroized.
pub fn decrypt_document(
    ciphertext: &[u8],
    format: EncryptionFormat,
    credential: Credential,
) -> Result<Plaintext, EditorError> {
    match (format, credential) {
        (EncryptionFormat::Age(_), Credential::Passphrase(passphrase)) => {
            age_format::decrypt_with_passphrase(ciphertext, passphrase)
        }
    }
}

/// Encrypts a complete document in memory.
///
/// The returned bytes are a complete, standard file of the given format. They
/// are ciphertext and are not treated as secret.
pub fn encrypt_document(
    plaintext: &Plaintext,
    format: EncryptionFormat,
    credential: Credential,
) -> Result<Vec<u8>, EditorError> {
    match (format, credential) {
        (EncryptionFormat::Age(encoding), Credential::Passphrase(passphrase)) => {
            age_format::encrypt_with_passphrase(plaintext, encoding, passphrase)
        }
    }
}

/// Non-secret data that lets the application check whether a passphrase typed
/// later is the one a document is currently encrypted with.
///
/// For age this is the file header (which is part of the ciphertext anyway).
/// Checking costs one scrypt derivation, the same as unlocking.
///
/// This exists for the "ask again when saving" policy: without it, a typo
/// at save time would silently re-encrypt the document under a new passphrase.
#[derive(Clone)]
pub struct PassphraseVerifier {
    format: EncryptionFormat,
    header: Vec<u8>,
}

impl PassphraseVerifier {
    /// Builds a verifier from a complete ciphertext.
    pub fn from_ciphertext(
        ciphertext: &[u8],
        format: EncryptionFormat,
    ) -> Result<Self, EditorError> {
        let header = match format {
            EncryptionFormat::Age(_) => age_format::extract_header(ciphertext)?,
        };
        Ok(PassphraseVerifier { format, header })
    }

    /// Returns `Ok(())` if `credential` unlocks this document's header, or
    /// [`EditorError::PassphraseMismatch`] if it does not.
    pub fn confirm(&self, credential: Credential) -> Result<(), EditorError> {
        match (self.format, credential) {
            (EncryptionFormat::Age(_), Credential::Passphrase(passphrase)) => {
                age_format::confirm_header_passphrase(&self.header, passphrase)
            }
        }
    }
}

impl std::fmt::Debug for PassphraseVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PassphraseVerifier {{ format: {:?}, header: {} bytes }}",
            self.format,
            self.header.len()
        )
    }
}

fn looks_like_openpgp(bytes: &[u8]) -> bool {
    let text_start = String::from_utf8_lossy(&bytes[..bytes.len().min(64)]);
    if text_start
        .trim_start()
        .starts_with("-----BEGIN PGP MESSAGE-----")
    {
        return true;
    }
    // Binary OpenPGP messages usually start with a public-key or symmetric-key
    // encrypted session key packet (old and new packet header forms).
    matches!(bytes.first(), Some(0x84 | 0x85 | 0x8c | 0x8d | 0xc1 | 0xc3))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::Passphrase;

    fn passphrase(text: &str) -> Credential {
        Credential::Passphrase(Passphrase::from_string(text.to_owned()))
    }

    fn encrypt(text: &str, encoding: AgeEncoding, secret: &str) -> Vec<u8> {
        encrypt_document(
            &Plaintext::from_string(text.to_owned()),
            EncryptionFormat::Age(encoding),
            passphrase(secret),
        )
        .expect("encryption succeeds")
    }

    #[test]
    fn plaintext_round_trips_through_age() {
        let ciphertext = encrypt("dummy line\nsecond line\n", AgeEncoding::Binary, "pw");
        let format = detect_encryption_format(&ciphertext).unwrap();
        let plaintext = decrypt_document(&ciphertext, format, passphrase("pw")).unwrap();
        assert_eq!(plaintext.as_str(), "dummy line\nsecond line\n");
    }

    #[test]
    fn armored_documents_round_trip_and_are_detected_as_armored() {
        let ciphertext = encrypt("armored dummy", AgeEncoding::Armored, "pw");
        assert!(ciphertext.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----"));
        let format = detect_encryption_format(&ciphertext).unwrap();
        assert_eq!(format, EncryptionFormat::Age(AgeEncoding::Armored));
        let plaintext = decrypt_document(&ciphertext, format, passphrase("pw")).unwrap();
        assert_eq!(plaintext.as_str(), "armored dummy");
    }

    #[test]
    fn wrong_passphrase_fails_with_authentication_failure() {
        let ciphertext = encrypt("dummy", AgeEncoding::Binary, "right");
        let format = detect_encryption_format(&ciphertext).unwrap();
        let result = decrypt_document(&ciphertext, format, passphrase("wrong"));
        assert!(matches!(
            result,
            Err(EditorError::AuthenticationFailed { .. })
        ));
    }

    #[test]
    fn corrupted_payload_fails_without_returning_plaintext() {
        let mut ciphertext = encrypt("dummy text that will be damaged", AgeEncoding::Binary, "pw");
        let last = ciphertext.len() - 5;
        ciphertext[last] ^= 0xff;
        let format = detect_encryption_format(&ciphertext).unwrap();
        let result = decrypt_document(&ciphertext, format, passphrase("pw"));
        assert!(matches!(result, Err(EditorError::Decryption { .. })));
    }

    #[test]
    fn truncated_payload_fails() {
        let ciphertext = encrypt(
            "dummy text that will be truncated",
            AgeEncoding::Binary,
            "pw",
        );
        let truncated = &ciphertext[..ciphertext.len() - 10];
        let format = detect_encryption_format(truncated).unwrap();
        let result = decrypt_document(truncated, format, passphrase("pw"));
        assert!(matches!(result, Err(EditorError::Decryption { .. })));
    }

    #[test]
    fn corrupted_header_is_reported_as_invalid_age_file() {
        let mut ciphertext = encrypt("dummy", AgeEncoding::Binary, "pw");
        // Damage the header MAC line ("--- <mac>").
        let mac_line = ciphertext
            .windows(4)
            .position(|w| w == b"\n---")
            .expect("header has a MAC line");
        ciphertext[mac_line + 6] ^= 0x01;
        let result = detect_encryption_format(&ciphertext)
            .and_then(|format| decrypt_document(&ciphertext, format, passphrase("pw")));
        assert!(matches!(result, Err(EditorError::InvalidAgeFile { .. })));
    }

    #[test]
    fn encrypted_output_differs_across_saves_of_the_same_text() {
        let first = encrypt("same text", AgeEncoding::Binary, "pw");
        let second = encrypt("same text", AgeEncoding::Binary, "pw");
        assert_ne!(first, second);
    }

    #[test]
    fn empty_document_round_trips() {
        let ciphertext = encrypt("", AgeEncoding::Binary, "pw");
        let format = detect_encryption_format(&ciphertext).unwrap();
        let plaintext = decrypt_document(&ciphertext, format, passphrase("pw")).unwrap();
        assert_eq!(plaintext.as_str(), "");
    }

    #[test]
    fn unicode_document_round_trips() {
        let text = "Grüße — 日本語 — emoji 🔐 — RTL שלום\n";
        let ciphertext = encrypt(text, AgeEncoding::Binary, "pässwörd ✓");
        let format = detect_encryption_format(&ciphertext).unwrap();
        let plaintext = decrypt_document(&ciphertext, format, passphrase("pässwörd ✓")).unwrap();
        assert_eq!(plaintext.as_str(), text);
    }

    #[test]
    fn non_age_data_is_rejected_as_unsupported_format() {
        let result = detect_encryption_format(b"just some plain text\n");
        assert!(matches!(result, Err(EditorError::UnsupportedFormat { .. })));
    }

    #[test]
    fn openpgp_data_is_named_in_the_unsupported_format_error() {
        let result = detect_encryption_format(b"-----BEGIN PGP MESSAGE-----\n\nabc\n");
        match result {
            Err(EditorError::UnsupportedFormat { detected }) => {
                assert!(detected.contains("OpenPGP"))
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn verifier_accepts_the_document_passphrase_and_rejects_others() {
        let ciphertext = encrypt("dummy", AgeEncoding::Armored, "right");
        let format = detect_encryption_format(&ciphertext).unwrap();
        let verifier = PassphraseVerifier::from_ciphertext(&ciphertext, format).unwrap();
        assert!(verifier.confirm(passphrase("right")).is_ok());
        assert!(matches!(
            verifier.confirm(passphrase("typo")),
            Err(EditorError::PassphraseMismatch)
        ));
    }
}
