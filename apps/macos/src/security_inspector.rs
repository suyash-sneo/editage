//! File → Document Security… (⌥⌘I).
//!
//! A live description of the main document's actual state. Every row comes
//! from `editage_core::inspect_document_state`, which reads the same session
//! that controls behaviour; this window only lays the report out. It is
//! rebuilt whenever the report changes (checked every second and after every
//! state change), so values such as a clipboard countdown stay current.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use editage_core::inspect_document_state;
use editage_core::security_state::{InspectorAction, ReportValue, SecurityReport};
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSPanel, NSScrollView, NSStackView, NSView, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::app::app;
use crate::controls::{
    abbreviate_home, button, format_time, key_label, ns, pin_to_edges, secondary_label,
    section_heading, small_button, small_label, title_label, value_label, vstack, FlippedView,
    TargetBag,
};

const WIDTH: f64 = 460.0;
const VALUE_WIDTH: f64 = 270.0;

pub struct SecurityInspector {
    mtm: MainThreadMarker,
    panel: Retained<NSPanel>,
    container: Retained<FlippedView>,
    bag: TargetBag,
    last_report: RefCell<Option<SecurityReport>>,
    mechanism_expanded: Cell<bool>,
    showing_empty: Cell<bool>,
}

fn value_text(value: &ReportValue) -> String {
    match value {
        ReportValue::Text(text) => text.clone(),
        ReportValue::Path(path) => abbreviate_home(path),
        ReportValue::TextWithTime { text, at } => format!("{text} {}", format_time(*at)),
    }
}

