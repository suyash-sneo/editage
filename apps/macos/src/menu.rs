//! The main menu: the design's menus plus Document Security, Forget
//! Retained Passphrase, Change Encryption Password, Clear Clipboard Now and
//! Diagnostics.
//!
//! Standard editing commands (Undo, Cut, Copy, Find, …) are sent to the
//! first responder with their standard selectors, so the native text view
//! handles and validates them. Editage's own commands use closure targets
//! whose validation reads the document session.

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::Sel;
use objc2::{sel, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEventModifierFlags, NSMenu,
    NSMenuItem,
};
use objc2_foundation::MainThreadMarker;

use crate::app::{app, App};
use crate::controls::{abbreviate_home, ns, ActionTarget, TargetBag};
use crate::document_window::DocumentWindow;

thread_local! {
    static OPEN_RECENT: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
    /// Targets for the Open Recent items, replaced on every rebuild.
    static OPEN_RECENT_TARGETS: TargetBag = TargetBag::default();
}

struct MenuBuilder<'a> {
    bag: &'a TargetBag,
    mtm: MainThreadMarker,
}

fn modifiers(spec: &str) -> NSEventModifierFlags {
    let mut flags = NSEventModifierFlags::empty();
    for symbol in spec.chars() {
        flags |= match symbol {
            '⌘' => NSEventModifierFlags::Command,
            '⇧' => NSEventModifierFlags::Shift,
            '⌥' => NSEventModifierFlags::Option,
            '⌃' => NSEventModifierFlags::Control,
            _ => NSEventModifierFlags::empty(),
        };
    }
    flags
}

impl MenuBuilder<'_> {
    fn menu(&self, title: &str) -> Retained<NSMenu> {
        NSMenu::initWithTitle(NSMenu::alloc(self.mtm), &ns(title))
    }

    fn add_submenu(&self, parent: &NSMenu, title: &str, submenu: &NSMenu) {
        let item = NSMenuItem::new(self.mtm);
        item.setTitle(&ns(title));
        item.setSubmenu(Some(submenu));
        parent.addItem(&item);
    }

    /// An item sent to the first responder (native behaviour).
    fn standard(
        &self,
        menu: &NSMenu,
        title: &str,
        action: Sel,
        key: &str,
        mods: &str,
    ) -> Retained<NSMenuItem> {
        // SAFETY: `action` is a standard AppKit action selector.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &ns(title),
                Some(action),
                &ns(key),
            )
        };
        if !key.is_empty() {
            item.setKeyEquivalentModifierMask(modifiers(mods));
        }
        menu.addItem(&item);
        item
    }

    /// An item handled by Editage, enabled when `enabled()` is true.
    fn command(
        &self,
        menu: &NSMenu,
        title: &str,
        key: &str,
        mods: &str,
        action: impl Fn() + 'static,
        validate: impl Fn(&NSMenuItem) -> bool + 'static,
    ) -> Retained<NSMenuItem> {
        let target = self
            .bag
            .keep(ActionTarget::validated(self.mtm, action, validate));
        // SAFETY: the target is kept alive by the app's menu bag.
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm),
                &ns(title),
                Some(ActionTarget::selector()),
                &ns(key),
            )
        };
        // SAFETY: as above.
        unsafe { item.setTarget(Some(target.as_object())) };
        if !key.is_empty() {
            item.setKeyEquivalentModifierMask(modifiers(mods));
        }
        menu.addItem(&item);
        item
    }

    fn separator(&self, menu: &NSMenu) {
        menu.addItem(&NSMenuItem::separatorItem(self.mtm));
    }
}

/// Runs `action` on the main document, if any.
fn on_document(action: impl Fn(&Rc<DocumentWindow>) + 'static) -> impl Fn() + 'static {
    move || {
        if let Some(document) = app().key_document() {
            action(&document);
        }
    }
}

/// Validation based on the main document.
fn when_document(
    condition: impl Fn(&DocumentWindow) -> bool + 'static,
) -> impl Fn(&NSMenuItem) -> bool + 'static {
    move |_item| {
        app()
            .key_document()
            .is_some_and(|document| condition(&document))
    }
}

fn checked(item: &NSMenuItem, on: bool) {
    item.setState(if on {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });
}

