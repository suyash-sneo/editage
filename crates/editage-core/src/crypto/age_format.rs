//! age v1 with passphrase (scrypt) protection, via the `age` crate.

use std::io::{self, Read, Write};
use std::iter;

use age::armor::{ArmoredReader, ArmoredWriter, Format};
use age::{DecryptError, Decryptor, Encryptor};
use zeroize::{Zeroize, Zeroizing};

use super::AgeEncoding;
use crate::error::EditorError;
use crate::secrets::{Passphrase, Plaintext};

const BINARY_MAGIC: &[u8] = b"age-encryption.org/";
const ARMOR_BEGIN: &str = "-----BEGIN AGE ENCRYPTED FILE-----";

/// age's payload nonce follows the header and is 16 bytes long.
const PAYLOAD_NONCE_LEN: usize = 16;

/// scrypt work factor (log2 N) used when encrypting: 18, the same fixed value
/// the reference age implementation (Go) uses. The `age` crate would
/// otherwise calibrate to about one second of work on the current machine,
/// which makes the result depend on the build (an unoptimised debug build
/// calibrates far lower) and the machine. A fixed value is predictable and
/// can be stated in the Security Inspector.
pub const ENCRYPTION_WORK_FACTOR: u8 = 18;

/// Highest scrypt work factor accepted when decrypting: 22, the reference
/// implementation's limit. This bounds the memory (up to 4 GiB) and time a
/// malicious file can demand, while opening any file a standard age tool
/// produces. The `age` crate's own default (calibrated target + 4) would
/// reject standard files in debug builds and on slower machines.
pub const MAXIMUM_ACCEPTED_WORK_FACTOR: u8 = 22;

/// Tests use a low work factor so the many save-protocol tests run in
/// milliseconds instead of about a second each. This is compiled only into
/// unit tests and the `test-support` feature, which `lib.rs` refuses to
/// compile into release builds.
#[cfg(any(test, feature = "test-support"))]
const EFFECTIVE_ENCRYPTION_WORK_FACTOR: u8 = 10;
#[cfg(not(any(test, feature = "test-support")))]
const EFFECTIVE_ENCRYPTION_WORK_FACTOR: u8 = ENCRYPTION_WORK_FACTOR;

fn passphrase_identity(passphrase: Passphrase) -> age::scrypt::Identity {
    let mut identity = age::scrypt::Identity::new(passphrase.into_age_secret());
    identity.set_max_work_factor(MAXIMUM_ACCEPTED_WORK_FACTOR);
    identity
}

pub(super) fn recognise(bytes: &[u8]) -> Option<AgeEncoding> {
    if bytes.starts_with(BINARY_MAGIC) {
        return Some(AgeEncoding::Binary);
    }
    let start = String::from_utf8_lossy(&bytes[..bytes.len().min(64)]);
    if start.trim_start().starts_with(ARMOR_BEGIN) {
        return Some(AgeEncoding::Armored);
    }
    None
}

/// Parses the header and checks it is protected by exactly one passphrase.
/// This parses the header only; it derives no keys.
pub(super) fn check_header_is_passphrase_protected(bytes: &[u8]) -> Result<(), EditorError> {
    let decryptor = Decryptor::new_buffered(ArmoredReader::new(bytes)).map_err(header_error)?;
    if decryptor.is_scrypt() {
        Ok(())
    } else {
        Err(EditorError::UnsupportedProtection {
            description: "This age file is encrypted to one or more age recipients (public keys), \
                          not to a passphrase. This version opens passphrase-protected files only."
                .to_owned(),
        })
    }
}

pub(super) fn decrypt_with_passphrase(
    ciphertext: &[u8],
    passphrase: Passphrase,
) -> Result<Plaintext, EditorError> {
    let identity = passphrase_identity(passphrase);
    let decryptor =
        Decryptor::new_buffered(ArmoredReader::new(ciphertext)).map_err(header_error)?;
    let mut payload_reader = decryptor
        .decrypt(iter::once(&identity as &dyn age::Identity))
        .map_err(unlock_error)?;

    // The plaintext is always shorter than the ciphertext, so reserving the
    // ciphertext length up front means the buffer is never reallocated.
    // A reallocation would leave an earlier, unwiped copy of partial
    // plaintext in freed memory.
    let mut plaintext_bytes = Zeroizing::new(Vec::with_capacity(ciphertext.len()));
    if let Err(source) = payload_reader.read_to_end(&mut plaintext_bytes) {
        // `plaintext_bytes` is zeroized when it goes out of scope here, so a
        // partially decrypted prefix is not kept.
        return Err(EditorError::Decryption { source });
    }

    let byte_len = plaintext_bytes.len();
    let owned_bytes = std::mem::take(&mut *plaintext_bytes);
    match String::from_utf8(owned_bytes) {
        Ok(text) => Ok(Plaintext::from_string(text)),
        Err(invalid) => {
            let valid_up_to = invalid.utf8_error().valid_up_to();
            let mut bytes = invalid.into_bytes();
            bytes.zeroize();
            Err(EditorError::InvalidUtf8 {
                valid_up_to,
                byte_len,
            })
        }
    }
}

