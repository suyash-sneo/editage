//! Password sheets: unlock, create (new document / change password), and
//! re-enter (the "ask again when saving" policy).
//!
//! Password fields are native `NSSecureTextField`s. When a password is read,
//! it is moved into a `Passphrase` (which zeroizes its own buffer) and the
//! field is emptied. The field's previous `NSString` is managed by AppKit and
//! cannot be zeroized by this application; see docs/security-model.md.

use std::cell::RefCell;
use std::rc::Rc;

use editage_core::secrets::Passphrase;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSButton, NSControlStateValueOn, NSProgressIndicator, NSProgressIndicatorStyle,
    NSSecureTextField, NSStackView, NSTextField, NSWindow,
};
use objc2_foundation::MainThreadMarker;

use crate::controls::{
    button, checkbox, error_label, hstack, key_label, make_cancel, make_default, ns,
    secondary_label, small_label, title_label, vstack, FieldObserver,
};
use crate::sheets::{button_row, details_disclosure, details_grid, Sheet, SHEET_WIDTH};

const TEXT_WIDTH: f64 = SHEET_WIDTH - 40.0;

fn secure_field(
    placeholder: &str,
    width: f64,
    mtm: MainThreadMarker,
) -> Retained<NSSecureTextField> {
    let field = NSSecureTextField::new(mtm);
    field.setPlaceholderString(Some(&ns(placeholder)));
    field
        .widthAnchor()
        .constraintEqualToConstant(width)
        .setActive(true);
    field
}

/// Moves a password out of a field into a zeroizing container and empties
/// the field.
fn take_passphrase(field: &NSTextField) -> Passphrase {
    let passphrase = Passphrase::from_string(field.stringValue().to_string());
    field.setStringValue(&ns(""));
    passphrase
}

fn field_is_empty(field: &NSTextField) -> bool {
    field.stringValue().length() == 0
}

// ---------------------------------------------------------------------------
// Unlock

pub struct UnlockSheet {
    pub sheet: Rc<Sheet>,
    field: Retained<NSSecureTextField>,
    error: Retained<NSTextField>,
    details: Retained<NSStackView>,
    disclosure: Retained<NSStackView>,
    spinner: Retained<NSProgressIndicator>,
    unlock_button: Retained<NSButton>,
    remember: Retained<NSButton>,
    observer: Retained<FieldObserver>,
}

impl UnlockSheet {
    pub fn close(&self) {
        self.sheet.close();
    }

    /// Shows or hides the "Decrypting…" state and disables input meanwhile.
    pub fn set_busy(&self, busy: bool) {
        self.field.setEnabled(!busy);
        self.unlock_button.setEnabled(!busy);
        self.remember.setEnabled(!busy);
        self.spinner.setHidden(!busy);
        if busy {
            // SAFETY: plain animation call on the main thread.
            unsafe { self.spinner.startAnimation(None) };
        } else {
            // SAFETY: plain animation call on the main thread.
            unsafe { self.spinner.stopAnimation(None) };
        }
    }

