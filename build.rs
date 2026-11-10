//! Embeds the app icon (a portrait of Osamu Dazai) into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=assets/dazai.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/dazai.ico");
        if let Err(e) = res.compile() {
            // Not fatal: the window still gets its icon at runtime.
            println!("cargo:warning=could not embed the exe icon: {e}");
        }
    }
}
