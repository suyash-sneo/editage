//! The document's text editor: a native `NSTextView` in a scroll view.
//!
//! All editing behaviour (selection, input methods, undo, find, context
//! menu, accessibility) is AppKit's own. This subclass only:
//! - reports edits to the document session (`didChangeText`),
//! - reports copies so the clipboard tracker knows what it owns
//!   (`copy:` and `cut:`),
//! - turns off automatic text changes that would corrupt passwords or send
//!   text elsewhere.
//!
//! The text storage of this view is where the plaintext lives while a
//! document is unlocked. Converting between `NSString` and Rust `String`
//! copies the text; those copies are kept as short-lived as possible.

use std::cell::RefCell;

use editage_core::preferences::{EditorFont, Preferences};
use editage_core::secrets::Plaintext;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBorderType, NSFont, NSFontWeightRegular, NSMutableParagraphStyle,
    NSResponder, NSScrollView, NSText, NSTextView, NSView,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize};

use crate::controls::ns;

#[derive(Default)]
pub struct EditorCallbacks {
    pub on_edit: Option<Box<dyn Fn()>>,
    pub on_copy: Option<Box<dyn Fn()>>,
}

pub struct EditorIvars {
    callbacks: RefCell<EditorCallbacks>,
}

define_class!(
    // SAFETY: NSTextView supports subclassing; overridden methods call super
    // first and keep their original signatures. No Drop implementation.
    #[unsafe(super(NSTextView, NSText, NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "EditageEditorTextView"]
    #[ivars = EditorIvars]
    pub struct EditorTextView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for EditorTextView {}

    impl EditorTextView {
        // SAFETY: `- (void)didChangeText` takes no arguments.
        #[unsafe(method(didChangeText))]
        fn did_change_text(&self) {
            // SAFETY: calling the superclass implementation with the same
            // (empty) arguments.
            let _: () = unsafe { msg_send![super(self), didChangeText] };
            if let Ok(callbacks) = self.ivars().callbacks.try_borrow() {
                if let Some(on_edit) = &callbacks.on_edit {
                    on_edit();
                }
            }
        }

        // SAFETY: standard action signature `- (void)copy:(id)sender`.
        #[unsafe(method(copy:))]
        fn copy(&self, sender: Option<&AnyObject>) {
            // SAFETY: forwarding the same argument to the superclass.
            let _: () = unsafe { msg_send![super(self), copy: sender] };
            self.notify_copy();
        }

        // SAFETY: standard action signature `- (void)cut:(id)sender`.
        #[unsafe(method(cut:))]
        fn cut(&self, sender: Option<&AnyObject>) {
            // SAFETY: forwarding the same argument to the superclass.
            let _: () = unsafe { msg_send![super(self), cut: sender] };
            self.notify_copy();
        }
    }
);

impl EditorTextView {
    fn notify_copy(&self) {
        if let Ok(callbacks) = self.ivars().callbacks.try_borrow() {
            if let Some(on_copy) = &callbacks.on_copy {
                on_copy();
            }
        }
    }

    fn create(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EditorIvars {
            callbacks: RefCell::new(EditorCallbacks::default()),
        });
        // SAFETY: `initWithFrame:` is NSTextView's designated initializer.
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    pub fn set_callbacks(&self, callbacks: EditorCallbacks) {
        *self.ivars().callbacks.borrow_mut() = callbacks;
    }

    /// Copies the editor's text into a zeroize-on-drop buffer for saving.
    ///
    /// This is one of the documented plaintext copy boundaries: AppKit's
    /// string is copied into Rust memory, handed to the save transaction,
    /// and zeroized right after encryption.
    pub fn plaintext_for_saving(&self) -> Plaintext {
        let string = self.string();
        Plaintext::from_string(string.to_string())
    }

    /// Current text length in UTF-8 bytes (for display only).
    pub fn utf8_length(&self) -> usize {
        self.string()
            .lengthOfBytesUsingEncoding(objc2_foundation::NSUTF8StringEncoding)
    }

    /// Replaces the text with decrypted plaintext. The caller drops the
    /// `Plaintext` right after this returns.
    pub fn load_plaintext(&self, plaintext: &Plaintext) {
        let callbacks = std::mem::take(&mut *self.ivars().callbacks.borrow_mut());
        self.setString(&ns(plaintext.as_str()));
        *self.ivars().callbacks.borrow_mut() = callbacks;
        self.scrollRangeToVisible(objc2_foundation::NSRange::new(0, 0));
        self.setSelectedRange(objc2_foundation::NSRange::new(0, 0));
    }

    /// Removes all text and every undo action.
    ///
    /// Returns whether an undo manager was found and cleared.
    pub fn clear_text_and_undo(&self) -> bool {
        let callbacks = std::mem::take(&mut *self.ivars().callbacks.borrow_mut());
        self.setString(&ns(""));
        *self.ivars().callbacks.borrow_mut() = callbacks;
        self.breakUndoCoalescing();
        match self.undoManager() {
            Some(undo_manager) => {
                undo_manager.removeAllActions();
                true
            }
            None => false,
        }
    }

    pub fn clear_undo_history(&self) {
        self.breakUndoCoalescing();
        if let Some(undo_manager) = self.undoManager() {
            undo_manager.removeAllActions();
        }
    }

