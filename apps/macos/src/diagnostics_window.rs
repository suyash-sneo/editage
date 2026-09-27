//! Window → Diagnostics: the bounded in-memory event history, copyable.
//! Nothing here is written to disk.

use std::cell::Cell;
use std::rc::Rc;

use editage_core::diagnostics::{DiagnosticLog, DIAGNOSTIC_HISTORY_LIMIT};
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSFont, NSFontWeightRegular, NSStandardKeyBindingResponding, NSTextView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::app::app;
use crate::controls::{
    button, flexible_space, format_time, hstack, ns, pin_to_edges, secondary_label, vstack,
    TargetBag,
};

pub struct DiagnosticsWindow {
    window: Retained<NSWindow>,
    text_view: Retained<NSTextView>,
    log: DiagnosticLog,
    shown_count: Cell<usize>,
    shown_last: std::cell::RefCell<Option<std::time::SystemTime>>,
    bag: TargetBag,
}

impl DiagnosticsWindow {
    pub fn new(mtm: MainThreadMarker, log: DiagnosticLog) -> Rc<DiagnosticsWindow> {
        // SAFETY: standard designated initializer with valid arguments.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(640.0, 420.0)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `DiagnosticsWindow`; not released on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setRestorable(false);
        window.setTitle(&ns("Diagnostics"));
        if !window.setFrameUsingName(&ns("EditageDiagnostics")) {
            window.center();
        }
        window.setFrameAutosaveName(&ns("EditageDiagnostics"));

        let scroll = NSTextView::scrollableTextView(mtm);
        let text_view = scroll
            .documentView()
            .and_then(|view| view.downcast::<NSTextView>().ok())
            .expect("scrollableTextView contains a text view");
        text_view.setEditable(false);
        text_view.setRichText(false);
        // SAFETY: valid font weight constant.
        text_view.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(
            11.0,
            unsafe { NSFontWeightRegular },
        )));
        text_view.setTextContainerInset(NSSize::new(8.0, 8.0));

        let bag = TargetBag::default();
        let note = secondary_label(
            &format!(
                "Persistent diagnostic log: Off. The last {DIAGNOSTIC_HISTORY_LIMIT} operational events are kept in memory only. Document text, passwords, clipboard contents and search text are never recorded."
            ),
            420.0,
            mtm,
        );
        let copy_log = log.clone();
        let copy = button("Copy", &bag, mtm, move || {
            crate::clipboard::copy_non_document_text(
                &copy_log.export_text(&|time| format_time(time)),
            );
        });
        let clear_log = log.clone();
        let clear = button("Clear", &bag, mtm, move || {
            clear_log.clear();
            app().show_diagnostics();
        });
        let space = flexible_space(mtm);
        let footer = hstack(&[&note, &space, &clear, &copy], 8.0, mtm);
        footer.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 8.0,
            left: 12.0,
            bottom: 10.0,
            right: 12.0,
        });

        let content = vstack(&[&scroll, &footer], 0.0, mtm);
        content.setAlignment(objc2_app_kit::NSLayoutAttribute::Width);
        let root = objc2_app_kit::NSView::new(mtm);
        root.addSubview(&content);
        pin_to_edges(&content, &root);
        window.setContentView(Some(&root));

        Rc::new(DiagnosticsWindow {
            window,
            text_view,
            log,
            shown_count: Cell::new(usize::MAX),
            shown_last: std::cell::RefCell::new(None),
            bag,
        })
    }

    pub fn show(&self) {
        self.shown_count.set(usize::MAX);
        self.refresh_if_changed();
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn refresh_if_changed(&self) {
        let _ = &self.bag;
        if !self.window.isVisible() && self.shown_count.get() != usize::MAX {
            return;
        }
        let entries = self.log.entries();
        let last = entries.last().map(|entry| entry.at);
        if entries.len() == self.shown_count.get() && *self.shown_last.borrow() == last {
            return;
        }
        self.shown_count.set(entries.len());
        *self.shown_last.borrow_mut() = last;
        let text = self.log.export_text(&|time| format_time(time));
        let text = if text.is_empty() {
            "No events yet.".to_owned()
        } else {
            text
        };
        self.text_view.setString(&ns(&text));
        // SAFETY: standard responder action; `None` sender is allowed.
        unsafe { self.text_view.scrollToEndOfDocument(None) };
    }
}
