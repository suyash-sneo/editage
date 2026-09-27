//! Native sheets and alerts.
//!
//! Short confirmations use `NSAlert`, with AppKit's own button order and
//! keyboard behaviour. Sheets that need fields or an expandable details
//! section (unlock, create password, save failure) are small custom panels
//! attached to the document window with `beginSheet`. They follow the design
//! prototype's layout: bold title, explanation, content, buttons at the
//! bottom right with an optional alternative button at the bottom left.

use std::cell::Cell;
use std::rc::Rc;

use block2::RcBlock;
use editage_core::presentation::{FailureAction, FailureReport};
use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSAlertThirdButtonReturn, NSBackingStoreType,
    NSButton, NSGridView, NSModalResponse, NSPanel, NSPasteboard, NSPasteboardTypeString,
    NSStackView, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSEdgeInsets, NSPoint, NSRect, NSSize};

use crate::controls::{
    button, flexible_space, hstack, key_label, make_cancel, make_default, ns, small_button,
    small_label, title_label, value_label, vstack, wrapping_label, TargetBag,
};

pub const SHEET_WIDTH: f64 = 420.0;
const TEXT_WIDTH: f64 = SHEET_WIDTH - 40.0;

/// A custom sheet attached to a window. Dropping it does not close it; call
/// [`Sheet::close`].
pub struct Sheet {
    pub panel: Retained<NSPanel>,
    parent: Retained<NSWindow>,
    pub content: Retained<NSStackView>,
    pub bag: TargetBag,
}

impl Sheet {
    /// Creates an empty sheet (not yet shown).
    pub fn new(parent: &NSWindow, mtm: MainThreadMarker) -> Sheet {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(SHEET_WIDTH, 200.0)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::DocModalWindow,
            NSBackingStoreType::Buffered,
            true,
        );
        // SAFETY: the sheet is owned by `Sheet` and closed explicitly; it
        // must not be released by AppKit when closed.
        unsafe { panel.setReleasedWhenClosed(false) };
        let content = vstack(&[], 12.0, mtm);
        content.setEdgeInsets(NSEdgeInsets {
            top: 20.0,
            left: 20.0,
            bottom: 20.0,
            right: 20.0,
        });
        panel.setContentView(Some(&content));
        Sheet {
            panel,
            parent: parent.retain(),
            content,
            bag: TargetBag::default(),
        }
    }

    pub fn add(&self, view: &NSView) {
        self.content.addArrangedSubview(view);
    }

    /// Attaches the sheet to its window.
    pub fn show(&self) {
        self.fit();
        self.parent.beginSheet_completionHandler(&self.panel, None);
    }

    /// Resizes to fit the current content (e.g. after showing details).
    pub fn fit(&self) {
        let size = self.content.fittingSize();
        self.panel
            .setContentSize(NSSize::new(SHEET_WIDTH.max(size.width), size.height));
    }

    pub fn close(&self) {
        self.parent.endSheet(&self.panel);
        self.panel.orderOut(None);
        self.bag.clear();
        // The close usually happens inside one of the sheet's own button
        // actions. Keep the panel (and so its buttons) alive until that
        // action has returned.
        let panel = self.panel.clone();
        crate::app::defer_on_main_thread(Box::new(move || drop(panel)));
    }

    pub fn make_first_responder(&self, view: &NSView) {
        self.panel.makeFirstResponder(Some(view));
    }
}

/// A button row: `leading` buttons at the left, `trailing` at the right.
pub fn button_row(
    leading: &[&NSView],
    trailing: &[&NSView],
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let mut views: Vec<&NSView> = leading.to_vec();
    let space = flexible_space(mtm);
    views.push(&space);
    views.extend(trailing.iter().copied());
    let row = hstack(&views, 8.0, mtm);
    row.setFrameSize(NSSize::new(TEXT_WIDTH, 24.0));
    row.widthAnchor()
        .constraintEqualToConstant(TEXT_WIDTH)
        .setActive(true);
    row
}

/// A two-column grid of label/value rows.
pub fn details_grid(
    rows: &[(String, String)],
    value_width: f64,
    mtm: MainThreadMarker,
) -> Retained<NSGridView> {
    let rows: Vec<Retained<NSArray<NSView>>> = rows
        .iter()
        .map(|(label, value)| {
            let key = key_label(label, mtm);
            let value = value_label(value, value_width, mtm);
            NSArray::from_retained_slice(&[
                Retained::into_super(Retained::into_super(key)),
                Retained::into_super(Retained::into_super(value)),
            ])
        })
        .collect();
    let array = NSArray::from_retained_slice(&rows);
    let grid = NSGridView::gridViewWithViews(&array, mtm);
    grid.setRowSpacing(6.0);
    grid.setColumnSpacing(10.0);
    grid
}

