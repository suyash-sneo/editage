//! One document window and every user flow on it: unlock, edit, save,
//! Save As, lock, close, reload, change password.
//!
//! The window owns a `DocumentSession` (the core state machine) and a native
//! text view. Every decision is made by the session; this file performs the
//! AppKit side of each transition and shows what the session reports.
//! Slow work runs through `background::run_in_background`; results come back
//! on the main thread and are applied with the session's `finish_*` calls.

use std::cell::{Cell, Ref, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use block2::RcBlock;
use editage_core::diagnostics::DiagnosticLog;
use editage_core::document::{
    DocumentId, DocumentState, EditorCleared, ExternalChangeStatus, LockReadiness, LockReason,
    PassphrasePolicy, SaveCompletion, SaveCredential, SaveCredentialNeed, SaveStart, SaveTarget,
    UnlockOutcome, UnsavedChangesDecision,
};
use editage_core::presentation::{
    cleanup_warning_report, error_details, external_change_report, lock_reason_text,
    open_notice_text, save_failure_report, unlock_failure_message, FailureAction, FailureReport,
};
use editage_core::save::{run_save_transaction, CleanupResult, SaveResult};
use editage_core::secrets::{Credential, Passphrase, Plaintext};
use editage_core::storage::{FileSystemStorage, OpenNotice};
use editage_core::DocumentSession;
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSModalResponseOK, NSSavePanel, NSScrollView, NSTextField,
    NSTextFinderBarContainer, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle, NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSPoint, NSRect, NSSize, NSURL};
use objc2_uniform_type_identifiers::UTType;

use crate::app::app;
use crate::background::{on_main_queue, run_in_background};
use crate::controls::{
    button, format_time, make_cancel, make_default, ns, small_label, title_label, ActionTarget,
    TargetBag, WindowObserver, WindowObserverCallbacks,
};
use crate::editor_view::{make_editor, EditorCallbacks, EditorTextView};
use crate::info_popover::InfoPopover;
use crate::password_sheet::{
    present_current_password_sheet, present_new_password_sheet, present_unlock_sheet,
    NewPasswordText, UnlockSheet,
};
use crate::sheets::{
    ask_save_changes, button_row, confirm, confirm_notice, present_report_sheet, SaveChoice, Sheet,
    SHEET_WIDTH,
};
use crate::toolbar::{ToolbarDelegate, ToolbarItemSpec};

/// A save operation as the user requested it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SaveAttempt {
    CurrentFile,
    ChosenPath(PathBuf),
    ChangePassword,
}

/// What to do once a save completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterSave {
    Nothing,
    Close,
    Lock,
}

pub struct DocumentWindow {
    id: DocumentId,
    mtm: MainThreadMarker,
    session: RefCell<DocumentSession>,
    window: Retained<NSWindow>,
    scroll_view: Retained<NSScrollView>,
    editor: Retained<EditorTextView>,
    bag: TargetBag,
    observer: RefCell<Option<Retained<WindowObserver>>>,
    toolbar_delegate: RefCell<Option<Retained<ToolbarDelegate>>>,
    unlock_sheet: RefCell<Option<Rc<UnlockSheet>>>,
    sheet: RefCell<Option<Rc<Sheet>>>,
    info: RefCell<Option<InfoPopover>>,
    after_save: Cell<AfterSave>,
    /// The save operation last started, so "Try Again" repeats exactly that
    /// operation (never silently a different one, such as saving over the
    /// original after a failed Save As).
    last_attempt: RefCell<Option<SaveAttempt>>,
    closing: Cell<bool>,
    checking_external: Cell<bool>,
    weak_self: RefCell<Weak<DocumentWindow>>,
}

fn age_content_type() -> Option<Retained<UTType>> {
    UTType::typeWithFilenameExtension(&ns("age"))
}

pub fn reveal_in_finder(path: &Path) {
    let url = NSURL::fileURLWithPath(&ns(&path.to_string_lossy()));
    let urls = NSArray::from_retained_slice(&[url]);
    NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
}

impl DocumentWindow {
    // ----- Construction ---------------------------------------------------

    /// A new, never-saved document.
    pub fn new_untitled(mtm: MainThreadMarker, diagnostics: DiagnosticLog) -> Rc<Self> {
        let policy = app().preferences().default_passphrase_policy;
        let session = DocumentSession::new_untitled(policy, diagnostics);
        let this = Self::build(session, mtm);
        this.scroll_view.setHidden(false);
        this.update_chrome();
        this.window.makeKeyAndOrderFront(None);
        this.window.makeFirstResponder(Some(&this.editor));
        this
    }

    /// A document that was opened (read and validated) but is still locked.
    pub fn from_locked_session(session: DocumentSession, mtm: MainThreadMarker) -> Rc<Self> {
        let this = Self::build(session, mtm);
        this.scroll_view.setHidden(true);
        this.update_chrome();
        this.window.makeKeyAndOrderFront(None);
        this
    }

    fn build(session: DocumentSession, mtm: MainThreadMarker) -> Rc<Self> {
        // SAFETY: standard designated initializer with valid arguments.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(760.0, 560.0)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the window is owned by this controller and closed
        // explicitly; AppKit must not release it on close.
        unsafe { window.setReleasedWhenClosed(false) };
        // Window restoration could persist state across launches; it is
        // never used for documents.
        window.setRestorable(false);
        window.setMinSize(NSSize::new(360.0, 240.0));
        window.setToolbarStyle(NSWindowToolbarStyle::Unified);
        if !window.setFrameUsingName(&ns("EditageDocumentWindow")) {
            window.center();
        }
        let offset = app().documents().len() as f64 * 22.0;
        if offset > 0.0 {
            let frame = window.frame();
            window.setFrameOrigin(NSPoint::new(
                frame.origin.x + offset,
                frame.origin.y - offset,
            ));
        }

        let (scroll_view, editor) = make_editor(mtm);
        window.setContentView(Some(&scroll_view));

        let this = Rc::new(DocumentWindow {
            id: session.id(),
            mtm,
            session: RefCell::new(session),
            window,
            scroll_view,
            editor,
            bag: TargetBag::default(),
            observer: RefCell::new(None),
            toolbar_delegate: RefCell::new(None),
            unlock_sheet: RefCell::new(None),
            sheet: RefCell::new(None),
            info: RefCell::new(None),
            after_save: Cell::new(AfterSave::Nothing),
            last_attempt: RefCell::new(None),
            closing: Cell::new(false),
            checking_external: Cell::new(false),
            weak_self: RefCell::new(Weak::new()),
        });
        *this.weak_self.borrow_mut() = Rc::downgrade(&this);
        this.install_callbacks();
        this.install_toolbar();
        this.apply_preferences();
        this
    }

