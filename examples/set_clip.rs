// Dev helper: puts text on the clipboard and keeps ownership for 2s (X11 needs a live owner).
//   set_clip "text"          -> normal copy
//   set_clip --pm "text"     -> copy the way a password manager does (marked "do not record")
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (pm, text) = match args.as_slice() {
        [flag, t] if flag == "--pm" => (true, t.clone()),
        [t] => (false, t.clone()),
        _ => { eprintln!("usage: set_clip [--pm] <text>"); std::process::exit(2) }
    };
    let mut cb = arboard::Clipboard::new().unwrap();
    if pm {
        #[cfg(all(unix, not(target_os = "macos")))]
        { use arboard::SetExtLinux; cb.set().exclude_from_history().text(text).unwrap(); }
        #[cfg(windows)]
        { use arboard::SetExtWindows; cb.set().exclude_from_monitoring().text(text).unwrap(); }
        #[cfg(target_os = "macos")]
        { use arboard::SetExtApple; cb.set().exclude_from_history().text(text).unwrap(); }
    } else {
        cb.set_text(text).unwrap();
    }
    let hold = std::env::var("CLIP_HOLD").ok().and_then(|s| s.parse().ok()).unwrap_or(2);
    std::thread::sleep(std::time::Duration::from_secs(hold));
}
