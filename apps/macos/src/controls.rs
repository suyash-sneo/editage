//! Small AppKit helpers shared by every window: closure-backed action
//! targets, label and button constructors, stacks, and formatting.
//!
//! AppKit controls send actions to a target object. `ActionTarget` is an
//! Objective-C object that forwards its action to a Rust closure, which
//! keeps all real behaviour in ordinary Rust code. Controls hold their
//! target weakly, so owners keep targets alive in a `TargetBag`.

use std::cell::RefCell;
use std::path::Path;
use std::time::SystemTime;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSButton, NSColor, NSControlSize, NSControlTextEditingDelegate, NSFont, NSFontWeightRegular,
    NSFontWeightSemibold, NSLayoutAttribute, NSMenuItem, NSStackView, NSStackViewDistribution,
    NSTextField, NSTextFieldDelegate, NSUserInterfaceLayoutOrientation, NSView, NSWindowDelegate,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSDate, NSDateFormatter, NSDateFormatterStyle, NSObject,
    NSObjectProtocol, NSString,
};

pub fn ns(text: &str) -> Retained<NSString> {
    NSString::from_str(text)
}

/// Decides whether a menu item is enabled (and may set its check mark).
type MenuValidation = Box<dyn Fn(&NSMenuItem) -> bool>;

pub struct ActionTargetIvars {
    action: Box<dyn Fn()>,
    /// For menu items: decides whether the item is enabled and may update
    /// its check mark.
    validate: Option<MenuValidation>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and this class does
    // not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageActionTarget"]
    #[ivars = ActionTargetIvars]
    pub struct ActionTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ActionTarget {}

    impl ActionTarget {
        // SAFETY: standard action method signature `- (void)perform:(id)sender`.
        #[unsafe(method(perform:))]
        fn perform(&self, _sender: Option<&AnyObject>) {
            (self.ivars().action)();
        }

        // SAFETY: matches `- (BOOL)validateMenuItem:(NSMenuItem *)item`.
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            match &self.ivars().validate {
                Some(validate) => validate(item),
                None => true,
            }
        }
    }
);

impl ActionTarget {
    pub fn new(mtm: MainThreadMarker, action: impl Fn() + 'static) -> Retained<Self> {
        Self::build(mtm, Box::new(action), None)
    }

    pub fn validated(
        mtm: MainThreadMarker,
        action: impl Fn() + 'static,
        validate: impl Fn(&NSMenuItem) -> bool + 'static,
    ) -> Retained<Self> {
        Self::build(mtm, Box::new(action), Some(Box::new(validate)))
    }

    fn build(
        mtm: MainThreadMarker,
        action: Box<dyn Fn()>,
        validate: Option<MenuValidation>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ActionTargetIvars { action, validate });
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn selector() -> Sel {
        sel!(perform:)
    }

    pub fn as_object(&self) -> &AnyObject {
        self.as_ref()
    }
}

/// Keeps action targets alive for as long as their controls exist.
#[derive(Default)]
pub struct TargetBag {
    targets: RefCell<Vec<Retained<ActionTarget>>>,
    /// Delegates and other helper objects that AppKit references weakly.
    objects: RefCell<Vec<Retained<NSObject>>>,
}

impl TargetBag {
    pub fn keep(&self, target: Retained<ActionTarget>) -> Retained<ActionTarget> {
        self.targets.borrow_mut().push(target.clone());
        target
    }

    pub fn keep_object(&self, object: Retained<NSObject>) {
        self.objects.borrow_mut().push(object);
    }

    /// Releases the kept objects on the next main-queue turn, never
    /// immediately: `clear` is often reached from inside one of these
    /// targets' own actions, and releasing an object AppKit is currently
    /// messaging would be a use-after-free.
    pub fn clear(&self) {
        let targets = std::mem::take(&mut *self.targets.borrow_mut());
        let objects = std::mem::take(&mut *self.objects.borrow_mut());
        crate::app::defer_on_main_thread(Box::new(move || {
            drop(targets);
            drop(objects);
        }));
    }
}

/// A push button calling `action`.
pub fn button(
    title: &str,
    bag: &TargetBag,
    mtm: MainThreadMarker,
    action: impl Fn() + 'static,
) -> Retained<NSButton> {
    let target = bag.keep(ActionTarget::new(mtm, action));
    // SAFETY: the target is kept alive by `bag`; the selector exists on it.
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &ns(title),
            Some(target.as_object()),
            Some(ActionTarget::selector()),
            mtm,
        )
    }
}

/// Makes a button the sheet's default (Return) button.
pub fn make_default(button: &NSButton) {
    button.setKeyEquivalent(&ns("\r"));
}

/// Makes a button respond to Escape.
pub fn make_cancel(button: &NSButton) {
    button.setKeyEquivalent(&ns("\u{1b}"));
}

