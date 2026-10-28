//! "Paste into the previous app": after our window hides and focus returns,
//! send the platform paste shortcut. macOS needs Accessibility permission;
//! without it the item is still on the clipboard and the user pastes manually.

use std::time::Duration;

pub fn paste_into_previous_app() {
    std::thread::spawn(|| {
        // Give the OS time to hand focus back to the previous window.
        std::thread::sleep(Duration::from_millis(150));
        if let Err(e) = send_paste() {
            eprintln!("[paste] auto-paste unavailable ({e}); item is on the clipboard");
        }
    });
}

fn send_paste() -> Result<(), String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    let modifier = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modifier = Key::Control;

    enigo.key(modifier, Direction::Press).map_err(|e| e.to_string())?;
    let r = enigo.key(Key::Unicode('v'), Direction::Click).map_err(|e| e.to_string());
    enigo.key(modifier, Direction::Release).map_err(|e| e.to_string())?;
    r
}

/// macOS: hiding only our window does not give focus back; hide the whole app.
#[cfg(target_os = "macos")]
pub fn hide_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).hide(None);
    }
}

#[cfg(target_os = "macos")]
pub fn show_app() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    if let Some(mtm) = MainThreadMarker::new() {
        let app = NSApplication::sharedApplication(mtm);
        app.unhide(None);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_app() {}
#[cfg(not(target_os = "macos"))]
pub fn show_app() {}
