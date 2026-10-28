//! Start-at-login, per user only (never system-wide, never needs admin):
//!   Windows: HKCU\...\Run   macOS: ~/Library/LaunchAgents   Linux: ~/.config/autostart
//! Starts with `--hidden` so the picker stays out of the way until the hotkey.

use auto_launch::{AutoLaunch, AutoLaunchBuilder};

fn launcher() -> Option<AutoLaunch> {
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?;
    let mut b = AutoLaunchBuilder::new();
    b.set_app_name("clipguard").set_app_path(exe).set_args(&["--hidden"]);
    #[cfg(windows)]
    b.set_windows_enable_mode(auto_launch::WindowsEnableMode::CurrentUser);
    b.build().ok()
}

pub fn is_supported() -> bool {
    launcher().is_some()
}

pub fn is_enabled() -> bool {
    launcher().and_then(|l| l.is_enabled().ok()).unwrap_or(false)
}

/// Returns the state actually in effect afterwards.
pub fn set(on: bool) -> bool {
    let Some(l) = launcher() else { return false };
    let res = if on { l.enable() } else { l.disable() };
    if let Err(e) = res {
        eprintln!("[autostart] {e}");
    }
    is_enabled()
}
