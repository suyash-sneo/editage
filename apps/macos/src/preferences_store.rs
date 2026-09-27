//! Persisting `Preferences` in `NSUserDefaults`.
//!
//! Every key is defined in `editage_core::preferences::keys` and stored as a
//! string, so `defaults read Editage` shows exactly what is kept. For the
//! unbundled binary the domain is the executable name ("Editage"), stored in
//! ~/Library/Preferences/Editage.plist.

use editage_core::preferences::Preferences;
use objc2_foundation::{NSNumber, NSString, NSUserDefaults};

use crate::controls::ns;

pub fn load_preferences() -> Preferences {
    let defaults = NSUserDefaults::standardUserDefaults();
    Preferences::from_entries(|key| {
        defaults
            .stringForKey(&ns(key))
            .map(|value| value.to_string())
    })
}

pub fn save_preferences(preferences: &Preferences) {
    let defaults = NSUserDefaults::standardUserDefaults();
    for (key, value) in preferences.to_entries() {
        let value = NSString::from_str(&value);
        // SAFETY: an NSString is a valid property-list object.
        unsafe { defaults.setObject_forKey(Some(&value), &ns(key)) };
    }
}

/// Where preferences are stored, for display in Settings.
pub fn preferences_location() -> String {
    "~/Library/Preferences/Editage.plist".to_owned()
}

/// Turns off macOS window restoration for this application.
///
/// Window restoration can persist window state (and, for document-based
/// apps, document contents) across launches. Editage never uses it: windows
/// are marked non-restorable and these defaults stop macOS from saving
/// restoration state at quit.
pub fn disable_state_restoration() {
    let defaults = NSUserDefaults::standardUserDefaults();
    let no = NSNumber::new_bool(false);
    let yes = NSNumber::new_bool(true);
    // SAFETY: NSNumber is a valid property-list object.
    unsafe {
        defaults.setObject_forKey(Some(&no), &ns("NSQuitAlwaysKeepsWindows"));
        defaults.setObject_forKey(Some(&yes), &ns("ApplePersistenceIgnoreState"));
    }
}
