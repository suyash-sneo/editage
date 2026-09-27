//! Application-wide state and the application delegate.
//!
//! `App` owns the open document windows, preferences, the clipboard tracker,
//! the diagnostic log and the auxiliary windows. It lives in a main-thread
//! `thread_local`, because AppKit objects may only be used on the main
//! thread; background work reaches it by dispatching back to the main queue.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::rc::Rc;
use std::time::{Instant, SystemTime};

use block2::RcBlock;
use editage_core::clipboard::{
    ClipboardAction, ClipboardClearPolicy, ClipboardStatus, ClipboardTracker,
};
use editage_core::diagnostics::{DiagnosticEvent, DiagnosticLog};
use editage_core::document::{AutoLockDecision, DocumentId};
use editage_core::open_encrypted_document;
use editage_core::preferences::Preferences;
use editage_core::presentation::open_failure_report;
use editage_core::security_state::ApplicationFacts;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSEvent, NSEventMask, NSModalResponseOK, NSOpenPanel,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSNotification, NSObject, NSObjectProtocol, NSTimer, NSURL,
};
use objc2_uniform_type_identifiers::UTType;

use crate::about_window::AboutWindow;
use crate::background::{on_main_queue, run_in_background};
use crate::controls::{ns, TargetBag};
use crate::diagnostics_window::DiagnosticsWindow;
use crate::document_window::DocumentWindow;
use crate::preferences_store::{load_preferences, save_preferences};
use crate::security_inspector::SecurityInspector;
use crate::settings_window::SettingsWindow;
use crate::sheets::run_modal_report;
use crate::welcome_window::WelcomeWindow;

/// How the Security Inspector describes the spelling checker on macOS.
pub const SPELLING_SERVICE: &str = "the macOS spelling service on this Mac";

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
    static DEFERRED: RefCell<VecDeque<Box<dyn FnOnce()>>> = RefCell::new(VecDeque::new());
}

/// The application controller. Only available on the main thread.
pub fn app() -> Rc<App> {
    APP.with(|app| app.borrow().clone().expect("App is installed at launch"))
}

/// Runs a closure on the main thread after the current event has been
/// handled. Useful for leaving AppKit callbacks (alert completion handlers,
/// panel handlers) before presenting another sheet.
pub fn defer_on_main_thread(work: Box<dyn FnOnce()>) {
    DEFERRED.with(|queue| queue.borrow_mut().push_back(work));
    on_main_queue(|| {
        while let Some(work) = DEFERRED.with(|queue| queue.borrow_mut().pop_front()) {
            work();
        }
    });
}

pub struct App {
    pub mtm: MainThreadMarker,
    pub diagnostics: DiagnosticLog,
    preferences: RefCell<Preferences>,
    clipboard: RefCell<ClipboardTracker>,
    documents: RefCell<Vec<Rc<DocumentWindow>>>,
    welcome: RefCell<Option<Rc<WelcomeWindow>>>,
    settings: RefCell<Option<Rc<SettingsWindow>>>,
    about: RefCell<Option<Rc<AboutWindow>>>,
    inspector: RefCell<Option<Rc<SecurityInspector>>>,
    diagnostics_window: RefCell<Option<Rc<DiagnosticsWindow>>>,
    last_activity: Cell<Instant>,
    quit_requested: Cell<bool>,
    pub menu_bag: TargetBag,
    timer: RefCell<Option<Retained<NSTimer>>>,
    event_monitor: RefCell<Option<Retained<AnyObject>>>,
}

impl App {
    fn new(mtm: MainThreadMarker) -> App {
        let preferences = load_preferences();
        let clipboard = ClipboardTracker::new(preferences.clipboard_clear);
        App {
            mtm,
            diagnostics: DiagnosticLog::new(),
            preferences: RefCell::new(preferences),
            clipboard: RefCell::new(clipboard),
            documents: RefCell::new(Vec::new()),
            welcome: RefCell::new(None),
            settings: RefCell::new(None),
            about: RefCell::new(None),
            inspector: RefCell::new(None),
            diagnostics_window: RefCell::new(None),
            last_activity: Cell::new(Instant::now()),
            quit_requested: Cell::new(false),
            menu_bag: TargetBag::default(),
            timer: RefCell::new(None),
            event_monitor: RefCell::new(None),
        }
    }

    // ----- Documents -----------------------------------------------------

    pub fn documents(&self) -> Vec<Rc<DocumentWindow>> {
        self.documents.borrow().clone()
    }

    pub fn document(&self, id: DocumentId) -> Option<Rc<DocumentWindow>> {
        self.documents
            .borrow()
            .iter()
            .find(|document| document.id() == id)
            .cloned()
    }

