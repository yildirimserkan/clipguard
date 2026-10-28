//! App core: owns the store, the in-memory (decrypted) view and the secret timers.
//! Shared between the clipboard thread and the UI behind one Mutex.

use crate::secrets;
use crate::storage::{Store, DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS};
use crate::watcher::Event;
use crate::crypto;
use eframe::egui;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// Secrets live this long after their last copy: removed from DB, and wiped from the
/// system clipboard if they are still on it.
pub const SECRET_TTL_SECS: i64 = 120;
const TICK: Duration = Duration::from_millis(500);

pub struct ItemView {
    pub id: i64,
    pub text: Zeroizing<String>,
    pub pinned: bool,
    pub secret: Option<&'static str>,
    pub last_used_at: i64, // unix ms
}

struct Pending {
    fingerprint: [u8; 32],
    deadline: Instant,
}

pub struct Core {
    store: Store,
    clipboard: arboard::Clipboard,
    pending: Vec<Pending>,
    pub items: Vec<ItemView>,
    /// Bumped on every change so the UI can cache its filtered list.
    pub version: u64,
}

pub struct Startup {
    pub db: String,
    pub purged: usize,
    pub undecryptable: usize,
}

impl Core {
    pub fn open() -> Result<(Self, Startup), String> {
        // No key store -> refuse to run. Never fall back to a plaintext key file.
        let cipher = crypto::Cipher::from_os_keystore().map_err(|e| format!("cannot load key: {e}"))?;
        let path = crate::storage::default_db_path().ok_or("no data dir for this OS")?;
        let store = Store::open(&path, cipher).map_err(|e| format!("cannot open db: {e}"))?;
        let clipboard = arboard::Clipboard::new().map_err(|e| format!("no clipboard: {e}"))?;

        let purged = store.purge_secrets(SECRET_TTL_SECS).unwrap_or(0)
            + store.purge(DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS).unwrap_or(0);
        let mut core = Core { store, clipboard, pending: Vec::new(), items: Vec::new(), version: 0 };
        let undecryptable = core.refresh();
        Ok((core, Startup { db: path.display().to_string(), purged, undecryptable }))
    }

    /// Reload the decrypted view from the DB. Returns undecryptable row count.
    fn refresh(&mut self) -> usize {
        match self.store.load_all() {
            Ok((items, bad)) => {
                self.items = items
                    .into_iter()
                    .map(|i| ItemView {
                        secret: if i.is_secret { secrets::detect(&i.text).or(Some("secret")) } else { None },
                        id: i.id,
                        text: i.text,
                        pinned: i.pinned,
                        last_used_at: i.last_used_at,
                    })
                    .collect();
                self.version += 1;
                bad
            }
            Err(e) => {
                eprintln!("[core] load failed: {e}");
                0
            }
        }
    }

    pub fn on_event(&mut self, ev: Event) {
        match ev {
            Event::Skipped(why) => println!("[skip] password manager marker: {why} (not stored)"),
            Event::Clip(text) => {
                let kind = secrets::detect(&text);
                if let Err(e) = self.store.insert(&text, kind.is_some()) {
                    eprintln!("[clip] store error: {e}");
                    return;
                }
                if let Some(kind) = kind {
                    let fp = self.store.fingerprint(&text);
                    self.pending.retain(|p| p.fingerprint != fp); // re-copy restarts the timer
                    self.pending.push(Pending {
                        fingerprint: fp,
                        deadline: Instant::now() + Duration::from_secs(SECRET_TTL_SECS as u64),
                    });
                    println!("[secret] {kind}: expires in {SECRET_TTL_SECS}s");
                }
                let _ = self.store.purge(DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS);
                self.refresh();
            }
        }
    }

    /// Expire secrets. Returns true if anything changed.
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let (expired, alive): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|p| p.deadline <= now);
        self.pending = alive;
        if expired.is_empty() {
            return false;
        }
        let _ = self.store.purge_secrets(SECRET_TTL_SECS);
        // Wipe the system clipboard only if it still holds one of the expired secrets;
        // never clear something the user copied afterwards.
        if let Ok(cur) = self.clipboard.get_text().map(Zeroizing::new) {
            let fp = self.store.fingerprint(&cur);
            if expired.iter().any(|p| p.fingerprint == fp) {
                if let Err(e) = self.clipboard.clear() {
                    eprintln!("[secret] clipboard wipe failed: {e}");
                }
            }
        }
        self.refresh();
        true
    }

    /// Put an item back on the system clipboard (the watcher then moves it to the top).
    pub fn copy(&mut self, id: i64) -> bool {
        let Some(item) = self.items.iter().find(|i| i.id == id) else { return false };
        let text: String = item.text.as_str().to_owned();
        match self.clipboard.set_text(text) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("[ui] copy failed: {e}");
                false
            }
        }
    }

    pub fn toggle_pin(&mut self, id: i64) {
        if let Some(i) = self.items.iter().find(|i| i.id == id) {
            let _ = self.store.set_pinned(id, !i.pinned);
            self.refresh();
        }
    }

    pub fn delete(&mut self, id: i64) {
        let _ = self.store.delete(id);
        self.refresh();
    }
}

/// Clipboard thread: feeds watcher events into the core and runs the secret timer.
pub fn spawn_loop(core: Arc<Mutex<Core>>, rx: Receiver<Event>, ui: Arc<OnceLock<egui::Context>>) {
    std::thread::spawn(move || loop {
        let ev = match rx.recv_timeout(TICK) {
            Ok(ev) => Some(ev),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let changed = {
            let mut c = core.lock().unwrap_or_else(|p| p.into_inner());
            let had_event = ev.is_some();
            if let Some(ev) = ev {
                c.on_event(ev);
            }
            c.tick() || had_event
        };
        if changed {
            if let Some(ctx) = ui.get() {
                ctx.request_repaint();
            }
        }
    });
}