    /// Shows an unlock failure below the field, with technical details under
    /// a disclosure.
    pub fn show_error(&self, message: &str, details: &[(String, String)], mtm: MainThreadMarker) {
        self.set_busy(false);
        self.error.setStringValue(&ns(message));
        self.error.setHidden(false);
        for view in self.details.arrangedSubviews().iter() {
            self.details.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        self.details
            .addArrangedSubview(&details_grid(details, TEXT_WIDTH - 150.0, mtm));
        self.disclosure.setHidden(details.is_empty());
        self.sheet.fit();
        self.sheet.make_first_responder(&self.field);
        let _ = &self.observer;
    }
}

/// The design's unlock sheet.
pub fn present_unlock_sheet(
    window: &NSWindow,
    document_name: &str,
    lock_reason: Option<String>,
    remember_by_default: bool,
    mtm: MainThreadMarker,
    on_unlock: impl Fn(Passphrase, bool) + 'static,
    on_cancel: impl Fn() + 'static,
) -> Rc<UnlockSheet> {
    let sheet = Rc::new(Sheet::new(window, mtm));
    sheet.add(&title_label(
        &format!("Unlock “{document_name}”"),
        TEXT_WIDTH,
        mtm,
    ));
    sheet.add(&small_label(
        "Enter the password used to encrypt this file.",
        TEXT_WIDTH,
        mtm,
    ));
    if let Some(reason) = lock_reason {
        sheet.add(&secondary_label(&reason, TEXT_WIDTH, mtm));
    }

    let field = secure_field("Password", TEXT_WIDTH, mtm);
    sheet.add(&field);

    let error = error_label("", TEXT_WIDTH, mtm);
    error.setHidden(true);
    sheet.add(&error);
    let details = vstack(&[], 6.0, mtm);
    let disclosure = details_disclosure(&sheet, &details, mtm);
    disclosure.setHidden(true);
    sheet.add(&disclosure);
    sheet.add(&details);

    let remember = checkbox(
        "Remember password until this document is locked or closed",
        remember_by_default,
        &sheet.bag,
        mtm,
        |_| {},
    );
    sheet.add(&remember);
    let explanation = secondary_label(
        "When off, the password is released right after unlocking and asked for again when you save.",
        TEXT_WIDTH,
        mtm,
    );
    sheet.add(&explanation);

    let spinner = NSProgressIndicator::new(mtm);
    spinner.setStyle(NSProgressIndicatorStyle::Spinning);
    spinner.setControlSize(objc2_app_kit::NSControlSize::Small);
    spinner.setDisplayedWhenStopped(false);
    spinner.setHidden(true);

    let on_unlock = Rc::new(on_unlock);
    let state: Rc<RefCell<Option<Rc<UnlockSheet>>>> = Default::default();

    let unlock_state = state.clone();
    let unlock_button = button("Unlock", &sheet.bag, mtm, move || {
        let Some(this) = unlock_state.borrow().clone() else {
            return;
        };
        if field_is_empty(&this.field) {
            return;
        }
        let remember = this.remember.state() == NSControlStateValueOn;
        let passphrase = take_passphrase(&this.field);
        this.error.setHidden(true);
        this.set_busy(true);
        on_unlock(passphrase, remember);
    });
    make_default(&unlock_button);
    unlock_button.setEnabled(false);

    let cancel_button = button("Cancel", &sheet.bag, mtm, on_cancel);
    make_cancel(&cancel_button);

    let row = button_row(&[&spinner], &[&cancel_button, &unlock_button], mtm);
    sheet.add(&row);

    let enable_state = state.clone();
    let observer = FieldObserver::new(mtm, move || {
        if let Some(this) = enable_state.borrow().as_ref() {
            this.unlock_button.setEnabled(!field_is_empty(&this.field));
        }
    });
    observer.observe(&field);

    let this = Rc::new(UnlockSheet {
        sheet: sheet.clone(),
        field: field.clone(),
        error,
        details,
        disclosure,
        spinner,
        unlock_button,
        remember,
        observer,
    });
    *state.borrow_mut() = Some(this.clone());
    sheet.show();
    sheet.make_first_responder(&field);
    this
}

// ---------------------------------------------------------------------------
// Create a new password (first save, or Change Encryption Password)

pub struct NewPasswordText {
    pub title: String,
    pub message: String,
    pub confirm_button: &'static str,
}

struct NewPasswordFields {
    secure: [Retained<NSSecureTextField>; 2],
    plain: [Retained<NSTextField>; 2],
    showing_plain: bool,
    mismatch: Retained<NSTextField>,
    weak: Retained<NSTextField>,
    confirm_button: Retained<NSButton>,
}

impl NewPasswordFields {
    fn active(&self, index: usize) -> &NSTextField {
        if self.showing_plain {
            &self.plain[index]
        } else {
            &self.secure[index]
        }
    }

