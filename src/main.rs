mod crypto;
mod markers;
mod secrets;
mod storage;
mod watcher;

use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use storage::{Insert, Store, DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS};
use watcher::Event;

/// Secrets live this long after their last copy: removed from DB, and wiped from the
/// system clipboard if they are still on it.
const SECRET_TTL_SECS: i64 = 120;
const TICK: Duration = Duration::from_secs(1);

fn fail(msg: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("[clipguard] {msg}: {e}");
    std::process::exit(1);
}

/// Secrets currently waiting for their clipboard wipe.
struct Pending {
    fingerprint: [u8; 32],
    deadline: Instant,
}

fn main() {
    // No key store -> refuse to run. Never fall back to a plaintext key file.
    let cipher = crypto::Cipher::from_os_keystore().unwrap_or_else(|e| fail("cannot load key", e));
    let path = storage::default_db_path().unwrap_or_else(|| fail("no data dir", "unknown OS layout"));
    let store = Store::open(&path, cipher).unwrap_or_else(|e| fail("cannot open db", e));
    let mut clipboard = arboard::Clipboard::new().unwrap_or_else(|e| fail("no clipboard", e));

    let purged = store.purge_secrets(SECRET_TTL_SECS).unwrap_or(0)
        + store.purge(DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS).unwrap_or(0);
    let (items, bad) = store.load_all().unwrap_or_else(|e| fail("cannot load db", e));
    println!(
        "[clipguard] db={}  items={}  purged={}  undecryptable={}",
        path.display(), items.len(), purged, bad
    );
    drop(items); // zeroized on drop

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        if let Err(e) = watcher::run(tx) {
            fail("watcher failed", e);
        }
    });

    println!("[clipguard] listening… (Ctrl+C to quit)");
    let mut pending: Vec<Pending> = Vec::new();

    loop {
        match rx.recv_timeout(TICK) {
            Ok(Event::Skipped(why)) => println!("[skip] password manager marker: {why} (not stored)"),
            Ok(Event::Clip(text)) => {
                let kind = secrets::detect(&text);
                match (store.insert(&text, kind.is_some()), kind) {
                    (Ok(ins), Some(kind)) => {
                        let fp = store.fingerprint(&text);
                        pending.retain(|p| p.fingerprint != fp); // re-copy restarts the timer
                        pending.push(Pending {
                            fingerprint: fp,
                            deadline: Instant::now() + Duration::from_secs(SECRET_TTL_SECS as u64),
                        });
                        let id = match ins { Insert::New(id) | Insert::Duplicate(id) => id };
                        println!("[secret] {kind}  id={id}  expires in {SECRET_TTL_SECS}s");
                    }
                    (Ok(Insert::New(id)), None) => println!("[clip] stored   id={id} len={}", text.chars().count()),
                    (Ok(Insert::Duplicate(id)), None) => println!("[clip] repeated id={id} (moved to top)"),
                    (Err(e), _) => eprintln!("[clip] store error: {e}"),
                }
                let _ = store.purge(DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // ---- tick: expire secrets
        let now = Instant::now();
        let (expired, alive): (Vec<_>, Vec<_>) = pending.drain(..).partition(|p| p.deadline <= now);
        pending = alive;
        if !expired.is_empty() {
            let removed = store.purge_secrets(SECRET_TTL_SECS).unwrap_or(0);
            // Wipe the system clipboard only if it still holds one of the expired secrets;
            // never clear something the user copied afterwards.
            let current = clipboard.get_text().ok().map(zeroize::Zeroizing::new);
            if let Some(cur) = current {
                let fp = store.fingerprint(&cur);
                if expired.iter().any(|p| p.fingerprint == fp) {
                    match clipboard.clear() {
                        Ok(()) => println!("[secret] expired: removed from db ({removed}) + clipboard wiped"),
                        Err(e) => eprintln!("[secret] clipboard wipe failed: {e}"),
                    }
                    continue;
                }
            }
            println!("[secret] expired: removed from db ({removed})");
        }
    }
    let _ = store.purge(DEFAULT_MAX_AGE_SECS, DEFAULT_MAX_ITEMS);
}
