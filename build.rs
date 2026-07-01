// Embed the application icon into the executable on Windows. This sets the
// Explorer/taskbar icon and makes the icon available as a resource (IDI_ICON1)
// that the tray code loads at runtime.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/goprocam.ico");
        let _ = res.compile();
    }
}