    fn refresh(&self) {
        let first = self.active(0).stringValue();
        let second = self.active(1).stringValue();
        let both_entered = first.length() > 0 && second.length() > 0;
        let matching = first.isEqualToString(&second);
        self.mismatch.setHidden(!both_entered || matching);
        self.weak
            .setHidden(!(first.length() > 0 && first.length() < 10));
        self.confirm_button
            .setEnabled(first.length() > 0 && matching);
    }
}

/// The design's "Create a password" sheet.
pub fn present_new_password_sheet(
    window: &NSWindow,
    text: NewPasswordText,
    mtm: MainThreadMarker,
    on_confirm: impl Fn(Passphrase) + 'static,
    on_cancel: impl Fn() + 'static,
) -> Rc<Sheet> {
    let sheet = Rc::new(Sheet::new(window, mtm));
    sheet.add(&title_label(&text.title, TEXT_WIDTH, mtm));
    sheet.add(&small_label(&text.message, TEXT_WIDTH, mtm));

    let field_width = TEXT_WIDTH - 130.0;
    let secure = [
        secure_field("", field_width, mtm),
        secure_field("", field_width, mtm),
    ];
    let plain = [NSTextField::new(mtm), NSTextField::new(mtm)];
    for field in &plain {
        field.setHidden(true);
        field
            .widthAnchor()
            .constraintEqualToConstant(field_width)
            .setActive(true);
    }
    let first_row = hstack(
        &[&key_label("Password:", mtm), &secure[0], &plain[0]],
        8.0,
        mtm,
    );
    let second_row = hstack(
        &[&key_label("Confirm Password:", mtm), &secure[1], &plain[1]],
        8.0,
        mtm,
    );
    for row in [&first_row, &second_row] {
        if let Some(label) = row.arrangedSubviews().iter().next() {
            label
                .widthAnchor()
                .constraintEqualToConstant(120.0)
                .setActive(true);
        }
    }
    sheet.add(&first_row);
    sheet.add(&second_row);

    let fields: Rc<RefCell<Option<NewPasswordFields>>> = Default::default();

    let toggle_fields = fields.clone();
    let show = checkbox("Show password", false, &sheet.bag, mtm, move |show| {
        if let Some(fields) = toggle_fields.borrow_mut().as_mut() {
            for index in 0..2 {
                let (from, to): (&NSTextField, &NSTextField) = if show {
                    (&fields.secure[index], &fields.plain[index])
                } else {
                    (&fields.plain[index], &fields.secure[index])
                };
                to.setStringValue(&from.stringValue());
                from.setStringValue(&ns(""));
                from.setHidden(true);
                to.setHidden(false);
            }
            fields.showing_plain = show;
        }
    });
    let show_row = hstack(&[&show], 0.0, mtm);
    show_row.setEdgeInsets(objc2_foundation::NSEdgeInsets {
        top: 0.0,
        left: 128.0,
        bottom: 0.0,
        right: 0.0,
    });
    sheet.add(&show_row);

    let mismatch = error_label("The passwords do not match.", TEXT_WIDTH, mtm);
    mismatch.setHidden(true);
    let weak = secondary_label(
        "This password is short. Longer passwords are harder to guess.",
        TEXT_WIDTH,
        mtm,
    );
    weak.setHidden(true);
    sheet.add(&mismatch);
    sheet.add(&weak);
    sheet.add(&secondary_label(
        "This password cannot be recovered by the application.",
        TEXT_WIDTH,
        mtm,
    ));

    let confirm_fields = fields.clone();
    let confirm_sheet = Rc::downgrade(&sheet);
    let confirm_button = button(text.confirm_button, &sheet.bag, mtm, move || {
        let passphrase = {
            let guard = confirm_fields.borrow();
            let Some(fields) = guard.as_ref() else {
                return;
            };
            let first = take_passphrase(fields.active(0));
            let second = take_passphrase(fields.active(1));
            if first.is_empty() || !first.matches(&second) {
                fields.refresh();
                return;
            }
            first
        };
        if let Some(sheet) = confirm_sheet.upgrade() {
            sheet.close();
        }
        on_confirm(passphrase);
    });
    make_default(&confirm_button);
    confirm_button.setEnabled(false);

    let cancel_sheet = Rc::downgrade(&sheet);
    let cancel_fields = fields.clone();
    let cancel_button = button("Cancel", &sheet.bag, mtm, move || {
        if let Some(fields) = cancel_fields.borrow().as_ref() {
            for index in 0..2 {
                fields.secure[index].setStringValue(&ns(""));
                fields.plain[index].setStringValue(&ns(""));
            }
        }
        if let Some(sheet) = cancel_sheet.upgrade() {
            sheet.close();
        }
        on_cancel();
    });
    make_cancel(&cancel_button);
    sheet.add(&button_row(&[], &[&*cancel_button, &*confirm_button], mtm));

    let refresh_fields = fields.clone();
    let observer = FieldObserver::new(mtm, move || {
        if let Some(fields) = refresh_fields.borrow().as_ref() {
            fields.refresh();
        }
    });
    for field in secure.iter() {
        observer.observe(field);
    }
    for field in plain.iter() {
        observer.observe(field);
    }
    // Text fields reference their delegate weakly; keep it with the sheet.
    sheet.bag.keep_object(Retained::into_super(observer));

    *fields.borrow_mut() = Some(NewPasswordFields {
        secure: secure.clone(),
        plain,
        showing_plain: false,
        mismatch,
        weak,
        confirm_button,
    });
    sheet.show();
    sheet.make_first_responder(&secure[0]);
    sheet
}

// ---------------------------------------------------------------------------
// Re-enter the current password

/// Asks once for the document's current password (used when it is not
/// retained). The entered password is checked against the document before
/// anything is written.
pub fn present_current_password_sheet(
    window: &NSWindow,
    title: &str,
    message: &str,
    confirm_title: &str,
    mtm: MainThreadMarker,
    on_confirm: impl Fn(Passphrase) + 'static,
    on_cancel: impl Fn() + 'static,
) -> Rc<Sheet> {
    let sheet = Rc::new(Sheet::new(window, mtm));
    sheet.add(&title_label(title, TEXT_WIDTH, mtm));
    sheet.add(&small_label(message, TEXT_WIDTH, mtm));
    let field = secure_field("Password", TEXT_WIDTH, mtm);
    sheet.add(&field);

    let confirm_field = field.clone();
    let confirm_sheet = Rc::downgrade(&sheet);
    let confirm_button = button(confirm_title, &sheet.bag, mtm, move || {
        if field_is_empty(&confirm_field) {
            return;
        }
        let passphrase = take_passphrase(&confirm_field);
        if let Some(sheet) = confirm_sheet.upgrade() {
            sheet.close();
        }
        on_confirm(passphrase);
    });
    make_default(&confirm_button);

    let cancel_field = field.clone();
    let cancel_sheet = Rc::downgrade(&sheet);
    let cancel_button = button("Cancel", &sheet.bag, mtm, move || {
        cancel_field.setStringValue(&ns(""));
        if let Some(sheet) = cancel_sheet.upgrade() {
            sheet.close();
        }
        on_cancel();
    });
    make_cancel(&cancel_button);
    sheet.add(&button_row(&[], &[&*cancel_button, &*confirm_button], mtm));
    sheet.show();
    sheet.make_first_responder(&field);
    sheet
}
