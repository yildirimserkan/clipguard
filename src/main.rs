// Release builds on Windows: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod crypto;
mod engine;
mod icon;
mod markers;
mod paste;
mod secrets;
mod storage;
mod tray;
mod ui;
mod watcher;

use eframe::egui;
use std::sync::{mpsc, Arc, Mutex, OnceLock};

fn main() {
    let (core, info) = match engine::Core::open() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[clipguard] {e}");
            std::process::exit(1);
        }
    };
    println!(
        "[clipguard] db={}  items={}  purged={}  undecryptable={}",
        info.db, core.items.len(), info.purged, info.undecryptable
    );
    let core = Arc::new(Mutex::new(core));
    let ctx_slot: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        if let Err(e) = watcher::run(tx) {
            eprintln!("[clipguard] watcher failed: {e}");
            std::process::exit(1);
        }
    });
    engine::spawn_loop(core.clone(), rx, ctx_slot.clone());

    let start_hidden = std::env::args().any(|a| a == "--hidden");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("clipguard")
            .with_inner_size([920.0, 520.0])
            .with_min_inner_size([640.0, 280.0])
            .with_decorations(false)
            .with_always_on_top()
            .with_taskbar(false) // lives in the tray, like Ditto
            .with_icon({
                let (rgba, width, height) = icon::rgba(64);
                Arc::new(egui::IconData { rgba, width, height })
            })
            .with_visible(!start_hidden),
        centered: true,
        ..Default::default()
    };

    let res = eframe::run_native(
        "clipguard",
        options,
        Box::new(move |cc| {
            let mut app = ui::ClipApp::new(cc, core, ctx_slot);
            if start_hidden { app.mark_hidden(); }
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = res {
        eprintln!("[clipguard] ui failed: {e}");
        std::process::exit(1);
    }
}