/// A "Show Details ▸" disclosure that reveals `details` and resizes the sheet.
pub fn details_disclosure(
    sheet: &Rc<Sheet>,
    details: &NSView,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    details.setHidden(true);
    let expanded = Rc::new(Cell::new(false));
    let disclosure_holder: Rc<std::cell::RefCell<Option<Retained<NSButton>>>> = Default::default();
    let weak_sheet = Rc::downgrade(sheet);
    let details_view = details.retain();
    let holder = disclosure_holder.clone();
    let toggle = button("Show Details", &sheet.bag, mtm, move || {
        let now_expanded = !expanded.get();
        expanded.set(now_expanded);
        details_view.setHidden(!now_expanded);
        if let Some(button) = holder.borrow().as_ref() {
            button.setTitle(&ns(if now_expanded {
                "Hide Details"
            } else {
                "Show Details"
            }));
        }
        if let Some(sheet) = weak_sheet.upgrade() {
            sheet.fit();
        }
    });
    toggle.setBordered(false);
    toggle.setContentTintColor(Some(&objc2_app_kit::NSColor::linkColor()));
    small_button(&toggle);
    *disclosure_holder.borrow_mut() = Some(toggle.clone());
    hstack(&[&toggle], 0.0, mtm)
}

fn action_title(action: &FailureAction) -> &'static str {
    match action {
        FailureAction::TryAgain => "Try Again",
        FailureAction::SaveAs => "Save As…",
        FailureAction::ReloadFromDisk => "Reload From Disk",
        FailureAction::RevealStagingFile(_) => "Reveal Staging File",
        FailureAction::RetryCleanup => "Try Cleanup Again",
        FailureAction::Cancel => "Cancel",
        FailureAction::Dismiss => "Dismiss",
    }
}

/// Presents a `FailureReport` as a sheet: short message first, details on
/// request, one button per action. `on_action` receives the chosen action
/// after the sheet has closed.
pub fn present_report_sheet(
    parent: &NSWindow,
    report: &FailureReport,
    mtm: MainThreadMarker,
    on_action: impl Fn(FailureAction) + 'static,
) -> Rc<Sheet> {
    let sheet = Rc::new(Sheet::new(parent, mtm));
    sheet.add(&title_label(&report.title, TEXT_WIDTH, mtm));
    for line in &report.message {
        sheet.add(&small_label(line, TEXT_WIDTH, mtm));
    }

    if !report.details.is_empty() {
        let grid = details_grid(&report.details, TEXT_WIDTH - 150.0, mtm);
        let plain_text = report.as_plain_text();
        let copy = button("Copy Details", &sheet.bag, mtm, move || {
            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            // SAFETY: NSPasteboardTypeString is a valid pasteboard type.
            pasteboard.setString_forType(&ns(&plain_text), unsafe { NSPasteboardTypeString });
        });
        small_button(&copy);
        let details = vstack(&[&grid, &copy], 8.0, mtm);
        let disclosure = details_disclosure(&sheet, &details, mtm);
        sheet.add(&disclosure);
        sheet.add(&details);
    }

    let on_action = Rc::new(on_action);
    let make = |action: FailureAction| {
        let weak_sheet = Rc::downgrade(&sheet);
        let on_action = on_action.clone();
        let title = action_title(&action);
        button(title, &sheet.bag, mtm, move || {
            if let Some(sheet) = weak_sheet.upgrade() {
                sheet.close();
            }
            on_action(action.clone());
        })
    };

    // Layout: "Save As…" and "Reveal…" at the leading edge (like the
    // design's save-error sheet); Cancel/Dismiss and the primary action at
    // the trailing edge, primary last.
    let mut leading = Vec::new();
    let mut trailing = Vec::new();
    let primary_exists = report.actions.iter().any(|action| {
        matches!(
            action,
            FailureAction::TryAgain | FailureAction::RetryCleanup
        )
    });
    for action in &report.actions {
        let button = make(action.clone());
        match action {
            FailureAction::SaveAs | FailureAction::RevealStagingFile(_) => leading.push(button),
            FailureAction::Cancel | FailureAction::Dismiss => {
                make_cancel(&button);
                if !primary_exists {
                    make_default(&button);
                }
                trailing.insert(0, button);
            }
            FailureAction::TryAgain | FailureAction::RetryCleanup => {
                make_default(&button);
                trailing.push(button);
            }
            FailureAction::ReloadFromDisk => trailing.insert(0, button),
        }
    }
    let leading_refs: Vec<&NSView> = leading.iter().map(|b| -> &NSView { b }).collect();
    let trailing_refs: Vec<&NSView> = trailing.iter().map(|b| -> &NSView { b }).collect();
    sheet.add(&button_row(&leading_refs, &trailing_refs, mtm));
    sheet.show();
    sheet
}

