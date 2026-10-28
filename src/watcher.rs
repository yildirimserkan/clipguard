//! Clipboard watcher — cross-platform core.
//!
//! Event source: `clipboard-master`
//!   - Windows: AddClipboardFormatListener (native event)
//!   - macOS:   NSPasteboard changeCount polling
//!   - Linux:   X11 XFixes events (Wayland: limited, see README)
//! Content read: `arboard` (text only for now).
//!
//! Order per change: 1) password-manager marker check  2) read text  3) emit.
//! Marked content is never read into our process memory as a clip.

use crate::markers::{self, Marker};
use arboard::Clipboard;
use clipboard_master::{CallbackResult, ClipboardHandler, Master};
use sha2::{Digest, Sha256};
use std::io;
use std::sync::mpsc::Sender;
use zeroize::Zeroizing;

pub enum Event {
    Clip(Zeroizing<String>),
    Skipped(&'static str), // reason (marker name)
}

struct Handler {
    clipboard: Clipboard,
    last_hash: Option<[u8; 32]>, // in-memory only, suppresses duplicate OS events
    tx: Sender<Event>,
}

impl Handler {
    fn emit(&self, ev: Event) -> CallbackResult {
        if self.tx.send(ev).is_err() { CallbackResult::Stop } else { CallbackResult::Next }
    }
}

impl ClipboardHandler for Handler {
    fn on_clipboard_change(&mut self) -> CallbackResult {
        if let Marker::Excluded(why) = markers::check() {
            self.last_hash = None;
            return self.emit(Event::Skipped(why));
        }

        let text = match self.clipboard.get_text() {
            Ok(t) if !t.trim().is_empty() => Zeroizing::new(t),
            // Empty / non-text (e.g. after we clear an expired secret): reset dedup so the
            // same secret copied again is seen again and gets a fresh 120 s timer.
            _ => {
                self.last_hash = None;
                return CallbackResult::Next;
            }
        };

        let hash: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        if self.last_hash == Some(hash) {
            return CallbackResult::Next;
        }
        self.last_hash = Some(hash);
        self.emit(Event::Clip(text))
    }

    fn on_clipboard_error(&mut self, err: io::Error) -> CallbackResult {
        eprintln!("[watcher] error: {err}");
        CallbackResult::Next
    }
}

/// Blocks the calling thread and sends every clipboard event to `tx`.
pub fn run(tx: Sender<Event>) -> io::Result<()> {
    let clipboard = Clipboard::new().map_err(|e| io::Error::other(e.to_string()))?;
    let handler = Handler { clipboard, last_hash: None, tx };
    Master::new(handler)?.run()
}
