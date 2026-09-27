//! A small `NSToolbarDelegate` that builds toolbar items from a list, used by
//! the document window (Lock and Info buttons) and the Settings window
//! (General and Security tabs).

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly, Message};
use objc2_app_kit::{
    NSImage, NSToolbar, NSToolbarDelegate, NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSString};

use crate::controls::{ns, ActionTarget};

pub struct ToolbarItemSpec {
    pub identifier: &'static str,
    pub label: &'static str,
    pub symbol: &'static str,
    pub tooltip: &'static str,
    pub target: Retained<ActionTarget>,
}

pub struct ToolbarDelegateIvars {
    items: Vec<ToolbarItemSpec>,
    /// Push items to the trailing edge with a flexible space.
    trailing: bool,
    /// Items act as selectable tabs (Settings window).
    selectable: bool,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageToolbarDelegate"]
    #[ivars = ToolbarDelegateIvars]
    pub struct ToolbarDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ToolbarDelegate {}

    // SAFETY: the method signatures match NSToolbarDelegate.
    unsafe impl NSToolbarDelegate for ToolbarDelegate {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn item_for_identifier(
            &self,
            _toolbar: &NSToolbar,
            identifier: &NSString,
            _will_be_inserted: bool,
        ) -> Option<Retained<NSToolbarItem>> {
            self.make_item(identifier)
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_identifiers(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            self.identifiers()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_identifiers(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            self.identifiers()
        }

        #[unsafe(method_id(toolbarSelectableItemIdentifiers:))]
        fn selectable_identifiers(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            if self.ivars().selectable {
                self.identifiers()
            } else {
                NSArray::new()
            }
        }
    }
);

impl ToolbarDelegate {
    pub fn new(
        mtm: MainThreadMarker,
        items: Vec<ToolbarItemSpec>,
        trailing: bool,
        selectable: bool,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ToolbarDelegateIvars {
            items,
            trailing,
            selectable,
        });
        // SAFETY: NSObject's `init` has this signature.
        unsafe { msg_send![super(this), init] }
    }

    fn make_item(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        let wanted = identifier.to_string();
        let spec = self
            .ivars()
            .items
            .iter()
            .find(|spec| wanted == spec.identifier)?;
        let item =
            NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
        item.setLabel(&ns(spec.label));
        item.setPaletteLabel(&ns(spec.label));
        item.setToolTip(Some(&ns(spec.tooltip)));
        if let Some(image) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &ns(spec.symbol),
            Some(&ns(spec.label)),
        ) {
            item.setImage(Some(&image));
        }
        // SAFETY: the target is retained by the delegate's ivars for the
        // toolbar's lifetime; `perform:` exists on it.
        unsafe {
            item.setTarget(Some(spec.target.as_object()));
            item.setAction(Some(ActionTarget::selector()));
        }
        if !self.ivars().selectable {
            item.setBordered(true);
        }
        Some(item)
    }

    fn identifiers(&self) -> Retained<NSArray<NSString>> {
        let mut identifiers: Vec<Retained<NSString>> = Vec::new();
        if self.ivars().trailing {
            // SAFETY: a framework-provided constant.
            identifiers.push(unsafe { NSToolbarFlexibleSpaceItemIdentifier }.retain());
        }
        for spec in &self.ivars().items {
            identifiers.push(ns(spec.identifier));
        }
        NSArray::from_retained_slice(&identifiers)
    }

    /// Creates a toolbar using this delegate.
    pub fn make_toolbar(&self, identifier: &str, mtm: MainThreadMarker) -> Retained<NSToolbar> {
        let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &ns(identifier));
        toolbar.setDelegate(Some(ProtocolObject::from_ref(self)));
        toolbar.setAllowsUserCustomization(false);
        toolbar.setDisplayMode(if self.ivars().selectable {
            objc2_app_kit::NSToolbarDisplayMode::IconAndLabel
        } else {
            objc2_app_kit::NSToolbarDisplayMode::IconOnly
        });
        toolbar
    }
}
