//! Minimal Win32 system-tray icon + popup menu. No external UI crates.

use crate::Control;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

static CTRL: OnceLock<Arc<Control>> = OnceLock::new();

const WM_TRAYICON: u32 = WM_APP + 1;
const ID_SUSPEND: usize = 2;
const ID_STARTUP: usize = 3;
const ID_QUIT: usize = 4;

/// Create the tray icon and pump the message loop until the user quits.
pub fn run(ctrl: Arc<Control>) {
    let _ = CTRL.set(ctrl);
    unsafe {
        let hinst = HINSTANCE(GetModuleHandleW(None).unwrap().0);
        let class_name = w!("GoProCamTrayWnd");

        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: class_name,
            ..Default::default()
        };
        RegisterClassW(&wc);

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class_name,
            w!("GoPro Cam"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            HMENU::default(),
            hinst,
            None,
        )
        .unwrap();

        // Add the tray icon: our embedded app icon (resource id 1 from build.rs),
        // loaded at the small-icon size for a crisp tray render. Falls back to the
        // generic app icon if anything goes wrong.
        let cx = GetSystemMetrics(SM_CXSMICON);
        let cy = GetSystemMetrics(SM_CYSMICON);
        let hicon = match LoadImageW(
            hinst,
            PCWSTR(1 as *const u16), // MAKEINTRESOURCE(1)
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        ) {
            Ok(h) => HICON(h.0),
            Err(_) => LoadIconW(HINSTANCE::default(), IDI_APPLICATION).unwrap(),
        };
        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAYICON,
            hIcon: hicon,
            ..Default::default()
        };
        write_tip(&mut nid.szTip, "GoPro Cam");
        Shell_NotifyIconW(NIM_ADD, &nid);

        // Message loop.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Remove the tray icon on exit.
        Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn write_tip(dst: &mut [u16; 128], s: &str) {
    for (i, c) in s.encode_utf16().take(dst.len() - 1).enumerate() {
        dst[i] = c;
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAYICON => {
                let mouse = (lparam.0 as u32) & 0xFFFF;
                if mouse == WM_RBUTTONUP || mouse == WM_LBUTTONUP {
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn show_menu(hwnd: HWND) {
    let ctrl = match CTRL.get() {
        Some(c) => c,
        None => return,
    };

    let hmenu = CreatePopupMenu().unwrap();

    // Status header (disabled).
    let status = if ctrl.streaming.load(Ordering::SeqCst) {
        w!("GoPro : diffusion en cours")
    } else if ctrl.suspended.load(Ordering::SeqCst) {
        w!("GoPro : suspendu")
    } else {
        w!("GoPro : en attente")
    };
    let _ = AppendMenuW(hmenu, MF_STRING | MF_GRAYED, 1, status);
    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());

    // Suspend / resume.
    if ctrl.suspended.load(Ordering::SeqCst) {
        let _ = AppendMenuW(hmenu, MF_STRING, ID_SUSPEND, w!("Reprendre"));
    } else {
        let _ = AppendMenuW(
            hmenu,
            MF_STRING,
            ID_SUSPEND,
            w!("Suspendre (reprend au rebranchement)"),
        );
    }

    // Run at login (checkable).
    let mut startup_flags = MF_STRING;
    if crate::startup::is_enabled() {
        startup_flags |= MF_CHECKED;
    }
    let _ = AppendMenuW(hmenu, startup_flags, ID_STARTUP, w!("Lancer au démarrage"));

    let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(hmenu, MF_STRING, ID_QUIT, w!("Quitter"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);
    let _ = SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(
        hmenu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD,
        pt.x,
        pt.y,
        0,
        hwnd,
        None,
    );
    let _ = DestroyMenu(hmenu);

    match cmd.0 as usize {
        ID_SUSPEND => {
            let now = ctrl.suspended.load(Ordering::SeqCst);
            ctrl.suspended.store(!now, Ordering::SeqCst);
        }
        ID_STARTUP => {
            if crate::startup::is_enabled() {
                let _ = crate::startup::disable();
            } else {
                let _ = crate::startup::enable();
            }
        }
        ID_QUIT => {
            ctrl.quit.store(true, Ordering::SeqCst);
            let _ = DestroyWindow(hwnd);
        }
        _ => {}
    }
}