    /// The document whose window is main (the one menus act on).
    pub fn key_document(&self) -> Option<Rc<DocumentWindow>> {
        let main_window = NSApplication::sharedApplication(self.mtm).mainWindow()?;
        self.documents
            .borrow()
            .iter()
            .find(|document| std::ptr::eq(document.window(), &*main_window))
            .cloned()
    }

    fn add_document(&self, document: Rc<DocumentWindow>) {
        self.documents.borrow_mut().push(document);
        if let Some(welcome) = self.welcome.borrow_mut().take() {
            welcome.close();
        }
        self.remember_open_documents();
        self.state_changed();
    }

    /// Called after a document window has closed.
    pub fn document_closed(&self, id: DocumentId) {
        let removed = {
            let mut documents = self.documents.borrow_mut();
            let index = documents.iter().position(|document| document.id() == id);
            index.map(|index| documents.remove(index))
        };
        drop(removed);
        if !self.quit_requested.get() {
            self.remember_open_documents();
        }
        self.state_changed();
        if self.quit_requested.get() {
            let mtm = self.mtm;
            defer_on_main_thread(Box::new(move || {
                NSApplication::sharedApplication(mtm).terminate(None);
            }));
        } else if self.documents.borrow().is_empty() {
            self.show_welcome();
        }
    }

    pub fn new_document(&self) {
        let document = DocumentWindow::new_untitled(self.mtm, self.diagnostics.clone());
        self.add_document(document);
    }

    /// File → Open…
    pub fn show_open_panel(&self) {
        let panel = NSOpenPanel::openPanel(self.mtm);
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setAllowsMultipleSelection(true);
        panel.setMessage(Some(&ns("Only .age files can be opened.")));
        if let Some(age_type) = UTType::typeWithFilenameExtension(&ns("age")) {
            panel.setAllowedContentTypes(&NSArray::from_retained_slice(&[age_type]));
        }
        if panel.runModal() != NSModalResponseOK {
            return;
        }
        let paths: Vec<PathBuf> = panel
            .URLs()
            .iter()
            .filter_map(|url| url.path())
            .map(|path| PathBuf::from(path.to_string()))
            .collect();
        for path in paths {
            self.open_path(path);
        }
    }

    /// Reads and validates a file in the background, then shows it locked
    /// with the unlock sheet.
    pub fn open_path(&self, path: PathBuf) {
        let already_open = self
            .documents()
            .into_iter()
            .find(|document| document.path().is_some_and(|open| same_file(&open, &path)));
        if let Some(document) = already_open {
            document.focus();
            return;
        }
        let policy = self.preferences().default_passphrase_policy;
        let diagnostics = self.diagnostics.clone();
        let path_for_report = path.clone();
        run_in_background(
            move || open_encrypted_document(&path, policy, diagnostics),
            move |result| {
                let app = app();
                match result {
                    Ok(session) => {
                        let document = DocumentWindow::from_locked_session(session, app.mtm);
                        app.add_document(document.clone());
                        document.begin_unlock_flow();
                    }
                    Err(error) => {
                        run_modal_report(&open_failure_report(&path_for_report, &error), app.mtm);
                        if app.documents.borrow().is_empty() {
                            app.show_welcome();
                        }
                    }
                }
            },
        );
    }

    // ----- Preferences ---------------------------------------------------

    pub fn preferences(&self) -> Preferences {
        self.preferences.borrow().clone()
    }

    /// Changes preferences, stores them, and applies them everywhere.
    pub fn update_preferences(&self, change: impl FnOnce(&mut Preferences)) {
        let updated = {
            let mut preferences = self.preferences.borrow_mut();
            change(&mut preferences);
            preferences.clone()
        };
        save_preferences(&updated);
        self.clipboard
            .borrow_mut()
            .set_policy(updated.clipboard_clear);
        for document in self.documents() {
            document.apply_preferences();
        }
        crate::menu::rebuild_open_recent(self);
        if let Some(welcome) = self.welcome.borrow().as_ref() {
            welcome.refresh();
        }
        if let Some(settings) = self.settings.borrow().as_ref() {
            settings.refresh();
        }
        self.refresh_inspector();
    }

    pub fn note_recent_document(&self, path: PathBuf) {
        self.update_preferences(|preferences| preferences.note_recent_document(path));
    }

    /// Records the paths of open documents for "Reopen documents" (only
    /// kept while that setting is on).
    pub fn remember_open_documents(&self) {
        let paths: Vec<PathBuf> = self.documents().iter().filter_map(|d| d.path()).collect();
        let changed = {
            let preferences = self.preferences.borrow();
            preferences.reopen_documents_at_launch && preferences.documents_to_reopen != paths
        };
        if changed {
            self.update_preferences(|preferences| preferences.set_open_documents(paths));
        }
    }

    // ----- Clipboard -------------------------------------------------------

