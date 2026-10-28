// Dev helper: prints the current clipboard text.
fn main() {
    println!("{}", arboard::Clipboard::new().unwrap().get_text().unwrap_or_default());
}
