//! The general pasteboard. Policy (what we own, when to clear) lives in
//! `editage_core::clipboard::ClipboardTracker`; this module only performs
//! the pasteboard operations it decides on.

use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};

use crate::controls::ns;

/// The pasteboard's change counter. It increases whenever any application
/// writes to the clipboard, which is how "still our item" is decided
/// without reading clipboard contents.
pub fn change_count() -> i64 {
    NSPasteboard::generalPasteboard().changeCount() as i64
}

/// Empties the general pasteboard.
pub fn clear() {
    NSPasteboard::generalPasteboard().clearContents();
}

/// Copies non-document text (such as a file path). Not tracked, because it
/// is not document plaintext.
pub fn copy_non_document_text(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: NSPasteboardTypeString is a valid framework constant.
    pasteboard.setString_forType(&ns(text), unsafe { NSPasteboardTypeString });
}
