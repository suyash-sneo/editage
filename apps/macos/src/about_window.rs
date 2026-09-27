//! Editage → About Editage, and the Help menu's compatibility and recovery
//! topics: how files are stored, and how to open them without this app.

use std::rc::Rc;

use editage_core::crypto::{ENCRYPTION_WORK_FACTOR, MAXIMUM_ACCEPTED_WORK_FACTOR};
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSFont, NSFontWeightRegular, NSFontWeightSemibold, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::controls::{label, ns, secondary_label, small_label, title_label, vstack};

const WIDTH: f64 = 480.0;

pub struct AboutWindow {
    window: Retained<NSWindow>,
}

impl AboutWindow {
    pub fn new(mtm: MainThreadMarker) -> Rc<AboutWindow> {
        // SAFETY: standard designated initializer with valid arguments.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, 520.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `AboutWindow`; not released on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setRestorable(false);
        window.setTitle(&ns("About Editage"));

        let text_width = WIDTH - 60.0;
        let name = label("Editage", mtm);
        // SAFETY: valid font weight constant.
        name.setFont(Some(&NSFont::systemFontOfSize_weight(20.0, unsafe {
            NSFontWeightSemibold
        })));
        let version = secondary_label(
            &format!("Version {}", env!("CARGO_PKG_VERSION")),
            text_width,
            mtm,
        );

        let bullets = [
            "Files are saved in the standard age format, encrypted with your password.".to_owned(),
            "While a document is unlocked, its text exists in this app’s memory. It is not intentionally written to any file.".to_owned(),
            "No plaintext autosave files or crash-recovery copies are created. If the app quits unexpectedly, unsaved edits are lost.".to_owned(),
            "Saving writes a complete encrypted staging file beside the document, then replaces the original in one step. If saving fails, the original file is kept.".to_owned(),
            "Locking or closing a document removes its text from the editor, clears its undo history, and releases the retained password.".to_owned(),
            "The app does not sync files. Dropbox, OneDrive, iCloud Drive, Syncthing or a USB drive can carry the encrypted file like any other file.".to_owned(),
            format!("Passwords are stretched with scrypt (work factor 2^{ENCRYPTION_WORK_FACTOR}, the reference age default). Files using up to 2^{MAXIMUM_ACCEPTED_WORK_FACTOR} can be opened."),
            "The password cannot be recovered by this app.".to_owned(),
            "The app makes no network connections and collects no telemetry.".to_owned(),
        ];
        let list = vstack(&[], 6.0, mtm);
        for bullet in bullets {
            list.addArrangedSubview(&small_label(&format!("•  {bullet}"), text_width, mtm));
        }

        let command = label("age --decrypt passwords.txt.age", mtm);
        // SAFETY: valid font weight constant.
        command.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            12.0,
            unsafe { NSFontWeightRegular },
        )));
        command.setDrawsBackground(true);
        command.setBackgroundColor(Some(&NSColor::textBackgroundColor()));

        let content = vstack(
            &[
                &name,
                &version,
                &title_label("How files are stored", text_width, mtm),
                &list,
                &title_label("Opening files without this app", text_width, mtm),
                &small_label("Any age-compatible tool can decrypt your files with the same password:", text_width, mtm),
                &command,
                &secondary_label("File format specification: https://age-encryption.org/v1", text_width, mtm),
                &secondary_label("Licensed under MIT or Apache-2.0. Source code and security documentation are in the project repository.", text_width, mtm),
            ],
            10.0,
            mtm,
        );
        content.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 22.0,
            left: 30.0,
            bottom: 26.0,
            right: 30.0,
        });
        window.setContentView(Some(&content));
        window.setContentSize(NSSize::new(WIDTH, content.fittingSize().height));
        window.center();
        Rc::new(AboutWindow { window })
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }
}
