// Embed the application icon and version metadata into the executable on
// Windows. The icon becomes the Explorer/taskbar icon and a resource (IDI_ICON1)
// the tray loads at runtime. The metadata shows in the file's Properties >
// Details and as the program name in the UAC dialog. (It does NOT remove the
// SmartScreen "unknown publisher" warning — that requires code signing.)
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/goprocam.ico");
        res.set("CompanyName", "Mathias Strasser");
        res.set("ProductName", "GoPro Cam");
        res.set("FileDescription", "GoPro Cam (GoPro as a webcam)");
        res.set("LegalCopyright", "Copyright (c) 2026 Mathias Strasser");
        let _ = res.compile();
    }
}