pub fn install_main_menu(app_ref: &Rc<App>) {
    let mtm = app_ref.mtm;
    let b = MenuBuilder {
        bag: &app_ref.menu_bag,
        mtm,
    };
    let bar = b.menu("");

    // Editage
    let app_menu = b.menu("Editage");
    b.command(
        &app_menu,
        "About Editage",
        "",
        "",
        || app().show_about(),
        |_| true,
    );
    b.separator(&app_menu);
    b.command(
        &app_menu,
        "Settings…",
        ",",
        "⌘",
        || app().show_settings(false),
        |_| true,
    );
    b.separator(&app_menu);
    b.standard(&app_menu, "Hide Editage", sel!(hide:), "h", "⌘");
    b.standard(
        &app_menu,
        "Hide Others",
        sel!(hideOtherApplications:),
        "h",
        "⌥⌘",
    );
    b.standard(&app_menu, "Show All", sel!(unhideAllApplications:), "", "");
    b.separator(&app_menu);
    b.standard(&app_menu, "Quit Editage", sel!(terminate:), "q", "⌘");
    b.add_submenu(&bar, "Editage", &app_menu);

    // File
    let file = b.menu("File");
    b.command(&file, "New", "n", "⌘", || app().new_document(), |_| true);
    b.command(
        &file,
        "Open…",
        "o",
        "⌘",
        || app().show_open_panel(),
        |_| true,
    );
    let recent = b.menu("Open Recent");
    b.add_submenu(&file, "Open Recent", &recent);
    OPEN_RECENT.with(|cell| *cell.borrow_mut() = Some(recent));
    b.separator(&file);
    b.standard(&file, "Close", sel!(performClose:), "w", "⌘");
    b.command(
        &file,
        "Save",
        "s",
        "⌘",
        on_document(|d| d.save()),
        when_document(|d| {
            let session = d.session();
            session.is_unlocked() && !session.is_saving()
        }),
    );
    b.command(
        &file,
        "Save As…",
        "s",
        "⇧⌘",
        on_document(|d| d.save_as()),
        when_document(|d| {
            let session = d.session();
            session.is_unlocked() && !session.is_saving()
        }),
    );
    b.command(
        &file,
        "Revert to Saved",
        "",
        "",
        on_document(|d| d.reload_from_disk(true)),
        when_document(|d| {
            let session = d.session();
            session.is_unlocked()
                && !session.is_saving()
                && !session.is_never_saved()
                && session.has_unsaved_changes()
        }),
    );
    b.separator(&file);
    b.command(
        &file,
        "Lock Document",
        "l",
        "⌃⌘",
        on_document(|d| d.lock()),
        when_document(|d| {
            let session = d.session();
            session.is_unlocked() && !session.is_never_saved()
        }),
    );
    b.command(
        &file,
        "Forget Retained Passphrase",
        "",
        "",
        on_document(|d| d.forget_passphrase()),
        when_document(|d| d.session().has_retained_passphrase()),
    );
    b.command(
        &file,
        "Change Encryption Password…",
        "",
        "",
        on_document(|d| d.change_password()),
        when_document(|d| {
            let session = d.session();
            session.is_unlocked()
                && !session.is_never_saved()
                && !session.is_saving()
                && !session.is_read_only()
        }),
    );
    b.command(
        &file,
        "Document Security…",
        "i",
        "⌥⌘",
        || app().show_security_inspector(),
        |_| true,
    );
    b.separator(&file);
    b.command(
        &file,
        "Reveal in Finder",
        "",
        "",
        on_document(|d| d.reveal_file()),
        when_document(|d| d.session().path().is_some()),
    );
    b.command(
        &file,
        "Copy File Path",
        "",
        "",
        on_document(|d| d.copy_path()),
        when_document(|d| d.session().path().is_some()),
    );
    b.add_submenu(&bar, "File", &file);

    // Edit
    let edit = b.menu("Edit");
    b.standard(&edit, "Undo", sel!(undo:), "z", "⌘");
    b.standard(&edit, "Redo", sel!(redo:), "z", "⇧⌘");
    b.separator(&edit);
    b.standard(&edit, "Cut", sel!(cut:), "x", "⌘");
    b.standard(&edit, "Copy", sel!(copy:), "c", "⌘");
    b.standard(&edit, "Paste", sel!(paste:), "v", "⌘");
    b.standard(
        &edit,
        "Paste and Match Style",
        sel!(pasteAsPlainText:),
        "v",
        "⌥⇧⌘",
    );
    b.standard(&edit, "Select All", sel!(selectAll:), "a", "⌘");
    b.command(
        &edit,
        "Clear Clipboard Now",
        "",
        "",
        || app().clear_clipboard_now(),
        |_| app().can_clear_clipboard(),
    );
    b.separator(&edit);
    let find = b.menu("Find");
    let find_items: [(&str, isize, &str, &str); 5] = [
        ("Find…", 1, "f", "⌘"),
        ("Find and Replace…", 12, "f", "⌥⌘"),
        ("Find Next", 2, "g", "⌘"),
        ("Find Previous", 3, "g", "⇧⌘"),
        ("Use Selection for Find", 7, "e", "⌘"),
    ];
    for (title, tag, key, mods) in find_items {
        let item = b.standard(&find, title, sel!(performTextFinderAction:), key, mods);
        item.setTag(tag);
    }
    b.standard(
        &find,
        "Jump to Selection",
        sel!(centerSelectionInVisibleArea:),
        "j",
        "⌘",
    );
    b.add_submenu(&edit, "Find", &find);
    let spelling = b.menu("Spelling and Grammar");
    b.command(
        &spelling,
        "Check Spelling While Typing",
        "",
        "",
        || {
            app().update_preferences(|p| {
                p.check_spelling_while_typing = !p.check_spelling_while_typing
            })
        },
        |item| {
            checked(item, app().preferences().check_spelling_while_typing);
            true
        },
    );
    b.add_submenu(&edit, "Spelling and Grammar", &spelling);
    b.add_submenu(&bar, "Edit", &edit);

    // Format
    let format = b.menu("Format");
    let font = b.menu("Font");
    b.command(
        &font,
        "Show Fonts",
        "t",
        "⌘",
        || app().show_settings(false),
        |_| true,
    );
    b.add_submenu(&format, "Font", &font);
    b.command(
        &format,
        "Bigger",
        "+",
        "⌘",
        || change_font_size(1),
        |_| true,
    );
    b.command(
        &format,
        "Smaller",
        "-",
        "⌘",
        || change_font_size(-1),
        |_| true,
    );
    b.separator(&format);
    b.command(
        &format,
        "Monospaced Font",
        "",
        "",
        || {
            app().update_preferences(|p| {
                p.font = if p.font == editage_core::preferences::EditorFont::SystemMonospaced {
                    editage_core::preferences::EditorFont::System
                } else {
                    editage_core::preferences::EditorFont::SystemMonospaced
                }
            })
        },
        |item| {
            checked(
                item,
                app().preferences().font == editage_core::preferences::EditorFont::SystemMonospaced,
            );
            true
        },
    );
    b.command(
        &format,
        "Wrap Lines",
        "",
        "",
        || app().update_preferences(|p| p.wrap_lines = !p.wrap_lines),
        |item| {
            checked(item, app().preferences().wrap_lines);
            true
        },
    );
    b.add_submenu(&bar, "Format", &format);

    // View
    let view = b.menu("View");
    b.command(
        &view,
        "Show Toolbar",
        "t",
        "⌥⌘",
        || app().update_preferences(|p| p.show_toolbar = !p.show_toolbar),
        |item| {
            checked(item, app().preferences().show_toolbar);
            true
        },
    );
    b.command(
        &view,
        "Document Info",
        "i",
        "⌘",
        on_document(|d| d.toggle_info()),
        when_document(|d| d.session().is_unlocked()),
    );
    b.command(
        &view,
        "Go to Line…",
        "l",
        "⌘",
        on_document(|d| d.go_to_line()),
        when_document(|d| d.session().is_unlocked()),
    );
    b.separator(&view);
    b.standard(
        &view,
        "Enter Full Screen",
        sel!(toggleFullScreen:),
        "f",
        "⌃⌘",
    );
    b.add_submenu(&bar, "View", &view);

    // Window
    let window = b.menu("Window");
    b.standard(&window, "Minimize", sel!(performMiniaturize:), "m", "⌘");
    b.standard(&window, "Zoom", sel!(performZoom:), "", "");
    b.separator(&window);
    b.command(
        &window,
        "Diagnostics",
        "",
        "",
        || app().show_diagnostics(),
        |_| true,
    );
    b.separator(&window);
    b.standard(&window, "Bring All to Front", sel!(arrangeInFront:), "", "");
    b.add_submenu(&bar, "Window", &window);

    // Help
    let help = b.menu("Help");
    b.command(
        &help,
        "Encrypted File Compatibility",
        "",
        "",
        || app().show_about(),
        |_| true,
    );
    b.command(
        &help,
        "Recovering Your Files",
        "",
        "",
        || app().show_about(),
        |_| true,
    );
    b.add_submenu(&bar, "Help", &help);

    let application = NSApplication::sharedApplication(mtm);
    application.setMainMenu(Some(&bar));
    application.setWindowsMenu(Some(&window));
    application.setHelpMenu(Some(&help));
    rebuild_open_recent(app_ref);
}

