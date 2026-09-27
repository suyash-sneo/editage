//! Editage for macOS: a native AppKit frontend over `editage-core`.
//!
//! All document, encryption and save logic lives in the portable core crate.
//! This crate owns windows, menus, sheets and the native text view. Unsafe
//! code is limited to Objective-C interop in this crate and each block
//! states why it is sound.

#[cfg(target_os = "macos")]
mod about_window;
#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod background;
#[cfg(target_os = "macos")]
mod clipboard;
#[cfg(target_os = "macos")]
mod controls;
#[cfg(target_os = "macos")]
mod diagnostics_window;
#[cfg(target_os = "macos")]
mod document_window;
#[cfg(target_os = "macos")]
mod editor_view;
#[cfg(target_os = "macos")]
mod info_popover;
#[cfg(target_os = "macos")]
mod menu;
#[cfg(target_os = "macos")]
mod password_sheet;
#[cfg(target_os = "macos")]
mod preferences_store;
#[cfg(target_os = "macos")]
mod security_inspector;
#[cfg(target_os = "macos")]
mod settings_window;
#[cfg(target_os = "macos")]
mod sheets;
#[cfg(target_os = "macos")]
mod toolbar;
#[cfg(target_os = "macos")]
mod welcome_window;

#[cfg(target_os = "macos")]
fn main() {
    // A panic must never write recovery data. The default hook prints the
    // panic message and location only; no document state is formatted,
    // and no type holding plaintext or secrets implements a revealing Debug.
    let mtm =
        objc2_foundation::MainThreadMarker::new().expect("Editage must start on the main thread");
    app::run(mtm);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This frontend is for macOS. The portable core is in crates/editage-core.");
    std::process::exit(1);
}
