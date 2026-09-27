//! The structured error type shared by every core operation.
//!
//! Errors stay structured (with the original OS or library error attached)
//! until they reach [`crate::presentation`], which is the only place that turns
//! them into sentences for people. No variant ever carries plaintext or a
//! passphrase.

use std::io;
use std::path::PathBuf;

use crate::document::DocumentState;

#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    #[error("could not open {path}")]
    FileOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not read {path}")]
    FileRead {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{path} is not a regular file")]
    NotARegularFile { path: PathBuf },

    #[error("{path} is {bytes} bytes, above the {limit} byte limit")]
    FileTooLarge {
        path: PathBuf,
        bytes: u64,
        limit: u64,
    },

    /// A cloud-storage placeholder whose contents are not present locally.
    #[error("{path} is a cloud placeholder that has not been downloaded")]
    CloudPlaceholder { path: PathBuf },

    /// The file is not in a format this version can read.
    #[error("unsupported file format: {detected}")]
    UnsupportedFormat { detected: String },

    /// The file is age, but protected by something other than a passphrase.
    #[error("unsupported protection: {description}")]
    UnsupportedProtection { description: String },

    /// The age header could not be parsed or is structurally invalid.
    #[error("the file is not a valid age file")]
    InvalidAgeFile {
        #[source]
        source: age::DecryptError,
    },

    /// Decrypting the file key with the supplied passphrase failed.
    ///
    /// With passphrase encryption, a wrong passphrase and a corrupted key
    /// stanza produce the same failure, so this does not claim which one it
    /// was.
    #[error("the file could not be decrypted with the supplied passphrase")]
    AuthenticationFailed {
        #[source]
        source: age::DecryptError,
    },

    /// The file key was recovered but the header or the payload failed
    /// authentication (the file is damaged or truncated).
    #[error("the encrypted contents are damaged or incomplete")]
    Decryption {
        #[source]
        source: io::Error,
    },

    #[error("the file requires scrypt work factor {required}; at most {maximum} is accepted")]
    ExcessiveWorkFactor { required: u8, maximum: u8 },

    #[error("encryption failed")]
    Encryption {
        #[source]
        source: io::Error,
    },

    /// The file decrypted successfully but is not UTF-8 text.
    #[error("decrypted contents are not valid UTF-8 (first invalid byte at offset {valid_up_to} of {byte_len})")]
    InvalidUtf8 { valid_up_to: usize, byte_len: usize },

    /// Saving needs the document's passphrase and none is retained.
    #[error("a passphrase is required")]
    PassphraseRequired,

    /// The passphrase entered at save time is not the one this document is
    /// encrypted with.
    #[error("the entered passphrase does not match this document's passphrase")]
    PassphraseMismatch,

    #[error("the folder {path} does not exist or is not accessible")]
    DestinationDirectoryUnavailable {
        path: PathBuf,
        #[source]
        source: Option<io::Error>,
    },

    #[error("{path} no longer exists")]
    DestinationMissing { path: PathBuf },

    #[error("{path} is read-only")]
    DestinationReadOnly { path: PathBuf },

    #[error("could not inspect {path}")]
    DestinationInspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("{path} changed on disk after it was opened")]
    ExternalModification { path: PathBuf },

    #[error("could not create staging file {path}")]
    StagingFileCreate {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not write staging file {path}")]
    StagingFileWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not flush staging file {path} to storage")]
    Flush {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not set permissions on {path}")]
    Permissions {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not replace {destination} with {staging}")]
    AtomicReplace {
        staging: PathBuf,
        destination: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("could not remove staging file {path}")]
    Cleanup {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("clipboard operation failed: {description}")]
    Clipboard { description: String },

    #[error("platform error: {description}")]
    Platform { description: String },

    #[error("{attempted} is not possible while the document is {from:?}")]
    InvalidTransition {
        from: DocumentState,
        attempted: &'static str,
    },
}

impl EditorError {
    /// The underlying OS error, if any, for "System error: …" detail rows.
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            EditorError::FileOpen { source, .. }
            | EditorError::FileRead { source, .. }
            | EditorError::Decryption { source }
            | EditorError::Encryption { source }
            | EditorError::DestinationInspect { source, .. }
            | EditorError::StagingFileCreate { source, .. }
            | EditorError::StagingFileWrite { source, .. }
            | EditorError::Flush { source, .. }
            | EditorError::Permissions { source, .. }
            | EditorError::AtomicReplace { source, .. }
            | EditorError::Cleanup { source, .. } => Some(source),
            EditorError::DestinationDirectoryUnavailable { source, .. } => source.as_ref(),
            _ => None,
        }
    }
}