    /// Applies font, wrapping and spelling preferences.
    pub fn apply_preferences(&self, preferences: &Preferences, scroll_view: &NSScrollView) {
        let size = f64::from(preferences.font_size);
        let font = match preferences.font {
            // SAFETY: NSFontWeightRegular is a valid weight constant.
            EditorFont::SystemMonospaced => {
                NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular })
            }
            EditorFont::System => NSFont::systemFontOfSize(size),
            EditorFont::Palatino => NSFont::fontWithName_size(&ns("Palatino"), size)
                .unwrap_or_else(|| NSFont::systemFontOfSize(size)),
        };
        self.setFont(Some(&font));
        let paragraph = NSMutableParagraphStyle::new();
        paragraph.setLineHeightMultiple(1.2);
        self.setDefaultParagraphStyle(Some(&paragraph));
        // Re-apply the style to existing text as well as future typing.
        // SAFETY: plain property access on the main thread.
        if let Some(storage) = unsafe { self.textStorage() } {
            let length = storage.length();
            // SAFETY: the attribute name constant is valid and the value is
            // an NSParagraphStyle, as the attribute requires.
            unsafe {
                storage.addAttribute_value_range(
                    objc2_app_kit::NSParagraphStyleAttributeName,
                    &paragraph,
                    objc2_foundation::NSRange::new(0, length),
                );
            }
        }

        self.setContinuousSpellCheckingEnabled(preferences.check_spelling_while_typing);
        self.set_wrapping(preferences.wrap_lines, scroll_view);
    }

    fn set_wrapping(&self, wrap: bool, scroll_view: &NSScrollView) {
        // SAFETY: plain property access on the main thread.
        let Some(container) = (unsafe { self.textContainer() }) else {
            return;
        };
        let huge = f64::MAX / 2.0;
        if wrap {
            let width = scroll_view.contentSize().width;
            scroll_view.setHasHorizontalScroller(false);
            self.setHorizontallyResizable(false);
            self.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
            container.setContainerSize(NSSize::new(width, huge));
            container.setWidthTracksTextView(true);
            let mut frame = self.frame();
            frame.size.width = width;
            self.setFrame(frame);
        } else {
            scroll_view.setHasHorizontalScroller(true);
            self.setHorizontallyResizable(true);
            self.setAutoresizingMask(NSAutoresizingMaskOptions::empty());
            container.setWidthTracksTextView(false);
            container.setContainerSize(NSSize::new(huge, huge));
        }
    }
}

/// Builds a scroll view containing a configured editor.
pub fn make_editor(mtm: MainThreadMarker) -> (Retained<NSScrollView>, Retained<EditorTextView>) {
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(720.0, 520.0));
    let scroll_view = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll_view.setHasVerticalScroller(true);
    scroll_view.setAutohidesScrollers(true);
    scroll_view.setBorderType(NSBorderType::NoBorder);
    scroll_view.setAutoresizingMask(
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
    );

    let content_size = scroll_view.contentSize();
    let text_view = EditorTextView::create(mtm, NSRect::new(NSPoint::new(0.0, 0.0), content_size));
    text_view.setMinSize(NSSize::new(0.0, content_size.height));
    text_view.setMaxSize(NSSize::new(f64::MAX / 2.0, f64::MAX / 2.0));
    text_view.setVerticallyResizable(true);
    text_view.setHorizontallyResizable(false);
    text_view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    // SAFETY: plain property access on the main thread.
    if let Some(container) = unsafe { text_view.textContainer() } {
        container.setContainerSize(NSSize::new(content_size.width, f64::MAX / 2.0));
        container.setWidthTracksTextView(true);
    }
    // Roughly the design's 22 px / 30 px padding.
    text_view.setTextContainerInset(NSSize::new(26.0, 20.0));

    configure_text_behaviour(&text_view);
    scroll_view.setDocumentView(Some(&text_view));
    (scroll_view, text_view)
}

/// Plain-text editing with native behaviour, minus automatic changes.
fn configure_text_behaviour(text_view: &EditorTextView) {
    text_view.setRichText(false);
    text_view.setImportsGraphics(false);
    text_view.setAllowsUndo(true);
    text_view.setUsesFindBar(true);
    text_view.setIncrementalSearchingEnabled(true);
    text_view.setAllowsDocumentBackgroundColorChange(false);

    // Automatic substitutions would silently change what the user typed:
    // "--" into a dash, straight quotes into curly quotes, words into
    // autocorrected words. In a file of passwords and codes that corrupts
    // data, so all of them are off.
    text_view.setAutomaticQuoteSubstitutionEnabled(false);
    text_view.setAutomaticDashSubstitutionEnabled(false);
    text_view.setAutomaticTextReplacementEnabled(false);
    text_view.setAutomaticSpellingCorrectionEnabled(false);
    text_view.setAutomaticLinkDetectionEnabled(false);
    text_view.setAutomaticDataDetectionEnabled(false);
    text_view.setSmartInsertDeleteEnabled(false);
    text_view.setGrammarCheckingEnabled(false);
    text_view.setAutomaticTextCompletionEnabled(false);

    // Writing Tools (macOS 15+) can send selected text to Apple's servers.
    // It is disabled for document text. The selector does not exist on
    // older macOS, so check before calling.
    let responds: bool = text_view.respondsToSelector(sel!(setWritingToolsBehavior:));
    if responds {
        // NSWritingToolsBehaviorNone = -1
        // SAFETY: the selector exists (checked above) and takes an NSInteger.
        let _: () = unsafe { msg_send![text_view, setWritingToolsBehavior: -1isize] };
    }
}