    /// Called right after document text was copied or cut.
    pub fn record_copy(&self, document: DocumentId) {
        let change_count = crate::clipboard::change_count();
        self.clipboard
            .borrow_mut()
            .record_copy(change_count, document, SystemTime::now());
        let name = self
            .document(document)
            .map(|d| d.session().display_name().to_owned())
            .unwrap_or_default();
        self.diagnostics
            .record(&name, DiagnosticEvent::ClipboardWritten);
        self.refresh_inspector();
    }

    pub fn clipboard_status(&self) -> ClipboardStatus {
        self.clipboard
            .borrow_mut()
            .status(crate::clipboard::change_count())
    }

    pub fn can_clear_clipboard(&self) -> bool {
        self.clipboard
            .borrow_mut()
            .can_clear_now(crate::clipboard::change_count())
    }

    /// Edit → Clear Clipboard Now. Only clears our own item.
    pub fn clear_clipboard_now(&self) {
        if self.can_clear_clipboard() {
            crate::clipboard::clear();
            self.clipboard.borrow_mut().record_cleared();
            self.diagnostics
                .record_app_event(DiagnosticEvent::ClipboardClearedByUser);
            self.refresh_inspector();
        }
    }

    pub fn application_facts<R>(&self, use_facts: impl FnOnce(ApplicationFacts<'_>) -> R) -> R {
        let clipboard = self.clipboard_status();
        let preferences = self.preferences();
        use_facts(ApplicationFacts {
            preferences: &preferences,
            clipboard,
            spelling_service_description: SPELLING_SERVICE,
        })
    }

    // ----- Auxiliary windows ------------------------------------------------

    pub fn show_welcome(&self) {
        if self.welcome.borrow().is_none() {
            *self.welcome.borrow_mut() = Some(WelcomeWindow::new(self.mtm));
        }
        if let Some(welcome) = self.welcome.borrow().as_ref() {
            welcome.show();
        }
    }

    pub fn welcome_closed(&self) {
        self.welcome.borrow_mut().take();
    }

    pub fn show_settings(&self, security_tab: bool) {
        if self.settings.borrow().is_none() {
            *self.settings.borrow_mut() = Some(SettingsWindow::new(self.mtm));
        }
        if let Some(settings) = self.settings.borrow().as_ref() {
            settings.show(security_tab);
        }
    }

    pub fn show_about(&self) {
        if self.about.borrow().is_none() {
            *self.about.borrow_mut() = Some(AboutWindow::new(self.mtm));
        }
        if let Some(about) = self.about.borrow().as_ref() {
            about.show();
        }
    }

    pub fn show_security_inspector(&self) {
        if self.inspector.borrow().is_none() {
            *self.inspector.borrow_mut() = Some(SecurityInspector::new(self.mtm));
        }
        let inspector = self.inspector.borrow().clone();
        if let Some(inspector) = inspector {
            inspector.show();
            inspector.refresh(true);
        }
    }

    pub fn show_diagnostics(&self) {
        if self.diagnostics_window.borrow().is_none() {
            *self.diagnostics_window.borrow_mut() =
                Some(DiagnosticsWindow::new(self.mtm, self.diagnostics.clone()));
        }
        if let Some(window) = self.diagnostics_window.borrow().as_ref() {
            window.show();
        }
    }

    pub fn refresh_inspector(&self) {
        let inspector = self.inspector.borrow().clone();
        if let Some(inspector) = inspector {
            inspector.refresh(false);
        }
    }

    /// Something about a document changed; refresh everything that
    /// displays document state.
    pub fn state_changed(&self) {
        self.refresh_inspector();
    }

    // ----- Timer: clipboard, auto-lock, live inspector ---------------------

    fn start_timer(self: &Rc<Self>) {
        let block = RcBlock::new(|_timer: NonNull<NSTimer>| {
            app().tick();
        });
        // SAFETY: the block is 'static and runs on the main run loop.
        let timer =
            unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(1.0, true, &block) };
        *self.timer.borrow_mut() = Some(timer);

        // Any keyboard, mouse or scroll event in this app counts as activity
        // for "Lock after inactivity". Events are passed through unchanged.
        let monitor_block = RcBlock::new(|event: NonNull<NSEvent>| -> *mut NSEvent {
            app().last_activity.set(Instant::now());
            event.as_ptr()
        });
        let mask = NSEventMask::KeyDown
            | NSEventMask::LeftMouseDown
            | NSEventMask::RightMouseDown
            | NSEventMask::ScrollWheel
            | NSEventMask::MouseMoved;
        // SAFETY: the handler returns the event it received, as required.
        let monitor =
            unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &monitor_block) };
        *self.event_monitor.borrow_mut() = monitor;
    }

    fn tick(&self) {
        let now = SystemTime::now();

        // Clipboard: clear only our own item, only after its deadline.
        let action = self
            .clipboard
            .borrow_mut()
            .action_on_tick(crate::clipboard::change_count(), now);
        if action == ClipboardAction::ClearNow {
            crate::clipboard::clear();
            self.clipboard.borrow_mut().record_cleared();
            self.diagnostics
                .record_app_event(DiagnosticEvent::ClipboardClearedAutomatically);
        }

        // Auto-lock: the session decides; documents with unsaved changes
        // are postponed, never saved or discarded silently.
        let preferences = self.preferences();
        let idle = self.last_activity.get().elapsed();
        for document in self.documents() {
            let decision = document.auto_lock_decision(idle, preferences.lock_after_inactivity());
            if decision == AutoLockDecision::LockNow {
                let minutes = preferences.lock_after_inactivity_minutes.unwrap_or(0);
                document.lock_for_inactivity(minutes);
            }
        }

        self.refresh_inspector();
        if let Some(window) = self.diagnostics_window.borrow().as_ref() {
            window.refresh_if_changed();
        }
    }

    // ----- Quitting -----------------------------------------------------------

    pub fn cancel_quit(&self) {
        self.quit_requested.set(false);
    }

    fn should_terminate(&self) -> NSApplicationTerminateReply {
        let needs_review = self.documents().into_iter().find(|document| {
            let session = document.session();
            session.is_saving() || session.has_unsaved_changes()
        });
        match needs_review {
            Some(document) => {
                self.quit_requested.set(true);
                document.focus();
                document.request_close();
                NSApplicationTerminateReply::TerminateCancel
            }
            None => NSApplicationTerminateReply::TerminateNow,
        }
    }

    fn will_terminate(&self) {
        self.remember_open_documents_at_quit();
        // A clipboard item scheduled for clearing would otherwise outlive
        // the app. Clear it now, but only if it is still our item.
        if self.preferences().clipboard_clear != ClipboardClearPolicy::Never {
            self.clear_clipboard_now();
        }
        for document in self.documents() {
            document
                .close_now(editage_core::document::UnsavedChangesDecision::DiscardUnsavedChanges);
        }
    }

    fn remember_open_documents_at_quit(&self) {
        let paths: Vec<PathBuf> = self.documents().iter().filter_map(|d| d.path()).collect();
        self.update_preferences(|preferences| preferences.set_open_documents(paths));
    }

    fn did_finish_launching(self: &Rc<Self>) {
        let application = NSApplication::sharedApplication(self.mtm);
        application.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        crate::menu::install_main_menu(self);
        self.start_timer();

        let to_reopen = {
            let preferences = self.preferences.borrow();
            if preferences.reopen_documents_at_launch {
                preferences.documents_to_reopen.clone()
            } else {
                Vec::new()
            }
        };
        // Paths given on the command line (useful when running the unbundled
        // binary: `Editage notes.txt.age`) are opened like File → Open.
        // Only existing files are considered; options macOS or Xcode may
        // pass (such as `-NSDocumentRevisionsDebugMode YES`) are ignored.
        let from_arguments: Vec<PathBuf> = std::env::args_os()
            .skip(1)
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .collect();
        let reopening = !to_reopen.is_empty() || !from_arguments.is_empty();
        for path in to_reopen.into_iter().chain(from_arguments) {
            self.open_path(path);
        }
        if !reopening {
            self.show_welcome();
        }
        #[allow(deprecated)] // Needed when launched unbundled (cargo run).
        application.activateIgnoringOtherApps(true);
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

// ----- Application delegate -------------------------------------------------

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageAppDelegate"]
    pub struct AppDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for AppDelegate {}

    // SAFETY: the method signatures match NSApplicationDelegate.
    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            app().did_finish_launching();
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _sender: &NSApplication) -> NSApplicationTerminateReply {
            app().should_terminate()
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            app().will_terminate();
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn terminate_after_last_window(&self, _sender: &NSApplication) -> bool {
            false
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _sender: &NSApplication, has_visible_windows: bool) -> bool {
            if !has_visible_windows {
                app().show_welcome();
            }
            true
        }

        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _application: &NSApplication, urls: &NSArray<NSURL>) {
            for url in urls.iter() {
                if let Some(path) = url.path() {
                    app().open_path(PathBuf::from(path.to_string()));
                }
            }
        }

        // Editage saves no restoration state (windows are non-restorable
        // and restoration is disabled in preferences_store). Returning true
        // only tells AppKit to use secure coding if it ever asks.
        #[unsafe(method(applicationSupportsSecureRestorableState:))]
        fn supports_secure_restorable_state(&self, _application: &NSApplication) -> bool {
            true
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }
}

/// Installs the application controller and runs the event loop.
pub fn run(mtm: MainThreadMarker) {
    crate::preferences_store::disable_state_restoration();
    let controller = Rc::new(App::new(mtm));
    APP.with(|app| *app.borrow_mut() = Some(controller));

    let application = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm);
    application.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    application.run();
    drop(delegate);
}
