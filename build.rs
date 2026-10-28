// Windows only: embed the app icon and version info into clipguard.exe,
// so Explorer, the Start menu and Task Manager show the right icon/name.
fn main() {
    println!("cargo:rerun-if-changed=assets/clipguard.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/clipguard.ico")
            .set("ProductName", "clipguard")
            .set("FileDescription", "clipguard - secure clipboard manager");
        res.compile().expect("embed Windows resources");
    }
}
