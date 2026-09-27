//! Editage → Settings… (⌘,): the design's General and Security tabs, plus
//! the default passphrase policy and a plain statement of what the
//! preferences store contains.

use std::cell::Cell;
use std::rc::Rc;

use editage_core::clipboard::ClipboardClearPolicy;
use editage_core::document::PassphrasePolicy;
use editage_core::preferences::{
    EditorFont, Preferences, FONT_SIZE_CHOICES, INACTIVITY_LOCK_CHOICES_MINUTES,
};
use objc2::rc::Retained;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSBackingStoreType, NSPopUpButton, NSStackView, NSView, NSWindow, NSWindowStyleMask,
    NSWindowToolbarStyle,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};

use crate::app::app;
use crate::controls::{
    checkbox, hstack, key_label, ns, secondary_label, vstack, ActionTarget, TargetBag,
    WindowObserver, WindowObserverCallbacks,
};
use crate::preferences_store::preferences_location;
use crate::toolbar::{ToolbarDelegate, ToolbarItemSpec};

const LABEL_WIDTH: f64 = 150.0;
const CONTROL_WIDTH: f64 = 300.0;

pub struct SettingsWindow {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    /// Keeps the toolbar item targets alive as long as the window.
    _toolbar_targets: TargetBag,
    /// Targets of the current tab's controls (replaced on rebuild).
    content_bag: TargetBag,
    toolbar_delegate: Retained<ToolbarDelegate>,
    observer: Retained<WindowObserver>,
    security_tab: Cell<bool>,
}

fn popup(
    titles: &[String],
    selected: usize,
    bag: &TargetBag,
    mtm: MainThreadMarker,
    on_select: impl Fn(usize) + 'static,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::new(mtm);
    for title in titles {
        popup.addItemWithTitle(&ns(title));
    }
    popup.selectItemAtIndex(selected as isize);
    let holder: Rc<std::cell::RefCell<Option<Retained<NSPopUpButton>>>> = Default::default();
    let read = holder.clone();
    let target = bag.keep(ActionTarget::new(mtm, move || {
        if let Some(popup) = read.borrow().as_ref() {
            let index = popup.indexOfSelectedItem();
            if index >= 0 {
                on_select(index as usize);
            }
        }
    }));
    // SAFETY: the target is kept alive by `bag`.
    unsafe {
        popup.setTarget(Some(target.as_object()));
        popup.setAction(Some(ActionTarget::selector()));
    }
    *holder.borrow_mut() = Some(popup.clone());
    popup
}

/// One "Label:  control" row with optional note underneath.
fn setting_row(
    label: &str,
    controls: &[&NSView],
    note: Option<&str>,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let key = key_label(label, mtm);
    key.widthAnchor()
        .constraintEqualToConstant(LABEL_WIDTH)
        .setActive(true);
    let control_column = vstack(controls, 6.0, mtm);
    if let Some(note) = note {
        control_column.addArrangedSubview(&secondary_label(note, CONTROL_WIDTH, mtm));
    }
    let row = hstack(&[&key, &control_column], 10.0, mtm);
    row.setAlignment(objc2_app_kit::NSLayoutAttribute::Top);
    row
}

impl SettingsWindow {
    pub fn new(mtm: MainThreadMarker) -> Rc<SettingsWindow> {
        // SAFETY: standard designated initializer with valid arguments.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(540.0, 380.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: owned by `SettingsWindow`; not released on close.
        unsafe { window.setReleasedWhenClosed(false) };
        window.setRestorable(false);
        window.setToolbarStyle(NSWindowToolbarStyle::Preference);

        let bag = TargetBag::default();
        let general = bag.keep(ActionTarget::new(mtm, || app().show_settings(false)));
        let security = bag.keep(ActionTarget::new(mtm, || app().show_settings(true)));
        let toolbar_delegate = ToolbarDelegate::new(
            mtm,
            vec![
                ToolbarItemSpec {
                    identifier: "general",
                    label: "General",
                    symbol: "gearshape",
                    tooltip: "General",
                    target: general,
                },
                ToolbarItemSpec {
                    identifier: "security",
                    label: "Security",
                    symbol: "lock",
                    tooltip: "Security",
                    target: security,
                },
            ],
            false,
            true,
        );
        let toolbar = toolbar_delegate.make_toolbar("EditageSettingsToolbar", mtm);
        window.setToolbar(Some(&toolbar));
        let observer = WindowObserver::new(mtm, WindowObserverCallbacks::default());
        observer.attach(&window);
        window.center();

        Rc::new(SettingsWindow {
            mtm,
            window,
            _toolbar_targets: bag,
            content_bag: TargetBag::default(),
            toolbar_delegate,
            observer,
            security_tab: Cell::new(false),
        })
    }

