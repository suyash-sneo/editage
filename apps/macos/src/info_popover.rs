//! The design's Document Info popover (ⓘ, ⌘I): quick facts about the file,
//! plus a way into the full Security Inspector.

use std::rc::Rc;

use editage_core::document::FileBinding;
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{NSPopover, NSPopoverBehavior, NSStackView, NSViewController};
use objc2_foundation::{NSPoint, NSRect, NSRectEdge, NSSize};

use crate::app::app;
use crate::controls::{
    abbreviate_home, button, format_bytes, format_date_time, hstack, small_button, title_label,
    vstack, TargetBag,
};
use crate::document_window::DocumentWindow;
use crate::sheets::details_grid;

pub struct InfoPopover {
    popover: Retained<NSPopover>,
    content: Retained<NSStackView>,
    bag: TargetBag,
}

impl InfoPopover {
    pub fn show(document: &Rc<DocumentWindow>) -> InfoPopover {
        let mtm = document.window().mtm();
        let content = vstack(&[], 10.0, mtm);
        content.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 14.0,
            left: 16.0,
            bottom: 14.0,
            right: 16.0,
        });
        let this = InfoPopover {
            popover: NSPopover::new(mtm),
            content,
            bag: TargetBag::default(),
        };
        this.fill(document);

        let controller = NSViewController::new(mtm);
        controller.setView(&this.content);
        this.popover.setContentViewController(Some(&controller));
        this.popover.setBehavior(NSPopoverBehavior::Transient);
        this.popover.setContentSize(this.content.fittingSize());

        // Anchor near the toolbar's Info button at the top-right corner.
        if let Some(view) = document.window().contentView() {
            let bounds = view.bounds();
            let anchor = NSRect::new(
                NSPoint::new(bounds.size.width - 40.0, bounds.size.height - 2.0),
                NSSize::new(20.0, 2.0),
            );
            this.popover
                .showRelativeToRect_ofView_preferredEdge(anchor, &view, NSRectEdge::MaxY);
        }
        this
    }

    pub fn is_shown(&self) -> bool {
        self.popover.isShown()
    }

    pub fn close(&self) {
        self.popover.close();
    }

    pub fn refresh(&self, document: &DocumentWindow) {
        if !self.is_shown() {
            return;
        }
        for view in self.content.arrangedSubviews().iter() {
            self.content.removeArrangedSubview(&view);
            view.removeFromSuperview();
        }
        self.bag.clear();
        self.fill(document);
        self.popover.setContentSize(self.content.fittingSize());
    }

    fn fill(&self, document: &DocumentWindow) {
        let mtm = document.window().mtm();
        let session = document.session();
        self.content
            .addArrangedSubview(&title_label(session.display_name(), 300.0, mtm));

        // Counts need the text; a short-lived zeroizing copy is used.
        let (characters, words, lines) = {
            let text = document.editor().plaintext_for_saving();
            let text = text.as_str();
            (
                text.chars().count(),
                text.split_whitespace().count(),
                text.split('\n').count(),
            )
        };
        let (location, modified, size) = match session.binding() {
            FileBinding::NotYetSaved => ("Not saved".to_owned(), "—".to_owned(), "—".to_owned()),
            FileBinding::OnDisk(file) => (
                file.path.parent().map(abbreviate_home).unwrap_or_default(),
                file.identity
                    .as_ref()
                    .map(|identity| format_date_time(identity.modified))
                    .unwrap_or_else(|| "—".to_owned()),
                format_bytes(file.ciphertext_bytes),
            ),
        };
        let rows = vec![
            ("Where".to_owned(), location),
            ("Modified".to_owned(), modified),
            ("Encrypted size".to_owned(), size),
            ("Characters".to_owned(), group(characters)),
            ("Words".to_owned(), group(words)),
            ("Lines".to_owned(), group(lines)),
            (
                "Format".to_owned(),
                format!(
                    "{}, {}",
                    session.format().display_name(),
                    session.format().protection_description().to_lowercase()
                ),
            ),
        ];
        self.content
            .addArrangedSubview(&details_grid(&rows, 200.0, mtm));

        let on_disk = session.path().is_some();
        drop(session);
        let id = document.id();
        let reveal = button("Reveal in Finder", &self.bag, mtm, move || {
            if let Some(document) = app().document(id) {
                document.reveal_file();
            }
        });
        let copy = button("Copy Path", &self.bag, mtm, move || {
            if let Some(document) = app().document(id) {
                document.copy_path();
            }
        });
        reveal.setEnabled(on_disk);
        copy.setEnabled(on_disk);
        let security = button("Document Security…", &self.bag, mtm, move || {
            if let Some(document) = app().document(id) {
                document.toggle_info();
            }
            app().show_security_inspector();
        });
        for control in [&reveal, &copy, &security] {
            small_button(control);
        }
        self.content
            .addArrangedSubview(&hstack(&[&reveal, &copy], 8.0, mtm));
        self.content.addArrangedSubview(&security);
    }
}

fn group(value: usize) -> String {
    editage_core::diagnostics::group_digits(value as u64)
}
