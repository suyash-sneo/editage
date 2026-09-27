//! The design's welcome window: Open…, New Document, and (if enabled) the
//! recent documents list. Shown at launch and whenever no document is open.

use std::rc::Rc;

use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSClickGestureRecognizer, NSFont, NSFontWeightSemibold, NSStackView,
    NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::app::app;
use crate::controls::{
    abbreviate_home, button, checkbox, hstack, label, make_default, ns, secondary_label,
    section_heading, small_label, vstack, ActionTarget, TargetBag, WindowObserver,
    WindowObserverCallbacks,
};

const WIDTH: f64 = 440.0;

pub struct WelcomeWindow {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    bag: TargetBag,
    observer: Retained<WindowObserver>,
}

impl WelcomeWindow {
    pub fn new(mtm: MainThreadMarker) -> Rc<WelcomeWindow> {
        // SAFETY: standard designated initializer with valid arguments.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, 360.0)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `WelcomeWindow`; not released on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setRestorable(false);
        window.setTitlebarAppearsTransparent(true);
        window.setTitle(&ns("Editage"));
        window.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
        let observer = WindowObserver::new(
            mtm,
            WindowObserverCallbacks {
                will_close: Some(Box::new(|| {
                    crate::app::defer_on_main_thread(Box::new(|| app().welcome_closed()))
                })),
                ..Default::default()
            },
        );
        observer.attach(&window);
        let this = Rc::new(WelcomeWindow {
            mtm,
            window,
            bag: TargetBag::default(),
            observer,
        });
        this.rebuild();
        this.window.center();
        this
    }

    pub fn show(&self) {
        self.window.makeKeyAndOrderFront(None);
    }

    pub fn close(&self) {
        self.window.close();
    }

    /// Rebuilds after the current event (the trigger may be this window's
    /// own checkbox).
    pub fn refresh(self: &Rc<Self>) {
        let this = self.clone();
        crate::app::defer_on_main_thread(Box::new(move || this.rebuild()));
    }

    fn rebuild(&self) {
        let _ = &self.observer;
        self.bag.clear();
        let mtm = self.mtm;
        let preferences = app().preferences();

        let title = label("Editage", mtm);
        // SAFETY: NSFontWeightSemibold is a valid font weight constant.
        title.setFont(Some(&NSFont::systemFontOfSize_weight(22.0, unsafe {
            NSFontWeightSemibold
        })));
        let subtitle = small_label(
            "Open an encrypted file or create a new one.",
            WIDTH - 60.0,
            mtm,
        );

        let open = button("Open…", &self.bag, mtm, || app().show_open_panel());
        make_default(&open);
        let new = button("New Document", &self.bag, mtm, || app().new_document());
        let buttons = hstack(&[&open, &new], 10.0, mtm);

        let content = vstack(&[&title, &subtitle, &buttons], 12.0, mtm);
        content.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 44.0,
            left: 30.0,
            bottom: 26.0,
            right: 30.0,
        });

        if preferences.remember_recent_documents {
            content.addArrangedSubview(&section_heading("Recent Documents", mtm));
            let list = vstack(&[], 8.0, mtm);
            if preferences.recent_documents.is_empty() {
                list.addArrangedSubview(&secondary_label("No recent documents", WIDTH - 60.0, mtm));
            }
            for path in preferences.recent_documents.iter().take(6).cloned() {
                list.addArrangedSubview(&self.recent_row(path));
            }
            content.addArrangedSubview(&list);
        }

        let remember = checkbox(
            "Remember recent documents",
            preferences.remember_recent_documents,
            &self.bag,
            mtm,
            |on| app().update_preferences(|p| p.set_remember_recent_documents(on)),
        );
        content.addArrangedSubview(&remember);
        if !preferences.remember_recent_documents {
            content.addArrangedSubview(&secondary_label(
                "File names are not kept.",
                WIDTH - 60.0,
                mtm,
            ));
        }

        self.window.setContentView(Some(&content));
        let size = content.fittingSize();
        self.window.setContentSize(NSSize::new(WIDTH, size.height));
    }

    fn recent_row(&self, path: std::path::PathBuf) -> Retained<NSStackView> {
        let mtm = self.mtm;
        let name = label(
            &path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            mtm,
        );
        name.setSelectable(false);
        let location = secondary_label(
            &path.parent().map(abbreviate_home).unwrap_or_default(),
            WIDTH - 60.0,
            mtm,
        );
        location.setSelectable(false);
        let row = vstack(&[&name, &location], 1.0, mtm);
        row.setToolTip(Some(&ns("Open")));
        let target = self.bag.keep(ActionTarget::new(mtm, move || {
            app().open_path(path.clone())
        }));
        // SAFETY: the target is kept alive by `bag`; `perform:` exists on it.
        let recognizer = unsafe {
            NSClickGestureRecognizer::initWithTarget_action(
                NSClickGestureRecognizer::alloc(mtm),
                Some(target.as_object()),
                Some(ActionTarget::selector()),
            )
        };
        row.addGestureRecognizer(&recognizer);
        let _ = hstack(&[], 0.0, mtm);
        row
    }
}