    pub fn show(self: &Rc<Self>, security_tab: bool) {
        self.security_tab.set(security_tab);
        self.rebuild();
        self.window.makeKeyAndOrderFront(None);
    }

    /// Rebuilds the visible tab after the current event. Called when
    /// preferences change, often from one of this window's own controls,
    /// which must not be replaced while its action is running.
    pub fn refresh(self: &Rc<Self>) {
        let this = self.clone();
        crate::app::defer_on_main_thread(Box::new(move || this.rebuild()));
    }

    fn rebuild(&self) {
        self.content_bag.clear();
        let _ = (&self.toolbar_delegate, &self.observer);
        let preferences = app().preferences();
        let security = self.security_tab.get();
        let identifier = if security { "security" } else { "general" };
        if let Some(toolbar) = self.window.toolbar() {
            toolbar.setSelectedItemIdentifier(Some(&ns(identifier)));
        }
        self.window
            .setTitle(&ns(if security { "Security" } else { "General" }));

        let content = if security {
            self.security_content(&preferences)
        } else {
            self.general_content(&preferences)
        };
        content.setEdgeInsets(objc2_foundation::NSEdgeInsets {
            top: 20.0,
            left: 24.0,
            bottom: 24.0,
            right: 24.0,
        });
        self.window.setContentView(Some(&content));
        let size = content.fittingSize();
        self.window
            .setContentSize(NSSize::new(size.width.max(540.0), size.height));
    }

    fn general_content(&self, preferences: &Preferences) -> Retained<NSStackView> {
        let mtm = self.mtm;
        let bag = &self.content_bag;
        let fonts: Vec<String> = EditorFont::CHOICES
            .iter()
            .map(|f| f.label().to_owned())
            .collect();
        let font_index = EditorFont::CHOICES
            .iter()
            .position(|f| *f == preferences.font)
            .unwrap_or(0);
        let font = popup(&fonts, font_index, bag, mtm, |index| {
            app().update_preferences(|p| p.font = EditorFont::CHOICES[index]);
        });
        let mut sizes: Vec<u32> = FONT_SIZE_CHOICES.to_vec();
        if !sizes.contains(&preferences.font_size) {
            sizes.push(preferences.font_size);
            sizes.sort_unstable();
        }
        let size_titles: Vec<String> = sizes.iter().map(|s| s.to_string()).collect();
        let size_index = sizes
            .iter()
            .position(|s| *s == preferences.font_size)
            .unwrap_or(0);
        let size = popup(&size_titles, size_index, bag, mtm, move |index| {
            let chosen = sizes[index];
            app().update_preferences(|p| p.font_size = chosen);
        });
        let font_row = hstack(&[&font, &size], 8.0, mtm);

        let wrap = checkbox(
            "Wrap lines to window width",
            preferences.wrap_lines,
            bag,
            mtm,
            |on| {
                app().update_preferences(|p| p.wrap_lines = on);
            },
        );
        let reopen = checkbox(
            "Reopen documents that were open",
            preferences.reopen_documents_at_launch,
            bag,
            mtm,
            |on| app().update_preferences(|p| p.set_reopen_documents_at_launch(on)),
        );
        let recent = checkbox(
            "Show files in Open Recent",
            preferences.remember_recent_documents,
            bag,
            mtm,
            |on| app().update_preferences(|p| p.set_remember_recent_documents(on)),
        );

        let stored = preferences.stored_metadata_description().join(" ");
        vstack(
            &[
                &setting_row("Default font:", &[&font_row], None, mtm),
                &setting_row("Line wrapping:", &[&wrap], None, mtm),
                &setting_row(
                    "At launch:",
                    &[&reopen],
                    Some("Reopened documents open locked. Only their file paths are stored."),
                    mtm,
                ),
                &setting_row(
                    "Recent documents:",
                    &[&recent],
                    Some("When off, file names are not kept. macOS, Finder or a sync app may still keep their own records of files you open."),
                    mtm,
                ),
                &setting_row(
                    "Stored settings:",
                    &[&secondary_label(&format!("{stored} Location: {}", preferences_location()), CONTROL_WIDTH, mtm)],
                    None,
                    mtm,
                ),
            ],
            16.0,
            mtm,
        )
    }

