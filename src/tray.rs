//! System tray: left click opens the picker, right click shows the menu.
//!
//!   Open (hotkey) · Pause capture · Start at login · Quit
//!
//! Linux uses the StatusNotifierItem (D-Bus) backend: works on KDE, XFCE, Cinnamon…;
//! GNOME needs the "AppIndicator and KStatusNotifierItem Support" extension.

use crate::autostart;
use crate::icon;
use eframe::egui;
use std::sync::{Arc, Mutex};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Toggle,
    PauseClicked,
    AutostartClicked,
    Quit,
}

pub struct Tray {
    _icon: TrayIcon,
    pause: CheckMenuItem,
    autostart: CheckMenuItem,
}

// Menu items are not thread-safe, so the event handler only reports *which* item was
// clicked; the UI thread reads the (already flipped) check state from here.
impl Tray {
    pub fn pause_checked(&self) -> bool {
        self.pause.is_checked()
    }

    pub fn autostart_checked(&self) -> bool {
        self.autostart.is_checked()
    }

    /// Reflect the real autostart state back into the menu (e.g. after a failed toggle).
    pub fn set_autostart_checked(&self, on: bool) {
        self.autostart.set_checked(on);
    }
}

/// Builds the tray and wires its events into `queue` (drained by the UI thread).
/// Must run on the main thread (macOS/Windows requirement).
pub fn build(ctx: &egui::Context, queue: Arc<Mutex<Vec<Action>>>, hotkey: &str) -> Option<Tray> {
    let open = MenuItem::new(format!("Open  ({hotkey})"), true, None);
    let pause = CheckMenuItem::new("Pause capture", true, false, None);
    let login_label = if cfg!(windows) { "Start with Windows" } else { "Start at login" };
    let start = CheckMenuItem::new(login_label, autostart::is_supported(), autostart::is_enabled(), None);
    let quit = MenuItem::new("Quit clipguard", true, None);

    let menu = Menu::new();
    let sep = PredefinedMenuItem::separator();
    menu.append_items(&[&open, &pause, &start, &sep, &quit]).ok()?;

    let ids: [(MenuId, &'static str); 4] = [
        (open.id().clone(), "open"),
        (pause.id().clone(), "pause"),
        (start.id().clone(), "start"),
        (quit.id().clone(), "quit"),
    ];
    {
        let (q, c) = (queue.clone(), ctx.clone());
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let action = match ids.iter().find(|(id, _)| *id == e.id).map(|(_, n)| *n) {
                Some("open") => Action::Toggle,
                Some("pause") => Action::PauseClicked,
                Some("start") => Action::AutostartClicked,
                Some("quit") => Action::Quit,
                _ => return,
            };
            q.lock().unwrap_or_else(|p| p.into_inner()).push(action);
            c.request_repaint();
        }));
    }
    {
        let (q, c) = (queue, ctx.clone());
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                q.lock().unwrap_or_else(|p| p.into_inner()).push(Action::Toggle);
                c.request_repaint();
            }
        }));
    }

    let (rgba, w, h) = icon::rgba(32);
    let tray_icon = tray_icon::Icon::from_rgba(rgba, w, h).ok()?;
    let built = TrayIconBuilder::new()
        .with_icon(tray_icon)
        .with_tooltip("clipguard")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build();
    match built {
        Ok(icon) => Some(Tray { _icon: icon, pause, autostart: start }),
        Err(e) => {
            eprintln!("[tray] unavailable: {e}");
            None
        }
    }
}