/// A checkbox calling `action` when toggled.
pub fn checkbox(
    title: &str,
    checked: bool,
    bag: &TargetBag,
    mtm: MainThreadMarker,
    action: impl Fn(bool) + 'static,
) -> Retained<NSButton> {
    let cell: std::rc::Rc<RefCell<Option<Retained<NSButton>>>> = Default::default();
    let weak_cell = cell.clone();
    let target = bag.keep(ActionTarget::new(mtm, move || {
        if let Some(button) = weak_cell.borrow().as_ref() {
            action(button.state() == objc2_app_kit::NSControlStateValueOn);
        }
    }));
    // SAFETY: the target is kept alive by `bag`.
    let checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &ns(title),
            Some(target.as_object()),
            Some(ActionTarget::selector()),
            mtm,
        )
    };
    checkbox.setState(if checked {
        objc2_app_kit::NSControlStateValueOn
    } else {
        objc2_app_kit::NSControlStateValueOff
    });
    // The closure needs the button to read its state; a strong reference
    // from target to button is fine because the bag and the view hierarchy
    // are released together.
    *cell.borrow_mut() = Some(checkbox.clone());
    checkbox
}

pub fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(text), mtm);
    field.setSelectable(true);
    field
}

pub fn wrapping_label(text: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::wrappingLabelWithString(&ns(text), mtm);
    field.setSelectable(true);
    field.setPreferredMaxLayoutWidth(width);
    field
}

pub fn title_label(text: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = wrapping_label(text, width, mtm);
    // SAFETY: NSFontWeightSemibold is a valid font weight constant.
    field.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, unsafe {
        NSFontWeightSemibold
    })));
    field
}

pub fn small_label(text: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = wrapping_label(text, width, mtm);
    // SAFETY: NSFontWeightRegular is a valid font weight constant.
    field.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, unsafe {
        NSFontWeightRegular
    })));
    field
}

pub fn secondary_label(text: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = small_label(text, width, mtm);
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}

pub fn error_label(text: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = small_label(text, width, mtm);
    field.setTextColor(Some(&NSColor::systemRedColor()));
    field
}

/// Right-aligned secondary label for key/value grids.
pub fn key_label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(text), mtm);
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    field
}

pub fn value_label(text: &str, width: f64, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = wrapping_label(text, width, mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    field
}

fn stack(
    views: &[&NSView],
    orientation: NSUserInterfaceLayoutOrientation,
    spacing: f64,
    mtm: MainThreadMarker,
) -> Retained<NSStackView> {
    let array = NSArray::from_slice(views);
    let stack = NSStackView::stackViewWithViews(&array, mtm);
    stack.setOrientation(orientation);
    stack.setSpacing(spacing);
    stack
}

/// A vertical stack, leading-aligned.
pub fn vstack(views: &[&NSView], spacing: f64, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = stack(
        views,
        NSUserInterfaceLayoutOrientation::Vertical,
        spacing,
        mtm,
    );
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack
}

/// A horizontal stack, vertically centred.
pub fn hstack(views: &[&NSView], spacing: f64, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = stack(
        views,
        NSUserInterfaceLayoutOrientation::Horizontal,
        spacing,
        mtm,
    );
    stack.setAlignment(NSLayoutAttribute::CenterY);
    stack.setDistribution(NSStackViewDistribution::Fill);
    stack
}

/// An empty view that expands horizontally inside a stack.
pub fn flexible_space(mtm: MainThreadMarker) -> Retained<NSView> {
    let view = NSView::new(mtm);
    view.setContentHuggingPriority_forOrientation(
        1.0,
        objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
    );
    view
}

pub fn small_button(button: &NSButton) {
    button.setControlSize(NSControlSize::Small);
    button.setFont(Some(&NSFont::systemFontOfSize(11.0)));
}

/// Replaces the user's home folder with `~` for display.
pub fn abbreviate_home(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        if let Ok(relative) = path.strip_prefix(&home) {
            if relative.as_os_str().is_empty() {
                return "~".to_owned();
            }
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

fn formatter(
    time_style: NSDateFormatterStyle,
    date_style: NSDateFormatterStyle,
) -> Retained<NSDateFormatter> {
    let formatter = NSDateFormatter::new();
    formatter.setTimeStyle(time_style);
    formatter.setDateStyle(date_style);
    formatter
}

fn to_nsdate(time: SystemTime) -> Retained<NSDate> {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0);
    NSDate::dateWithTimeIntervalSince1970(seconds)
}

/// "1:42:16 PM" in the user's locale.
pub fn format_time(time: SystemTime) -> String {
    let formatter = formatter(
        NSDateFormatterStyle::MediumStyle,
        NSDateFormatterStyle::NoStyle,
    );
    formatter.stringFromDate(&to_nsdate(time)).to_string()
}

/// "Today at 09:12" style date and time, for file modification dates.
pub fn format_date_time(time: SystemTime) -> String {
    let formatter = formatter(
        NSDateFormatterStyle::ShortStyle,
        NSDateFormatterStyle::MediumStyle,
    );
    formatter.setDoesRelativeDateFormatting(true);
    formatter.stringFromDate(&to_nsdate(time)).to_string()
}

/// "4.2 KB" style sizes.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} bytes")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub struct FieldObserverIvars {
    on_change: Box<dyn Fn()>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageFieldObserver"]
    #[ivars = FieldObserverIvars]
    pub struct FieldObserver;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for FieldObserver {}

    // SAFETY: the implemented method matches the protocol's signature.
    unsafe impl NSControlTextEditingDelegate for FieldObserver {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &objc2_foundation::NSNotification) {
            (self.ivars().on_change)();
        }
    }

    // SAFETY: no required methods.
    unsafe impl NSTextFieldDelegate for FieldObserver {}
);