    fn weak(&self) -> Weak<DocumentWindow> {
        self.weak_self.borrow().clone()
    }

    fn install_callbacks(self: &Rc<Self>) {
        let edit_target = self.weak();
        let copy_id = self.id;
        self.editor.set_callbacks(EditorCallbacks {
            on_edit: Some(Box::new(move || {
                if let Some(this) = edit_target.upgrade() {
                    this.on_edit();
                }
            })),
            on_copy: Some(Box::new(move || app().record_copy(copy_id))),
        });

        let close_target = self.weak();
        let key_target = self.weak();
        let will_close_id = self.id;
        let main_id = self.id;
        let observer = WindowObserver::new(
            self.mtm,
            WindowObserverCallbacks {
                should_close: Some(Box::new(move || match close_target.upgrade() {
                    Some(this) => this.handle_should_close(),
                    None => true,
                })),
                did_become_key: Some(Box::new(move || {
                    if let Some(this) = key_target.upgrade() {
                        this.on_became_key();
                    }
                })),
                did_become_main: Some(Box::new(move || app().note_main_document(main_id))),
                will_close: Some(Box::new(move || {
                    let id = will_close_id;
                    // Defer so AppKit finishes closing before the controller
                    // is dropped.
                    on_main_queue(move || app().document_closed(id));
                })),
            },
        );
        observer.attach(&self.window);
        *self.observer.borrow_mut() = Some(observer);
    }

    fn install_toolbar(self: &Rc<Self>) {
        let lock_target = self.weak();
        let info_target = self.weak();
        let items = vec![
            ToolbarItemSpec {
                identifier: "lock",
                label: "Lock",
                symbol: "lock",
                tooltip: "Lock Document (⌃⌘L)",
                target: self.bag.keep(ActionTarget::new(self.mtm, move || {
                    if let Some(this) = lock_target.upgrade() {
                        this.lock();
                    }
                })),
            },
            ToolbarItemSpec {
                identifier: "info",
                label: "Info",
                symbol: "info.circle",
                tooltip: "Document Info (⌘I)",
                target: self.bag.keep(ActionTarget::new(self.mtm, move || {
                    if let Some(this) = info_target.upgrade() {
                        this.toggle_info();
                    }
                })),
            },
        ];
        let delegate = ToolbarDelegate::new(self.mtm, items, true, false);
        let toolbar = delegate.make_toolbar("EditageDocumentToolbar", self.mtm);
        self.window.setToolbar(Some(&toolbar));
        *self.toolbar_delegate.borrow_mut() = Some(delegate);
    }

    // ----- Accessors -------------------------------------------------------

