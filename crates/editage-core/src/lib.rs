//! Portable core of Editage, a native text editor for age-encrypted files.
//!
//! This crate contains everything that decides what happens to a document:
//! encryption, the document state machine, the save transaction, clipboard
//! policy, preferences, and the user-facing wording of reports. It has no
//! user interface code. A frontend (the macOS AppKit app today; potentially
//! Windows or GTK frontends later) owns windows and the native text control,
//! and asks this crate what to do and what to say.
//!
//! Module guide:
//! - [`secrets`]: passphrase, credential and plaintext types (redacted Debug,
//!   zeroized on drop).
//! - [`crypto`]: format detection, encryption and decryption (age today).
//! - [`storage`]: reading files and the narrow filesystem boundary used by
//!   saving.
//! - [`save`]: the staged save transaction.
//! - [`document`]: the per-document state machine.
//! - [`security_state`]: the Security Inspector's content.
//! - [`presentation`]: failure and notice wording.
//! - [`clipboard`], [`preferences`], [`diagnostics`]: application-wide state.
//! - [`error`]: the structured error type.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod clipboard;
pub mod crypto;
pub mod diagnostics;
pub mod document;
pub mod error;
pub mod preferences;
pub mod presentation;
pub mod save;
pub mod secrets;
pub mod security_state;
pub mod storage;

pub use document::{open_encrypted_document, DocumentSession};
pub use error::EditorError;
pub use save::run_save_transaction;
pub use security_state::inspect_document_state;

#[cfg(all(feature = "test-support", not(debug_assertions)))]
compile_error!(
    "The `test-support` feature lowers the scrypt work factor and must never be enabled in release builds."
);