/// The choice made in a three-button save/discard alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveChoice {
    Save,
    Cancel,
    DontSave,
}

/// "Do you want to save the changes…?" with Save / Cancel / Don't Save in
/// the standard macOS order.
pub fn ask_save_changes(
    window: &NSWindow,
    title: &str,
    mtm: MainThreadMarker,
    on_choice: impl Fn(SaveChoice) + 'static,
) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(title));
    alert.setInformativeText(&ns("Your changes will be lost if you don’t save them."));
    alert.addButtonWithTitle(&ns("Save"));
    alert.addButtonWithTitle(&ns("Cancel"));
    let dont_save = alert.addButtonWithTitle(&ns("Don’t Save"));
    dont_save.setKeyEquivalent(&ns("d"));
    dont_save.setKeyEquivalentModifierMask(objc2_app_kit::NSEventModifierFlags::Command);
    let handler = RcBlock::new(move |response: NSModalResponse| {
        let choice = if response == NSAlertFirstButtonReturn {
            SaveChoice::Save
        } else if response == NSAlertThirdButtonReturn {
            SaveChoice::DontSave
        } else {
            SaveChoice::Cancel
        };
        on_choice(choice);
    });
    alert.beginSheetModalForWindow_completionHandler(window, Some(&handler));
}

/// A two-button confirmation alert. `on_confirm` runs only if confirmed.
pub fn confirm(
    window: &NSWindow,
    title: &str,
    message: &str,
    confirm_title: &str,
    destructive: bool,
    mtm: MainThreadMarker,
    on_confirm: impl Fn() + 'static,
) {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(title));
    alert.setInformativeText(&ns(message));
    let confirm_button = alert.addButtonWithTitle(&ns(confirm_title));
    alert.addButtonWithTitle(&ns("Cancel"));
    if destructive {
        confirm_button.setHasDestructiveAction(true);
    }
    let handler = RcBlock::new(move |response: NSModalResponse| {
        if response == NSAlertFirstButtonReturn {
            on_confirm();
        }
    });
    alert.beginSheetModalForWindow_completionHandler(window, Some(&handler));
}

/// A notice with Continue/Cancel. `on_result(true)` if the user continues.
pub fn confirm_notice(
    window: &NSWindow,
    title: &str,
    message: &str,
    confirm_title: &str,
    offers_cancel: bool,
    mtm: MainThreadMarker,
    on_result: impl Fn(bool) + 'static,
) {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&ns(title));
    alert.setInformativeText(&ns(message));
    alert.addButtonWithTitle(&ns(confirm_title));
    if offers_cancel {
        alert.addButtonWithTitle(&ns("Cancel"));
    }
    let handler = RcBlock::new(move |response: NSModalResponse| {
        on_result(response == NSAlertFirstButtonReturn);
    });
    alert.beginSheetModalForWindow_completionHandler(window, Some(&handler));
}

/// An application-modal report for failures with no document window (for
/// example a file that could not be opened).
pub fn run_modal_report(report: &FailureReport, mtm: MainThreadMarker) {
    let alert = NSAlert::new(mtm);
    alert.setAlertStyle(NSAlertStyle::Warning);
    alert.setMessageText(&ns(&report.title));
    alert.setInformativeText(&ns(&report.message.join("\n\n")));
    alert.addButtonWithTitle(&ns("OK"));
    if !report.details.is_empty() {
        let grid = details_grid(&report.details, 260.0, mtm);
        let holder = vstack(&[&wrapping_label("Details", 300.0, mtm), &grid], 6.0, mtm);
        holder.setFrameSize(holder.fittingSize());
        alert.setAccessoryView(Some(&holder));
    }
    alert.runModal();
}
