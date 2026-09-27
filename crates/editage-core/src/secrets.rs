//! Types that carry secret or plaintext material.
//!
//! Everything in this module is deliberately awkward to copy, print, or
//! persist. If a value from this module appears somewhere unexpected during
//! code review, that is worth a second look.
//!
//! Limits, stated plainly: these types request zeroization of the memory
//! *they* own when dropped. They cannot guarantee that no copy of a secret has
//! ever existed elsewhere in process memory (for example inside the operating
//! system's text system, the allocator's freed pages, or a library's internal
//! buffers). See `docs/security-model.md`.

use std::fmt;

use secrecy::{ExposeSecret, SecretString};
use zeroize::{Zeroize, Zeroizing};

/// A passphrase typed by the user.
///
/// Requests zeroization of its buffer when dropped. It has no `Clone`
/// implementation; the only way to duplicate it is the explicitly named
/// [`Passphrase::duplicate_for_operation`], so every copy is visible at the
/// call site.
pub struct Passphrase(SecretString);

impl Passphrase {
    /// Takes ownership of a passphrase string and wipes the original buffer.
    ///
    /// The characters are copied into an exactly-sized secret allocation and
    /// the source `String` is zeroized before it is dropped. Converting the
    /// `String` directly could reallocate and leave an unwiped copy behind.
    pub fn from_string(mut typed: String) -> Self {
        let secret = SecretString::from(typed.as_str());
        typed.zeroize();
        Passphrase(secret)
    }

    pub fn is_empty(&self) -> bool {
        self.0.expose_secret().is_empty()
    }

    /// Number of characters. Used only for the "short password" hint.
    pub fn character_count(&self) -> usize {
        self.0.expose_secret().chars().count()
    }

    /// Whether two passphrases are identical (used for "confirm password").
    pub fn matches(&self, other: &Passphrase) -> bool {
        // Both values are held by this process already; a timing side channel
        // between two local buffers is not part of the threat model.
        self.0.expose_secret() == other.0.expose_secret()
    }

    /// Creates a second secret container holding the same passphrase.
    ///
    /// The `age` library takes ownership of the passphrase it encrypts or
    /// decrypts with, so an operation that must also leave the retained
    /// passphrase in place needs its own copy. That copy is itself zeroized
    /// when the operation finishes.
    pub fn duplicate_for_operation(&self) -> Passphrase {
        Passphrase(SecretString::from(self.0.expose_secret()))
    }

    pub(crate) fn into_age_secret(self) -> SecretString {
        self.0
    }
}

impl fmt::Debug for Passphrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Passphrase(<redacted>)")
    }
}

/// The secret needed to decrypt or encrypt a document.
///
/// Only passphrases exist in v1. Other methods (for example an OpenPGP key or
/// an age identity file) are expected to become further variants here, next to
/// a matching [`crate::crypto::EncryptionFormat`] variant.
pub enum Credential {
    Passphrase(Passphrase),
}

impl Credential {
    pub fn kind(&self) -> CredentialKind {
        match self {
            Credential::Passphrase(_) => CredentialKind::Passphrase,
        }
    }

    /// See [`Passphrase::duplicate_for_operation`].
    pub fn duplicate_for_operation(&self) -> Credential {
        match self {
            Credential::Passphrase(passphrase) => {
                Credential::Passphrase(passphrase.duplicate_for_operation())
            }
        }
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::Passphrase(_) => f.write_str("Credential::Passphrase(<redacted>)"),
        }
    }
}

/// What kind of secret the user must provide. Frontends use this to decide
/// which prompt to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Passphrase,
}

/// Decrypted document text.
///
/// The buffer is zeroized on drop. Its `Debug` output shows only the length.
/// Frontends receive a `Plaintext` after unlocking, copy it into their native
/// text control, and should drop it immediately afterwards.
pub struct Plaintext(Zeroizing<String>);

impl Plaintext {
    /// Wraps text taken from the editor, for example just before saving.
    /// The `String` is moved, not copied.
    pub fn from_string(text: String) -> Self {
        Plaintext(Zeroizing::new(text))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    pub fn byte_len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for Plaintext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Plaintext(<redacted: {} bytes>)", self.0.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_of_passphrase_does_not_contain_the_passphrase() {
        let passphrase = Passphrase::from_string("correct horse battery staple".to_owned());
        let printed = format!("{passphrase:?}");
        assert!(!printed.contains("horse"));
        assert!(printed.contains("redacted"));
    }

    #[test]
    fn debug_output_of_plaintext_shows_only_its_length() {
        let plaintext = Plaintext::from_string("password: hunter2".to_owned());
        let printed = format!("{plaintext:?}");
        assert!(!printed.contains("hunter2"));
        assert!(printed.contains("17 bytes"));
    }

    #[test]
    fn debug_output_of_credential_is_redacted() {
        let credential = Credential::Passphrase(Passphrase::from_string("s3cret".to_owned()));
        assert!(!format!("{credential:?}").contains("s3cret"));
    }

    #[test]
    fn passphrase_comparison_detects_mismatch() {
        let a = Passphrase::from_string("one".to_owned());
        let b = Passphrase::from_string("two".to_owned());
        let c = Passphrase::from_string("one".to_owned());
        assert!(!a.matches(&b));
        assert!(a.matches(&c));
    }
}