    fn security_content(&self, preferences: &Preferences) -> Retained<NSStackView> {
        let mtm = self.mtm;
        let bag = &self.content_bag;

        let clipboard_titles: Vec<String> = ClipboardClearPolicy::CHOICES
            .iter()
            .map(|p| p.label())
            .collect();
        let clipboard_index = ClipboardClearPolicy::CHOICES
            .iter()
            .position(|p| *p == preferences.clipboard_clear)
            .unwrap_or(0);
        let clipboard = popup(&clipboard_titles, clipboard_index, bag, mtm, |index| {
            app().update_preferences(|p| p.clipboard_clear = ClipboardClearPolicy::CHOICES[index]);
        });

        let lock_titles: Vec<String> = INACTIVITY_LOCK_CHOICES_MINUTES
            .iter()
            .map(|choice| match choice {
                None => "Off".to_owned(),
                Some(minutes) => format!("{minutes} minutes"),
            })
            .collect();
        let lock_index = INACTIVITY_LOCK_CHOICES_MINUTES
            .iter()
            .position(|c| *c == preferences.lock_after_inactivity_minutes)
            .unwrap_or(0);
        let lock = popup(&lock_titles, lock_index, bag, mtm, |index| {
            app().update_preferences(|p| {
                p.lock_after_inactivity_minutes = INACTIVITY_LOCK_CHOICES_MINUTES[index]
            });
        });

        let policies = [
            PassphrasePolicy::KeepUntilLocked,
            PassphrasePolicy::AskAgainWhenSaving,
        ];
        let policy_titles = vec![
            "Keep until the document is locked".to_owned(),
            "Ask again when saving".to_owned(),
        ];
        let policy_index = policies
            .iter()
            .position(|p| *p == preferences.default_passphrase_policy)
            .unwrap_or(0);
        let policy = popup(&policy_titles, policy_index, bag, mtm, move |index| {
            app().update_preferences(|p| p.default_passphrase_policy = policies[index]);
        });

        vstack(
            &[
                &setting_row(
                    "Clear clipboard:",
                    &[&clipboard],
                    Some("Only if text copied from this app is still on the clipboard. Text copied by other apps is never touched. Clipboard managers may keep their own copy."),
                    mtm,
                ),
                &setting_row(
                    "Lock after inactivity:",
                    &[&lock],
                    Some("Locking removes the decrypted text from the editor and releases the password. The file stays open. A document with unsaved changes is not locked; the Security Inspector shows when auto-lock is postponed."),
                    mtm,
                ),
                &setting_row(
                    "Password:",
                    &[&policy],
                    Some("The initial choice for “Remember password” when unlocking. When not kept, you are asked for the password each time the document is saved."),
                    mtm,
                ),
                &setting_row(
                    "Diagnostic log:",
                    &[&secondary_label(
                        "Off. Recent operational events are kept in memory only (Window → Diagnostics) and are never written to disk.",
                        CONTROL_WIDTH,
                        mtm,
                    )],
                    None,
                    mtm,
                ),
            ],
            16.0,
            mtm,
        )
    }
}