impl FieldObserver {
    /// Calls `on_change` whenever the observed field's text changes.
    pub fn new(mtm: MainThreadMarker, on_change: impl Fn() + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(FieldObserverIvars {
            on_change: Box::new(on_change),
        });
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    pub fn observe(&self, field: &NSTextField) {
        // SAFETY: the field holds its delegate weakly; callers keep this
        // observer alive for as long as the field exists.
        unsafe { field.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self))) };
    }
}

/// Callbacks for window events. `should_close` decides `windowShouldClose:`.
#[derive(Default)]
pub struct WindowObserverCallbacks {
    pub should_close: Option<Box<dyn Fn() -> bool>>,
    pub did_become_key: Option<Box<dyn Fn()>>,
    pub did_become_main: Option<Box<dyn Fn()>>,
    pub will_close: Option<Box<dyn Fn()>>,
}

pub struct WindowObserverIvars {
    callbacks: WindowObserverCallbacks,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageWindowObserver"]
    #[ivars = WindowObserverIvars]
    pub struct WindowObserver;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for WindowObserver {}

    // SAFETY: the implemented methods match NSWindowDelegate's signatures.
    unsafe impl NSWindowDelegate for WindowObserver {
        #[unsafe(method(windowShouldClose:))]
        fn window_should_close(&self, _sender: &objc2_app_kit::NSWindow) -> bool {
            match &self.ivars().callbacks.should_close {
                Some(should_close) => should_close(),
                None => true,
            }
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn window_did_become_key(&self, _notification: &objc2_foundation::NSNotification) {
            if let Some(callback) = &self.ivars().callbacks.did_become_key {
                callback();
            }
        }

        #[unsafe(method(windowDidBecomeMain:))]
        fn window_did_become_main(&self, _notification: &objc2_foundation::NSNotification) {
            if let Some(callback) = &self.ivars().callbacks.did_become_main {
                callback();
            }
        }

        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &objc2_foundation::NSNotification) {
            if let Some(callback) = &self.ivars().callbacks.will_close {
                callback();
            }
        }
    }
);

impl WindowObserver {
    pub fn new(mtm: MainThreadMarker, callbacks: WindowObserverCallbacks) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WindowObserverIvars { callbacks });
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    /// Sets this observer as the window's delegate. The window holds its
    /// delegate weakly; the caller must keep the observer alive.
    pub fn attach(&self, window: &objc2_app_kit::NSWindow) {
        window.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self)));
    }
}

define_class!(
    // SAFETY: NSView supports subclassing; only `isFlipped` is overridden.
    #[unsafe(super(NSView, objc2_app_kit::NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageFlippedView"]
    pub struct FlippedView;

    impl FlippedView {
        // SAFETY: `- (BOOL)isFlipped` takes no arguments.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    /// A view whose origin is at the top-left, so scrolled content starts at
    /// the top.
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSView's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }
}

/// Pins `child` to all four edges of `parent` using Auto Layout.
pub fn pin_to_edges(child: &NSView, parent: &NSView) {
    child.setTranslatesAutoresizingMaskIntoConstraints(false);
    child
        .leadingAnchor()
        .constraintEqualToAnchor(&parent.leadingAnchor())
        .setActive(true);
    child
        .trailingAnchor()
        .constraintEqualToAnchor(&parent.trailingAnchor())
        .setActive(true);
    child
        .topAnchor()
        .constraintEqualToAnchor(&parent.topAnchor())
        .setActive(true);
    child
        .bottomAnchor()
        .constraintEqualToAnchor(&parent.bottomAnchor())
        .setActive(true);
}

/// A section heading in the style of the design's inspector ("ON DISK").
pub fn section_heading(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(&text.to_uppercase()), mtm);
    // SAFETY: NSFontWeightSemibold is a valid font weight constant.
    field.setFont(Some(&NSFont::systemFontOfSize_weight(10.5, unsafe {
        NSFontWeightSemibold
    })));
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}
