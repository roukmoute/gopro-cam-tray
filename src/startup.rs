//! Manage the "run at login" entry: a hidden-launch .vbs in the user's Startup
//! folder that starts THIS executable. No admin rights, no registry.

use std::path::PathBuf;

fn vbs_path() -> PathBuf {
    // %APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\gopro-cam-watch.vbs
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup")
        .join("gopro-cam-watch.vbs")
}

pub fn is_enabled() -> bool {
    vbs_path().exists()
}

/// Create the Startup launcher pointing at the current executable, run hidden.
pub fn enable() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy().replace('"', "\"\"");
    let vbs = format!(
        "' Auto-start the GoPro virtual-camera tray app, hidden, at login.\r\n\
         ' Delete this file (or use the tray menu) to disable auto-start.\r\n\
         Set sh = CreateObject(\"WScript.Shell\")\r\n\
         sh.Run \"\"\"{exe}\"\"\", 0, False\r\n"
    );
    std::fs::write(vbs_path(), vbs)
}

pub fn disable() -> std::io::Result<()> {
    let p = vbs_path();
    if p.exists() {
        std::fs::remove_file(p)?;
    }
    Ok(())
}