fn change_font_size(delta: i32) {
    app().update_preferences(|p| {
        let size = (p.font_size as i32 + delta).clamp(
            *editage_core::preferences::FONT_SIZE_RANGE.start() as i32,
            *editage_core::preferences::FONT_SIZE_RANGE.end() as i32,
        );
        p.font_size = size as u32;
    });
}

/// Rebuilds File → Open Recent from preferences.
pub fn rebuild_open_recent(app_ref: &App) {
    let Some(menu) = OPEN_RECENT.with(|cell| cell.borrow().clone()) else {
        return;
    };
    menu.removeAllItems();
    OPEN_RECENT_TARGETS.with(|bag| {
        bag.clear();
        fill_open_recent(app_ref, &menu, bag);
    });
}

fn fill_open_recent(app_ref: &App, menu: &NSMenu, bag: &TargetBag) {
    let b = MenuBuilder {
        bag,
        mtm: app_ref.mtm,
    };
    let preferences = app_ref.preferences();
    for path in preferences.recent_documents.clone() {
        let title = format!(
            "{}  —  {}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path.parent().map(abbreviate_home).unwrap_or_default()
        );
        b.command(
            menu,
            &title,
            "",
            "",
            move || app().open_path(path.clone()),
            |_| true,
        );
    }
    if !preferences.recent_documents.is_empty() {
        b.separator(menu);
    }
    let remembering = preferences.remember_recent_documents;
    b.command(
        menu,
        "Clear Menu",
        "",
        "",
        || app().update_preferences(|p| p.recent_documents.clear()),
        move |_| remembering && !app().preferences().recent_documents.is_empty(),
    );
}