    pub fn id(&self) -> DocumentId {
        self.id
    }
    pub fn window(&self) -> &NSWindow {
        &self.window
    }
    pub fn session(&self) -> Ref<'_, DocumentSession> {
        self.session.borrow()
    }
    pub fn path(&self) -> Option<PathBuf> {
        self.session.borrow().path().map(Path::to_owned)
    }
    pub fn auto_lock_decision(
        &self,
        idle: std::time::Duration,
        lock_after: Option<std::time::Duration>,
    ) -> editage_core::document::AutoLockDecision {
        self.session
            .borrow_mut()
            .auto_lock_decision(idle, lock_after)
    }

    pub fn editor(&self) -> &EditorTextView {
        &self.editor
    }
    fn name(&self) -> String {
        self.session.borrow().display_name().to_owned()
    }
    fn has_sheet(&self) -> bool {
        self.sheet.borrow().is_some()
            || self.unlock_sheet.borrow().is_some()
            || self.window.attachedSheet().is_some()
    }

    // ----- Presentation ---------------------------------------------------

    pub fn apply_preferences(&self) {
        let preferences = app().preferences();
        let bytes = self.editor.utf8_length();
        self.editor
            .apply_preferences(&preferences, &self.scroll_view, bytes);
        if let Some(toolbar) = self.window.toolbar() {
            toolbar.setVisible(preferences.show_toolbar);
        }
    }

    /// Updates title, edited dot, subtitle and editor visibility from the
    /// session. Nothing here keeps its own state.
    pub fn update_chrome(&self) {
        let session = self.session.borrow();
        let modified = session.has_unsaved_changes();
        let title = if modified {
            format!("{} — Edited", session.display_name())
        } else {
            session.display_name().to_owned()
        };
        self.window.setTitle(&ns(&title));
        self.window.setDocumentEdited(modified);
        match session.path() {
            Some(path) => {
                let url = NSURL::fileURLWithPath(&ns(&path.to_string_lossy()));
                self.window.setRepresentedURL(Some(&url));
            }
            None => self.window.setRepresentedURL(None),
        }

        let subtitle = match session.state() {
            DocumentState::Locked => "Locked".to_owned(),
            DocumentState::Unlocking => "Decrypting…".to_owned(),
            DocumentState::Saving(_) => "Saving…".to_owned(),
            DocumentState::SaveFailed => "Not saved — the last save failed".to_owned(),
            DocumentState::Locking | DocumentState::Closed => String::new(),
            DocumentState::UnlockedClean | DocumentState::UnlockedModified => {
                match session.external_change() {
                    ExternalChangeStatus::Changed { .. } => {
                        "Changed on disk by another program".to_owned()
                    }
                    ExternalChangeStatus::Missing { .. } => {
                        "File no longer on disk at this path".to_owned()
                    }
                    _ if session.is_read_only() => "Read-only".to_owned(),
                    _ => match session.last_save() {
                        Some(record) => match session.binding() {
                            // Reloaded from disk after this app's last save:
                            // the text shown is the file's, not ours.
                            editage_core::document::FileBinding::OnDisk(file)
                                if file.last_synchronised > record.at =>
                            {
                                format!("Read from disk at {}", format_time(file.last_synchronised))
                            }
                            _ => format!("Saved locally at {}", format_time(record.at)),
                        },
                        None if session.is_never_saved() => {
                            "Not saved yet — exists only in memory".to_owned()
                        }
                        None => String::new(),
                    },
                }
            }
        };
        self.window.setSubtitle(&ns(&subtitle));
        self.scroll_view.setHidden(!session.is_unlocked());
    }

    fn changed(&self) {
        self.update_chrome();
        if let Some(info) = self.info.borrow().as_ref() {
            info.refresh(self);
        }
        app().state_changed();
    }

    pub fn focus(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    // ----- Editing ---------------------------------------------------------

    fn on_edit(&self) {
        let bytes = self.editor.utf8_length();
        let spelling = app().preferences().spell_checking_active(bytes);
        if self.editor.isContinuousSpellCheckingEnabled() != spelling {
            self.editor.setContinuousSpellCheckingEnabled(spelling);
        }
        let was_modified = self.session.borrow().has_unsaved_changes();
        if self.session.borrow_mut().record_edit(bytes).is_ok() && !was_modified {
            self.changed();
        }
    }

    // ----- Unlocking --------------------------------------------------------

    /// Shows any notices that need confirmation, then the unlock sheet.
    pub fn begin_unlock_flow(self: &Rc<Self>) {
        let pending: Vec<OpenNotice> = self
            .session
            .borrow()
            .notices()
            .iter()
            .filter(|notice| {
                !matches!(notice, OpenNotice::LargeDocument { .. })
                    || notice.requires_confirmation()
            })
            .cloned()
            .collect();
        self.confirm_notices(pending);
    }

    fn confirm_notices(self: &Rc<Self>, mut remaining: Vec<OpenNotice>) {
        if remaining.is_empty() {
            self.show_unlock_sheet();
            return;
        }
        let notice = remaining.remove(0);
        let text = open_notice_text(&notice);
        let weak = self.weak();
        // Read-only and network-volume notices are informational (OK only);
        // symbolic links, hard links and large files need Continue/Cancel.
        confirm_notice(
            &self.window,
            &text.title,
            &text.message.join("\n"),
            text.confirm_button,
            notice.requires_confirmation(),
            self.mtm,
            move |continued| {
                let Some(this) = weak.upgrade() else { return };
                if continued {
                    let remaining = remaining.clone();
                    let this_for_next = this.clone();
                    on_main_queue_local(move || this_for_next.confirm_notices(remaining));
                } else {
                    this.close_now(UnsavedChangesDecision::NoUnsavedChanges);
                }
            },
        );
    }

    fn show_unlock_sheet(self: &Rc<Self>) {
        let (name, reason) = {
            let session = self.session.borrow();
            (
                session.display_name().to_owned(),
                lock_reason_text(session.lock_reason()),
            )
        };
        let remember_default =
            app().preferences().default_passphrase_policy == PassphrasePolicy::KeepUntilLocked;
        let unlock_target = self.weak();
        let cancel_target = self.weak();
        let sheet = present_unlock_sheet(
            &self.window,
            &name,
            reason,
            remember_default,
            self.mtm,
            move |passphrase, remember| {
                if let Some(this) = unlock_target.upgrade() {
                    this.unlock_with(passphrase, remember);
                }
            },
            move || {
                if let Some(this) = cancel_target.upgrade() {
                    this.close_now(UnsavedChangesDecision::NoUnsavedChanges);
                }
            },
        );
        *self.unlock_sheet.borrow_mut() = Some(sheet);
    }

    fn unlock_with(&self, passphrase: Passphrase, remember: bool) {
        let policy = if remember {
            PassphrasePolicy::KeepUntilLocked
        } else {
            PassphrasePolicy::AskAgainWhenSaving
        };
        let job = self
            .session
            .borrow_mut()
            .begin_unlock(Credential::Passphrase(passphrase), policy);
        let job = match job {
            Ok(job) => job,
            Err(error) => {
                if let Some(sheet) = self.unlock_sheet.borrow().as_ref() {
                    sheet.show_error(
                        &unlock_failure_message(&error),
                        &error_details(&error),
                        self.mtm,
                    );
                }
                return;
            }
        };
        self.changed();
        let id = self.id;
        run_in_background(
            move || job.run(),
            move |outcome| {
                if let Some(this) = app().document(id) {
                    this.finish_unlock(outcome);
                }
                // Otherwise the window was closed meanwhile; the outcome
                // (and any plaintext in it) is dropped and zeroized here.
            },
        );
    }

    fn finish_unlock(&self, outcome: UnlockOutcome) {
        let result = self.session.borrow_mut().finish_unlock(outcome);
        match result {
            Ok(plaintext) => {
                self.show_plaintext(plaintext);
                if let Some(sheet) = self.unlock_sheet.borrow_mut().take() {
                    sheet.close();
                }
                self.window.makeFirstResponder(Some(&self.editor));
                if let Some(path) = self.path() {
                    app().note_recent_document(path);
                }
                self.changed();
                app().remember_open_documents();
            }
            Err(error) => {
                if let Some(sheet) = self.unlock_sheet.borrow().as_ref() {
                    sheet.show_error(
                        &unlock_failure_message(&error),
                        &error_details(&error),
                        self.mtm,
                    );
                }
                self.changed();
            }
        }
    }

    /// Places decrypted text in the editor and drops the Rust copy.
    fn show_plaintext(&self, plaintext: Plaintext) {
        // Turn spell checking off before inserting the text, then re-apply
        // preferences for the new size, so a large document is never handed
        // to the spelling service in full.
        self.editor.setContinuousSpellCheckingEnabled(false);
        self.editor.load_plaintext(&plaintext);
        drop(plaintext);
        self.editor.clear_undo_history();
        self.apply_preferences();
        self.scroll_view.setHidden(false);
    }

    // ----- Saving ------------------------------------------------------------

    /// File → Save (⌘S).
    pub fn save(self: &Rc<Self>) {
        let (unlocked, never_saved, read_only, externally_changed, need) = {
            let session = self.session.borrow();
            (
                session.is_unlocked(),
                session.is_never_saved(),
                session.is_read_only(),
                matches!(
                    session.external_change(),
                    ExternalChangeStatus::Changed { .. } | ExternalChangeStatus::Missing { .. }
                ),
                session.save_credential_need(),
            )
        };
        if !unlocked || self.has_sheet() {
            return;
        }
        if never_saved {
            self.save_as();
            return;
        }
        if read_only {
            self.show_simple_notice(
                "This document is read-only.",
                "It cannot be saved in place. Use Save As… to save a copy somewhere else.",
            );
            self.after_save.set(AfterSave::Nothing);
            return;
        }
        if externally_changed {
            self.show_external_change();
            return;
        }
        self.last_attempt.replace(Some(SaveAttempt::CurrentFile));
        match need {
            SaveCredentialNeed::UseRetained => {
                self.start_save(SaveTarget::CurrentFile, SaveCredential::Retained)
            }
            SaveCredentialNeed::AskForCurrentPassphrase => {
                self.ask_current_passphrase("Save", move |this, passphrase| {
                    this.start_save(
                        SaveTarget::CurrentFile,
                        SaveCredential::ReenteredCurrent(Credential::Passphrase(passphrase)),
                    )
                });
            }
            SaveCredentialNeed::AskForNewPassphrase => self.save_as(),
        }
    }

    /// File → Save As… (⇧⌘S).
    pub fn save_as(self: &Rc<Self>) {
        let (unlocked, name, never_saved) = {
            let session = self.session.borrow();
            (
                session.is_unlocked(),
                session.display_name().to_owned(),
                session.is_never_saved(),
            )
        };
        if !unlocked || self.has_sheet() || self.session.borrow().is_saving() {
            return;
        }
        let suggested = if never_saved {
            "Untitled.txt".to_owned()
        } else {
            name.strip_suffix(".age").unwrap_or(&name).to_owned()
        };
        let weak = self.weak();
        self.run_save_panel(&suggested, move |path| {
            if let Some(this) = weak.upgrade() {
                this.save_to_chosen_path(path);
            }
        });
    }

    /// Saves to a path chosen in the save dialog (Save As, or the first
    /// save of a new document), asking for whichever password is needed.
    fn save_to_chosen_path(self: &Rc<Self>, path: PathBuf) {
        let this = self;
        self.last_attempt
            .replace(Some(SaveAttempt::ChosenPath(path.clone())));
        let need = this.session.borrow().save_credential_need();
        match need {
            SaveCredentialNeed::UseRetained => {
                this.start_save(SaveTarget::ChosenPath(path), SaveCredential::Retained)
            }
            SaveCredentialNeed::AskForCurrentPassphrase => {
                this.ask_current_passphrase("Save", move |this, passphrase| {
                    this.start_save(
                        SaveTarget::ChosenPath(path.clone()),
                        SaveCredential::ReenteredCurrent(Credential::Passphrase(passphrase)),
                    )
                });
            }
            SaveCredentialNeed::AskForNewPassphrase => {
                let file_name = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let weak = this.weak();
                let sheet = present_new_password_sheet(
                    &this.window,
                    NewPasswordText {
                        title: format!("Create a password for “{file_name}”"),
                        message: "The file will be encrypted with this password.".to_owned(),
                        confirm_button: "Encrypt and Save",
                    },
                    this.mtm,
                    move |passphrase| {
                        if let Some(this) = weak.upgrade() {
                            this.sheet.borrow_mut().take();
                            this.start_save(
                                SaveTarget::ChosenPath(path.clone()),
                                SaveCredential::New(Credential::Passphrase(passphrase)),
                            );
                        }
                    },
                    {
                        let weak = this.weak();
                        move || {
                            if let Some(this) = weak.upgrade() {
                                this.sheet.borrow_mut().take();
                                this.after_save.set(AfterSave::Nothing);
                                app().cancel_quit();
                            }
                        }
                    },
                );
                *this.sheet.borrow_mut() = Some(sheet);
            }
        }
    }

    /// File → Change Encryption Password…
    pub fn change_password(self: &Rc<Self>) {
        let (can, name) = {
            let session = self.session.borrow();
            (
                session.is_unlocked()
                    && !session.is_never_saved()
                    && !session.is_saving()
                    && !session.is_read_only(),
                session.display_name().to_owned(),
            )
        };
        if !can || self.has_sheet() {
            return;
        }
        self.last_attempt.replace(Some(SaveAttempt::ChangePassword));
        let weak = self.weak();
        let cancel = self.weak();
        let sheet = present_new_password_sheet(
            &self.window,
            NewPasswordText {
                title: format!("Change the password for “{name}”"),
                message: "The whole document will be encrypted again with the new password and saved with the normal save process (staging file, then atomic replacement). The old password will no longer open this file.".to_owned(),
                confirm_button: "Re-encrypt and Save",
            },
            self.mtm,
            move |passphrase| {
                if let Some(this) = weak.upgrade() {
                    this.sheet.borrow_mut().take();
                    this.start_save(
                        SaveTarget::CurrentFile,
                        SaveCredential::New(Credential::Passphrase(passphrase)),
                    );
                }
            },
            move || {
                if let Some(this) = cancel.upgrade() {
                    this.sheet.borrow_mut().take();
                }
            },
        );
        *self.sheet.borrow_mut() = Some(sheet);
    }

    /// "Try Again" after a failed save: repeats the same operation. A retry
    /// of a password change asks for the new password again, because it is
    /// not kept after a failed attempt.
    fn retry_last_save(self: &Rc<Self>) {
        let attempt = self.last_attempt.borrow().clone();
        match attempt {
            Some(SaveAttempt::ChosenPath(path)) => self.save_to_chosen_path(path),
            Some(SaveAttempt::ChangePassword) => self.change_password(),
            Some(SaveAttempt::CurrentFile) | None => self.save(),
        }
    }

    fn ask_current_passphrase(
        self: &Rc<Self>,
        confirm_title: &'static str,
        then: impl Fn(&Rc<DocumentWindow>, Passphrase) + 'static,
    ) {
        let name = self.name();
        let (title, message) = if confirm_title == "Save" {
            (
                format!("Enter the password for “{name}”"),
                "The password is not kept in memory, so it is needed to encrypt the document again. It is checked against the document before anything is written.",
            )
        } else {
            (
                format!("Enter the password for “{name}”"),
                "The password is not kept in memory, so it is needed to decrypt the file on disk again.",
            )
        };
        let weak = self.weak();
        let cancel = self.weak();
        let sheet = present_current_password_sheet(
            &self.window,
            &title,
            message,
            confirm_title,
            self.mtm,
            move |passphrase| {
                if let Some(this) = weak.upgrade() {
                    this.sheet.borrow_mut().take();
                    then(&this, passphrase);
                }
            },
            move || {
                if let Some(this) = cancel.upgrade() {
                    this.sheet.borrow_mut().take();
                    this.after_save.set(AfterSave::Nothing);
                    app().cancel_quit();
                }
            },
        );
        *self.sheet.borrow_mut() = Some(sheet);
    }

    /// Shows the native save dialog. `.age` is appended when missing, and an
    /// existing file under the appended name is only replaced after asking.
    fn run_save_panel(self: &Rc<Self>, suggested: &str, on_path: impl Fn(PathBuf) + 'static) {
        let panel = NSSavePanel::savePanel(self.mtm);
        panel.setNameFieldStringValue(&ns(suggested));
        panel.setCanCreateDirectories(true);
        panel.setExtensionHidden(false);
        if let Some(age_type) = age_content_type() {
            panel.setAllowedContentTypes(&NSArray::from_retained_slice(&[age_type]));
            panel.setAllowsOtherFileTypes(false);
        }
        let accessory = small_label(
            "Format: age encrypted file (passphrase). “.age” is added to the name if missing.",
            360.0,
            self.mtm,
        );
        let holder = crate::controls::vstack(&[&accessory], 0.0, self.mtm);
        holder.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 8.0,
            left: 12.0,
            bottom: 8.0,
            right: 12.0,
        });
        holder.setFrameSize(holder.fittingSize());
        panel.setAccessoryView(Some(&holder));

        let panel_for_block = panel.clone();
        let weak = self.weak();
        let on_path = Rc::new(on_path);
        let handler = RcBlock::new(move |response| {
            if response != NSModalResponseOK {
                if let Some(this) = weak.upgrade() {
                    this.after_save.set(AfterSave::Nothing);
                }
                app().cancel_quit();
                return;
            }
            let Some(path) = panel_for_block
                .URL()
                .and_then(|url| url.path())
                .map(|p| PathBuf::from(p.to_string()))
            else {
                return;
            };
            let (path, appended) = ensure_age_extension(path);
            let on_path = on_path.clone();
            let weak = weak.clone();
            // Run after the panel's own sheet has finished closing.
            on_main_queue_local(move || {
                let Some(this) = weak.upgrade() else { return };
                if appended && path.exists() {
                    let file_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let on_path = on_path.clone();
                    let confirmed_path = path.clone();
                    confirm(
                        &this.window,
                        &format!("“{file_name}” already exists. Do you want to replace it?"),
                        "A file with the same name already exists in this folder. Replacing it will overwrite its current contents.",
                        "Replace",
                        true,
                        this.mtm,
                        move || on_path(confirmed_path.clone()),
                    );
                } else {
                    on_path(path.clone());
                }
            });
        });
        panel.beginSheetModalForWindow_completionHandler(&self.window, &handler);
    }

    /// Copies the editor text and starts the save transaction off the main
    /// thread.
    fn start_save(self: &Rc<Self>, target: SaveTarget, credential: SaveCredential) {
        // The one deliberate plaintext copy of a save: taken here, moved into
        // the job, zeroized right after encryption.
        let plaintext = self.editor.plaintext_for_saving();
        let start = self
            .session
            .borrow_mut()
            .begin_save(target, credential, plaintext);
        let job = match start {
            Ok(SaveStart::Started(job)) => job,
            Ok(SaveStart::QueuedBehindRunningSave) => return,
            Err(error) => {
                let report = FailureReport {
                    title: "The document could not be saved.".to_owned(),
                    message: vec![
                        "Nothing was written. Your edits are still open in memory.".to_owned(),
                        error.to_string(),
                    ],
                    details: error_details(&error),
                    actions: vec![FailureAction::Dismiss],
                };
                self.show_report(&report);
                return;
            }
        };
        self.changed();

        let id = self.id;
        run_in_background(
            move || {
                let mut report_stage = move |stage| {
                    on_main_queue(move || {
                        if let Some(this) = app().document(id) {
                            this.session.borrow_mut().record_save_stage(stage);
                            app().refresh_inspector();
                        }
                    });
                };
                run_save_transaction(job, &FileSystemStorage, &mut report_stage)
            },
            move |result| {
                if let Some(this) = app().document(id) {
                    this.finish_save(result);
                }
            },
        );
    }

    fn finish_save(self: &Rc<Self>, result: SaveResult) {
        let cleanup_warning = match &result {
            Ok(success) => match &success.cleanup {
                CleanupResult::Failed { path, error } => Some((path.clone(), error.to_string())),
                _ => None,
            },
            Err(_) => None,
        };
        let completion = self.session.borrow_mut().finish_save(result);
        let Ok(SaveCompletion {
            committed,
            follow_up_save_requested,
        }) = completion
        else {
            return;
        };
        self.changed();

        if !committed {
            self.show_save_failure();
            return;
        }
        if let Some(path) = self.path() {
            app().note_recent_document(path);
        }
        app().remember_open_documents();
        if let Some((path, error)) = cleanup_warning {
            self.show_cleanup_warning(&path, &error);
        }
        if follow_up_save_requested
            && self.session.borrow().save_credential_need() == SaveCredentialNeed::UseRetained
        {
            self.start_save(SaveTarget::CurrentFile, SaveCredential::Retained);
            return;
        }
        match self.after_save.replace(AfterSave::Nothing) {
            AfterSave::Nothing => {}
            AfterSave::Close => {
                if !self.session.borrow().has_unsaved_changes() {
                    self.close_now(UnsavedChangesDecision::NoUnsavedChanges);
                }
            }
            AfterSave::Lock => {
                if !self.session.borrow().has_unsaved_changes() {
                    self.do_lock(
                        UnsavedChangesDecision::NoUnsavedChanges,
                        LockReason::LockedByUser,
                    );
                }
            }
        }
    }

    fn show_save_failure(self: &Rc<Self>) {
        let report = {
            let session = self.session.borrow();
            let Some(failure) = session.last_failure() else {
                return;
            };
            let mut report = save_failure_report(failure, &session);
            if *self.last_attempt.borrow() == Some(SaveAttempt::ChangePassword) {
                report.message.push(
                    "The password was not changed. The file on disk still opens with the previous password."
                        .to_owned(),
                );
            }
            report
        };
        let weak = self.weak();
        let sheet = present_report_sheet(&self.window, &report, self.mtm, move |action| {
            let Some(this) = weak.upgrade() else { return };
            this.sheet.borrow_mut().take();
            this.session.borrow_mut().acknowledge_save_failure();
            this.changed();
            match action {
                FailureAction::TryAgain => this.retry_last_save(),
                FailureAction::SaveAs => this.save_as(),
                FailureAction::ReloadFromDisk => this.reload_from_disk(true),
                FailureAction::RevealStagingFile(path) => {
                    reveal_in_finder(&path);
                    this.after_save.set(AfterSave::Nothing);
                    app().cancel_quit();
                }
                FailureAction::RetryCleanup => this.retry_cleanup(),
                FailureAction::Cancel | FailureAction::Dismiss => {
                    this.after_save.set(AfterSave::Nothing);
                    app().cancel_quit();
                }
            }
        });
        *self.sheet.borrow_mut() = Some(sheet);
    }

    fn show_cleanup_warning(self: &Rc<Self>, path: &Path, error: &str) {
        let report = cleanup_warning_report(path, error);
        self.show_report(&report);
    }

    pub fn retry_cleanup(self: &Rc<Self>) {
        let still_present = self
            .session
            .borrow_mut()
            .retry_staging_cleanup(&FileSystemStorage);
        self.changed();
        if let Some(path) = still_present.first() {
            let error = self
                .session
                .borrow()
                .leftover_staging_files()
                .first()
                .map(|leftover| leftover.removal_error.clone())
                .unwrap_or_default();
            let mut report = cleanup_warning_report(path, &error);
            report.title = "The staging file still could not be removed.".to_owned();
            self.show_report(&report);
        }
    }

    /// Shows a report whose actions are handled generically.
    fn show_report(self: &Rc<Self>, report: &FailureReport) {
        if self.window.attachedSheet().is_some() {
            return;
        }
        let weak = self.weak();
        let sheet = present_report_sheet(&self.window, report, self.mtm, move |action| {
            let Some(this) = weak.upgrade() else { return };
            this.sheet.borrow_mut().take();
            match action {
                FailureAction::RevealStagingFile(path) => reveal_in_finder(&path),
                FailureAction::RetryCleanup => this.retry_cleanup(),
                FailureAction::TryAgain => this.retry_last_save(),
                FailureAction::SaveAs => this.save_as(),
                FailureAction::ReloadFromDisk => this.reload_from_disk(true),
                FailureAction::Cancel | FailureAction::Dismiss => {}
            }
        });
        *self.sheet.borrow_mut() = Some(sheet);
    }

    fn show_simple_notice(self: &Rc<Self>, title: &str, message: &str) {
        let report = FailureReport {
            title: title.to_owned(),
            message: vec![message.to_owned()],
            details: Vec::new(),
            actions: vec![FailureAction::Dismiss],
        };
        self.show_report(&report);
    }

    // ----- External changes ---------------------------------------------------

    fn on_became_key(self: &Rc<Self>) {
        app().refresh_inspector();
        self.check_for_external_change();
    }

    /// Compares the file on disk with the version this window last read or
    /// wrote, in the background, and reports a change if one is found.
    pub fn check_for_external_change(self: &Rc<Self>) {
        let job = {
            let session = self.session.borrow();
            if !session.is_unlocked() || session.is_saving() || self.checking_external.get() {
                return;
            }
            session.begin_external_check()
        };
        let Some(job) = job else { return };
        self.checking_external.set(true);
        let id = self.id;
        run_in_background(
            move || job.run(&FileSystemStorage),
            move |outcome| {
                if let Some(this) = app().document(id) {
                    this.checking_external.set(false);
                    let newly_changed = this.session.borrow_mut().finish_external_check(outcome);
                    this.changed();
                    if newly_changed {
                        this.show_external_change();
                    }
                }
            },
        );
    }

    fn show_external_change(self: &Rc<Self>) {
        let report = external_change_report(&self.session.borrow());
        self.show_report(&report);
    }

    // ----- Reload / Revert -------------------------------------------------------

    /// Revert to Saved and Reload From Disk: re-reads and decrypts the file
    /// on disk. The current edits are discarded only after confirmation.
    pub fn reload_from_disk(self: &Rc<Self>, confirm_first: bool) {
        let (has_changes, name, on_disk) = {
            let session = self.session.borrow();
            (
                session.has_unsaved_changes(),
                session.display_name().to_owned(),
                session.path().is_some(),
            )
        };
        if !on_disk || self.has_sheet() {
            return;
        }
        if confirm_first && has_changes {
            let weak = self.weak();
            confirm(
                &self.window,
                &format!("Replace the text in this window with the version of “{name}” on disk?"),
                "Your unsaved edits in this window will be discarded. The file on disk is decrypted again; nothing is written.",
                "Reload",
                true,
                self.mtm,
                move || {
                    if let Some(this) = weak.upgrade() {
                        let this_again = this.clone();
                        on_main_queue_local(move || this_again.perform_reload());
                    }
                },
            );
        } else {
            self.perform_reload();
        }
    }

    fn perform_reload(self: &Rc<Self>) {
        let need = self.session.borrow().save_credential_need();
        match need {
            SaveCredentialNeed::UseRetained => self.start_reload(SaveCredential::Retained),
            _ => self.ask_current_passphrase("Reload", |this, passphrase| {
                this.start_reload(SaveCredential::ReenteredCurrent(Credential::Passphrase(
                    passphrase,
                )))
            }),
        }
    }

    fn start_reload(self: &Rc<Self>, credential: SaveCredential) {
        let job = match self.session.borrow_mut().begin_reload(credential) {
            Ok(job) => job,
            Err(_) => return,
        };
        let id = self.id;
        run_in_background(
            move || job.run(),
            move |outcome| {
                if let Some(this) = app().document(id) {
                    this.finish_reload(outcome);
                }
            },
        );
    }

    fn finish_reload(self: &Rc<Self>, outcome: UnlockOutcome) {
        let result = self.session.borrow_mut().finish_reload(outcome);
        match result {
            Ok(plaintext) => self.show_plaintext(plaintext),
            Err(error) => {
                let report = FailureReport {
                    title: "The document could not be reloaded.".to_owned(),
                    message: vec![
                        unlock_failure_message(&error),
                        "Your current text is unchanged.".to_owned(),
                    ],
                    details: error_details(&error),
                    actions: vec![FailureAction::Dismiss],
                };
                self.show_report(&report);
            }
        }
        self.changed();
    }

    // ----- Locking ------------------------------------------------------------------

    /// File → Lock Document (⌃⌘L).
    pub fn lock(self: &Rc<Self>) {
        if self.has_sheet() {
            return;
        }
        let readiness = self.session.borrow().lock_readiness();
        match readiness {
            LockReadiness::Ready => self.do_lock(
                UnsavedChangesDecision::NoUnsavedChanges,
                LockReason::LockedByUser,
            ),
            LockReadiness::HasUnsavedChanges => {
                let name = self.name();
                let weak = self.weak();
                ask_save_changes(
                    &self.window,
                    &format!("Do you want to save the changes made to “{name}” before locking it?"),
                    self.mtm,
                    move |choice| {
                        let Some(this) = weak.upgrade() else { return };
                        let this_again = this.clone();
                        on_main_queue_local(move || match choice {
                            SaveChoice::Save => {
                                this_again.after_save.set(AfterSave::Lock);
                                this_again.save();
                            }
                            SaveChoice::DontSave => this_again.do_lock(
                                UnsavedChangesDecision::DiscardUnsavedChanges,
                                LockReason::LockedByUser,
                            ),
                            SaveChoice::Cancel => {}
                        });
                    },
                );
            }
            LockReadiness::SaveInProgress => self.after_save.set(AfterSave::Lock),
            LockReadiness::NeverSaved | LockReadiness::AlreadyLocked => {}
        }
    }

    /// Auto-lock after inactivity. Only called when the session decided
    /// `LockNow` (no unsaved changes).
    pub fn lock_for_inactivity(self: &Rc<Self>, minutes: u32) {
        if self.window.attachedSheet().is_some() {
            // A sheet is open (e.g. a save dialog). Try again next tick.
            return;
        }
        self.do_lock(
            UnsavedChangesDecision::NoUnsavedChanges,
            LockReason::Inactivity { minutes },
        );
    }

    fn do_lock(self: &Rc<Self>, decision: UnsavedChangesDecision, reason: LockReason) {
        if self
            .session
            .borrow_mut()
            .begin_lock(decision, reason)
            .is_err()
        {
            return;
        }
        if let Some(info) = self.info.borrow_mut().take() {
            info.close();
        }
        // Hide the find bar so its search field is not left on screen.
        self.scroll_view.setFindBarVisible(false);
        let undo_history_cleared = self.editor.clear_text_and_undo();
        self.scroll_view.setHidden(true);
        let _ = self.session.borrow_mut().finish_lock(EditorCleared {
            undo_history_cleared,
        });
        self.changed();
        self.show_unlock_sheet();
    }

    /// File → Forget Retained Passphrase.
    pub fn forget_passphrase(self: &Rc<Self>) {
        if !self.session.borrow().has_retained_passphrase() || self.has_sheet() {
            return;
        }
        let weak = self.weak();
        confirm(
            &self.window,
            "Forget Retained Passphrase",
            "The passphrase currently retained for this document will be released. You will need to enter it again the next time you save.",
            "Forget Passphrase",
            false,
            self.mtm,
            move || {
                if let Some(this) = weak.upgrade() {
                    this.session.borrow_mut().forget_retained_passphrase();
                    this.changed();
                }
            },
        );
    }

    // ----- Closing -------------------------------------------------------------------

    /// Starts closing, asking about unsaved changes when needed. Used by the
    /// close button, ⌘W, and Quit.
    pub fn request_close(self: &Rc<Self>) {
        if self.handle_should_close() {
            self.closing.set(true);
            self.window.close();
        }
    }

    /// `windowShouldClose:`. Returns true only when the window can close
    /// right away; otherwise the flow continues asynchronously.
    fn handle_should_close(self: &Rc<Self>) -> bool {
        if self.closing.get() {
            return true;
        }
        let (saving, dirty, name) = {
            let session = self.session.borrow();
            (
                session.is_saving(),
                session.close_needs_decision(),
                session.display_name().to_owned(),
            )
        };
        if saving {
            self.after_save.set(AfterSave::Close);
            return false;
        }
        if dirty {
            if self.window.attachedSheet().is_some() {
                return false;
            }
            let weak = self.weak();
            ask_save_changes(
                &self.window,
                &format!("Do you want to save the changes made to “{name}”?"),
                self.mtm,
                move |choice| {
                    let Some(this) = weak.upgrade() else { return };
                    let this_again = this.clone();
                    on_main_queue_local(move || match choice {
                        SaveChoice::Save => {
                            this_again.after_save.set(AfterSave::Close);
                            this_again.save();
                        }
                        SaveChoice::DontSave => {
                            this_again.close_now(UnsavedChangesDecision::DiscardUnsavedChanges)
                        }
                        SaveChoice::Cancel => app().cancel_quit(),
                    });
                },
            );
            return false;
        }
        self.release_for_close(UnsavedChangesDecision::NoUnsavedChanges);
        true
    }

    /// Clears the editor and ends the session (without closing the window).
    fn release_for_close(&self, decision: UnsavedChangesDecision) {
        self.closing.set(true);
        if let Some(sheet) = self.unlock_sheet.borrow_mut().take() {
            sheet.close();
        }
        if let Some(sheet) = self.sheet.borrow_mut().take() {
            sheet.close();
        }
        if let Some(info) = self.info.borrow_mut().take() {
            info.close();
        }
        self.window.saveFrameUsingName(&ns("EditageDocumentWindow"));
        let undo_history_cleared = self.editor.clear_text_and_undo();
        let _ = self.session.borrow_mut().close(
            decision,
            EditorCleared {
                undo_history_cleared,
            },
        );
    }

    /// Closes sheets that hold no unsaved work (unlock sheets, notices), so
    /// that quitting is not blocked by them.
    pub fn dismiss_sheets_for_quit(&self) {
        if let Some(sheet) = self.unlock_sheet.borrow_mut().take() {
            sheet.close();
        }
        if let Some(sheet) = self.sheet.borrow_mut().take() {
            sheet.close();
        }
        if let Some(attached) = self.window.attachedSheet() {
            self.window.endSheet(&attached);
        }
    }

    /// Ends the session and closes the window immediately.
    pub fn close_now(self: &Rc<Self>, decision: UnsavedChangesDecision) {
        self.release_for_close(decision);
        self.window.close();
    }

    // ----- Info popover, go to line, file actions -------------------------------------

    /// View → Document Info (⌘I).
    pub fn toggle_info(self: &Rc<Self>) {
        let existing = self.info.borrow_mut().take();
        if let Some(info) = existing {
            // A transient popover closes itself when the user clicks
            // elsewhere; only a visible one is toggled closed here.
            if info.is_shown() {
                info.close();
                return;
            }
        }
        if !self.session.borrow().is_unlocked() {
            return;
        }
        let info = InfoPopover::show(self);
        *self.info.borrow_mut() = Some(info);
    }

    /// View → Go to Line… (⌘L).
    pub fn go_to_line(self: &Rc<Self>) {
        if !self.session.borrow().is_unlocked() || self.has_sheet() {
            return;
        }
        let line_count = {
            let text = self.editor.plaintext_for_saving();
            text.as_str().split('\n').count()
        };
        let sheet = Rc::new(Sheet::new(&self.window, self.mtm));
        sheet.add(&title_label("Go to Line", SHEET_WIDTH - 40.0, self.mtm));
        let field = NSTextField::new(self.mtm);
        field.setPlaceholderString(Some(&ns(&format!("Line number (1–{line_count})"))));
        field
            .widthAnchor()
            .constraintEqualToConstant(SHEET_WIDTH - 40.0)
            .setActive(true);
        sheet.add(&field);

        let weak = self.weak();
        let go_field = field.clone();
        let go = button("Go", &sheet.bag, self.mtm, move || {
            let Some(this) = weak.upgrade() else { return };
            let line: usize = go_field
                .stringValue()
                .to_string()
                .trim()
                .parse()
                .unwrap_or(0);
            if let Some(sheet) = this.sheet.borrow_mut().take() {
                sheet.close();
            }
            if line > 0 {
                this.select_line(line);
            }
        });
        make_default(&go);
        let weak = self.weak();
        let cancel = button("Cancel", &sheet.bag, self.mtm, move || {
            if let Some(this) = weak.upgrade() {
                if let Some(sheet) = this.sheet.borrow_mut().take() {
                    sheet.close();
                }
            }
        });
        make_cancel(&cancel);
        sheet.add(&button_row(&[], &[&*cancel, &*go], self.mtm));
        sheet.show();
        sheet.make_first_responder(&field);
        *self.sheet.borrow_mut() = Some(sheet);
    }

    fn select_line(&self, line: usize) {
        // A short-lived copy to find the line's UTF-16 range; zeroized on drop.
        let text = self.editor.plaintext_for_saving();
        let mut start_utf16 = 0usize;
        let mut current_line = 1usize;
        let mut line_utf16_len = 0usize;
        for character in text.as_str().chars() {
            if current_line == line {
                if character == '\n' {
                    break;
                }
                line_utf16_len += character.len_utf16();
            } else {
                start_utf16 += character.len_utf16();
                if character == '\n' {
                    current_line += 1;
                }
            }
        }
        drop(text);
        let range = objc2_foundation::NSRange::new(start_utf16, line_utf16_len);
        self.window.makeFirstResponder(Some(&self.editor));
        self.editor.setSelectedRange(range);
        self.editor.scrollRangeToVisible(range);
    }

    pub fn reveal_file(&self) {
        if let Some(path) = self.path() {
            reveal_in_finder(&path);
        }
    }

    pub fn reveal_leftover_staging_file(&self) {
        let path = self
            .session
            .borrow()
            .leftover_staging_files()
            .first()
            .map(|leftover| leftover.path.clone())
            .or_else(|| {
                self.path().and_then(|path| {
                    editage_core::storage::find_staging_files(&path)
                        .into_iter()
                        .next()
                })
            });
        if let Some(path) = path {
            reveal_in_finder(&path);
        }
    }

    pub fn copy_path(&self) {
        if let Some(path) = self.path() {
            crate::clipboard::copy_non_document_text(&path.to_string_lossy());
        }
    }
}

/// Adds ".age" if the chosen name does not end with it.
fn ensure_age_extension(path: PathBuf) -> (PathBuf, bool) {
    let has_extension = path
        .extension()
        .map(|extension| extension.eq_ignore_ascii_case("age"))
        .unwrap_or(false);
    if has_extension {
        (path, false)
    } else {
        let mut name = path.as_os_str().to_owned();
        name.push(".age");
        (PathBuf::from(name), true)
    }
}

/// Runs a non-`Send` closure on the main queue after the current event
/// (used to leave AppKit callbacks such as alert completion handlers before
/// presenting another sheet). Only called on the main thread.
fn on_main_queue_local(work: impl FnOnce() + 'static) {
    crate::app::defer_on_main_thread(Box::new(work));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_extension_is_appended_once() {
        assert_eq!(
            ensure_age_extension(PathBuf::from("/a/passwords.txt")),
            (PathBuf::from("/a/passwords.txt.age"), true)
        );
        assert_eq!(
            ensure_age_extension(PathBuf::from("/a/passwords.txt.age")),
            (PathBuf::from("/a/passwords.txt.age"), false)
        );
    }
}