impl SecurityInspector {
    pub fn new(mtm: MainThreadMarker) -> Rc<SecurityInspector> {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, 620.0)),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered,
            true,
        );
        // SAFETY: owned by `SecurityInspector`; not released on close.
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&ns("Document Security"));
        panel.setRestorable(false);
        panel.setHidesOnDeactivate(false);
        panel.setFloatingPanel(false);
        panel.setMinSize(NSSize::new(380.0, 300.0));
        if !panel.setFrameUsingName(&ns("EditageSecurityInspector")) {
            panel.center();
        }
        panel.setFrameAutosaveName(&ns("EditageSecurityInspector"));

        let scroll = NSScrollView::new(mtm);
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);
        let container = FlippedView::new(mtm);
        container.setTranslatesAutoresizingMaskIntoConstraints(false);
        scroll.setDocumentView(Some(&container));
        container
            .widthAnchor()
            .constraintEqualToAnchor(&scroll.contentView().widthAnchor())
            .setActive(true);
        panel.setContentView(Some(&scroll));

        Rc::new(SecurityInspector {
            mtm,
            panel,
            container,
            bag: TargetBag::default(),
            last_report: RefCell::new(None),
            mechanism_expanded: Cell::new(false),
            showing_empty: Cell::new(false),
        })
    }

    pub fn show(&self) {
        self.panel.makeKeyAndOrderFront(None);
    }

    /// Rebuilds the view if the report changed (or `force`).
    pub fn refresh(self: &Rc<Self>, force: bool) {
        if !self.panel.isVisible() {
            return;
        }
        let application = app();
        let Some(document) = application.key_document() else {
            if force || !self.showing_empty.get() {
                self.showing_empty.set(true);
                *self.last_report.borrow_mut() = None;
                self.schedule_rebuild(None);
            }
            return;
        };
        let report = application
            .application_facts(|facts| inspect_document_state(&document.session(), facts));
        let unchanged = self.last_report.borrow().as_ref() == Some(&report);
        if unchanged && !force {
            return;
        }
        self.showing_empty.set(false);
        *self.last_report.borrow_mut() = Some(report.clone());
        self.schedule_rebuild(Some(report));
    }

    /// Rebuilds after the current event: a refresh is often triggered from
    /// one of this window's own buttons, which must not be removed while its
    /// action is running.
    fn schedule_rebuild(self: &Rc<Self>, report: Option<SecurityReport>) {
        let this = self.clone();
        crate::app::defer_on_main_thread(Box::new(move || this.rebuild(report.as_ref())));
    }

    fn rebuild(self: &Rc<Self>, report: Option<&SecurityReport>) {
        for view in self.container.subviews().iter() {
            view.removeFromSuperview();
        }
        self.bag.clear();
        let mtm = self.mtm;
        let content = vstack(&[], 14.0, mtm);
        content.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 16.0,
            left: 18.0,
            bottom: 18.0,
            right: 18.0,
        });

        match report {
            None => {
                content.addArrangedSubview(&small_label("No document is open.", WIDTH - 40.0, mtm));
                self.panel.setTitle(&ns("Document Security"));
            }
            Some(report) => {
                self.panel.setTitle(&ns(&format!(
                    "Document Security — {}",
                    report.document_name
                )));
                for section in &report.sections {
                    let rows = vstack(&[&section_heading(section.title, mtm)], 6.0, mtm);
                    for row in &section.rows {
                        rows.addArrangedSubview(&self.row(
                            row.label,
                            &value_text(&row.value),
                            row.needs_attention,
                        ));
                    }
                    content.addArrangedSubview(&rows);
                }
                content.addArrangedSubview(&self.mechanism(report));
                content.addArrangedSubview(&self.actions(report));
            }
        }

        self.container.addSubview(&content);
        pin_to_edges(&content, &self.container);
    }

    fn row(&self, label: &str, value: &str, needs_attention: bool) -> Retained<NSStackView> {
        let mtm = self.mtm;
        let key = key_label(label, mtm);
        key.widthAnchor()
            .constraintEqualToConstant(140.0)
            .setActive(true);
        let value = value_label(value, VALUE_WIDTH, mtm);
        if needs_attention {
            value.setTextColor(Some(&NSColor::systemOrangeColor()));
        }
        let row = crate::controls::hstack(&[&key, &value], 10.0, mtm);
        row.setAlignment(objc2_app_kit::NSLayoutAttribute::FirstBaseline);
        row
    }

    /// "How saving works ▸": generated from the save stages in the core, so
    /// it describes the implementation that actually runs.
    fn mechanism(self: &Rc<Self>, report: &SecurityReport) -> Retained<NSStackView> {
        let mtm = self.mtm;
        let expanded = self.mechanism_expanded.get();
        let weak = Rc::downgrade(self);
        let toggle = button(
            if expanded {
                "How saving works ▾"
            } else {
                "How saving works ▸"
            },
            &self.bag,
            mtm,
            move || {
                if let Some(this) = weak.upgrade() {
                    this.mechanism_expanded.set(!this.mechanism_expanded.get());
                    this.refresh(true);
                }
            },
        );
        toggle.setBordered(false);
        small_button(&toggle);
        let section = vstack(&[&toggle], 6.0, mtm);
        if expanded {
            for (index, step) in report.save_mechanism.iter().enumerate() {
                section.addArrangedSubview(&small_label(
                    &format!("{}. {step}", index + 1),
                    WIDTH - 50.0,
                    mtm,
                ));
            }
            section.addArrangedSubview(&secondary_label(
                report.save_mechanism_footer,
                WIDTH - 50.0,
                mtm,
            ));
            section.addArrangedSubview(&secondary_label(
                "The application does not overwrite the current encrypted file incrementally. It first creates a complete encrypted replacement and commits it only after writing succeeds.",
                WIDTH - 50.0,
                mtm,
            ));
        }
        section
    }

    fn actions(&self, report: &SecurityReport) -> Retained<NSView> {
        let mtm = self.mtm;
        let buttons = vstack(&[&title_label("Actions", WIDTH - 40.0, mtm)], 6.0, mtm);
        let mut row = crate::controls::hstack(&[], 8.0, mtm);
        let mut in_row = 0;
        for action in &report.actions {
            let (title, enabled, handler): (&str, bool, Box<dyn Fn()>) = match action.clone() {
                InspectorAction::RevealEncryptedFile { enabled } => (
                    "Reveal Encrypted File in Finder",
                    enabled,
                    Box::new(|| with_document(|d| d.reveal_file())),
                ),
                InspectorAction::CopyFilePath { enabled } => (
                    "Copy File Path",
                    enabled,
                    Box::new(|| with_document(|d| d.copy_path())),
                ),
                InspectorAction::LockDocument { enabled } => (
                    "Lock Document",
                    enabled,
                    Box::new(|| with_document(|d| d.lock())),
                ),
                InspectorAction::ForgetRetainedPassphrase { enabled } => (
                    "Forget Retained Passphrase",
                    enabled,
                    Box::new(|| with_document(|d| d.forget_passphrase())),
                ),
                InspectorAction::ClearClipboardNow { enabled } => (
                    "Clear Clipboard Now",
                    enabled,
                    Box::new(|| app().clear_clipboard_now()),
                ),
                InspectorAction::ViewDiagnostics => (
                    "View Diagnostics",
                    true,
                    Box::new(|| app().show_diagnostics()),
                ),
                InspectorAction::RevealStagingFile { .. } => (
                    "Reveal Staging File",
                    true,
                    Box::new(|| with_document(|d| d.reveal_leftover_staging_file())),
                ),
                InspectorAction::RetryCleanup => (
                    "Retry Cleanup",
                    true,
                    Box::new(|| with_document(|d| d.retry_cleanup())),
                ),
            };
            let control = button(title, &self.bag, mtm, handler);
            small_button(&control);
            control.setEnabled(enabled);
            if in_row == 2 {
                buttons.addArrangedSubview(&row);
                row = crate::controls::hstack(&[], 8.0, mtm);
                in_row = 0;
            }
            row.addArrangedSubview(&control);
            in_row += 1;
        }
        buttons.addArrangedSubview(&row);
        Retained::into_super(buttons)
    }
}

fn with_document(action: impl Fn(&Rc<crate::document_window::DocumentWindow>)) {
    if let Some(document) = app().key_document() {
        action(&document);
    }
}