pub(super) fn encrypt_with_passphrase(
    plaintext: &Plaintext,
    encoding: AgeEncoding,
    passphrase: Passphrase,
) -> Result<Vec<u8>, EditorError> {
    let mut recipient = age::scrypt::Recipient::new(passphrase.into_age_secret());
    recipient.set_work_factor(EFFECTIVE_ENCRYPTION_WORK_FACTOR);
    let encryptor = Encryptor::with_recipients(iter::once(&recipient as &dyn age::Recipient))
        .map_err(|error| EditorError::Encryption {
            source: io::Error::other(error.to_string()),
        })?;

    let output = Vec::with_capacity(plaintext.byte_len() + 512);
    let armor_format = match encoding {
        AgeEncoding::Binary => Format::Binary,
        AgeEncoding::Armored => Format::AsciiArmor,
    };
    let write_everything = || -> io::Result<Vec<u8>> {
        let armored = ArmoredWriter::wrap_output(output, armor_format)?;
        let mut stream = encryptor.wrap_output(armored)?;
        stream.write_all(plaintext.as_bytes())?;
        let armored = stream.finish()?;
        armored.finish()
    };
    write_everything().map_err(|source| EditorError::Encryption { source })
}

/// Returns the binary header plus payload nonce: everything needed to check a
/// passphrase, and nothing of the encrypted payload.
pub(super) fn extract_header(ciphertext: &[u8]) -> Result<Vec<u8>, EditorError> {
    let mut binary = Vec::new();
    ArmoredReader::new(ciphertext)
        .take(64 * 1024)
        .read_to_end(&mut binary)
        .map_err(|source| EditorError::InvalidAgeFile {
            source: DecryptError::Io(source),
        })?;

    // The header ends with the MAC line: "\n--- <base64 mac>\n".
    let mac_line_start = binary
        .windows(5)
        .position(|window| window == b"\n--- ")
        .ok_or(EditorError::InvalidAgeFile {
            source: DecryptError::InvalidHeader,
        })?;
    let mac_line_end = binary[mac_line_start + 1..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|offset| mac_line_start + 1 + offset + 1)
        .ok_or(EditorError::InvalidAgeFile {
            source: DecryptError::InvalidHeader,
        })?;
    let header_end = mac_line_end + PAYLOAD_NONCE_LEN;
    if binary.len() < header_end {
        return Err(EditorError::InvalidAgeFile {
            source: DecryptError::InvalidHeader,
        });
    }
    binary.truncate(header_end);
    Ok(binary)
}

pub(super) fn confirm_header_passphrase(
    header: &[u8],
    passphrase: Passphrase,
) -> Result<(), EditorError> {
    let identity = passphrase_identity(passphrase);
    let decryptor = Decryptor::new_buffered(header).map_err(header_error)?;
    // `decrypt` unwraps the file key and checks the header MAC; the payload
    // reader it returns is dropped unread.
    match decryptor.decrypt(iter::once(&identity as &dyn age::Identity)) {
        Ok(_) => Ok(()),
        Err(DecryptError::DecryptionFailed | DecryptError::NoMatchingKeys) => {
            Err(EditorError::PassphraseMismatch)
        }
        Err(other) => Err(unlock_error(other)),
    }
}

/// Maps errors that occur while parsing the header (no secret involved yet).
fn header_error(error: DecryptError) -> EditorError {
    match error {
        DecryptError::ExcessiveWork { required, .. } => EditorError::ExcessiveWorkFactor {
            required,
            maximum: MAXIMUM_ACCEPTED_WORK_FACTOR,
        },
        other => EditorError::InvalidAgeFile { source: other },
    }
}

/// Maps errors from unwrapping the file key with the passphrase.
///
/// `DecryptionFailed` here means the passphrase-derived key did not
/// authenticate the key stanza. A wrong passphrase and a damaged stanza look
/// identical, so both are reported as "could not be decrypted with that
/// passphrase". `InvalidMac` can only happen after the stanza authenticated,
/// so it means the header itself is damaged.
fn unlock_error(error: DecryptError) -> EditorError {
    match error {
        DecryptError::DecryptionFailed
        | DecryptError::KeyDecryptionFailed
        | DecryptError::NoMatchingKeys => EditorError::AuthenticationFailed { source: error },
        DecryptError::ExcessiveWork { required, .. } => EditorError::ExcessiveWorkFactor {
            required,
            maximum: MAXIMUM_ACCEPTED_WORK_FACTOR,
        },
        other => EditorError::InvalidAgeFile { source: other },
    }
}
