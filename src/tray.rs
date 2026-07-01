//! Minimal Win32 system-tray icon + popup menu. No external UI crates.

use crate::Control;
use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

static CTRL: OnceLock<Arc<Control>> = OnceLock::new();
/// HWND of the open preview window (0 = none), as a raw isize for atomic access.
static PREVIEW_HWND: AtomicIsize = AtomicIsize::new(0);
const PREVIEW_TIMER: usize = 1;

const WM_TRAYICON: u32 = WM_APP + 1;
const ID_SUSPEND: usize = 2;
const ID_STARTUP: usize = 3;
const ID_QUIT: usize = 4;
const ID_PREVIEW: usize = 5;

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

        // Preview window class (uses the app icon, black background).
        let big_icon = LoadImageW(hinst, PCWSTR(1 as *const u16), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE)
            .map(|h| HICON(h.0))
            .unwrap_or_default();
        let pwc = WNDCLASSW {
            lpfnWndProc: Some(preview_wndproc),
            hInstance: hinst,
            lpszClassName: w!("GoProCamPreview"),
            hIcon: big_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            ..Default::default()
        };
        RegisterClassW(&pwc);

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
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);

        // Message loop.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Remove the tray icon on exit.
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
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

    // Preview window.
    let _ = AppendMenuW(hmenu, MF_STRING, ID_PREVIEW, w!("Aperçu"));

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
        ID_PREVIEW => open_preview(),
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

/// Open (or focus) the preview window.
unsafe fn open_preview() {
    let existing = PREVIEW_HWND.load(Ordering::SeqCst);
    if existing != 0 {
        let h = HWND(existing as *mut c_void);
        let _ = ShowWindow(h, SW_RESTORE);
        let _ = SetForegroundWindow(h);
        return;
    }

    let hinst = HINSTANCE(GetModuleHandleW(None).unwrap().0);
    let hwnd = match CreateWindowExW(
        WINDOW_EX_STYLE(0),
        w!("GoProCamPreview"),
        w!("Aperçu"),
        WS_OVERLAPPEDWINDOW | WS_VISIBLE,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        660,
        400,
        HWND::default(),
        HMENU::default(),
        hinst,
        None,
    ) {
        Ok(h) => h,
        Err(_) => return,
    };

    PREVIEW_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    if let Some(c) = CTRL.get() {
        c.preview_on.store(true, Ordering::SeqCst);
    }
    // Repaint ~30x per second while open.
    let _ = SetTimer(hwnd, PREVIEW_TIMER, 33, None);
}

extern "system" fn preview_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                let _ = InvalidateRect(hwnd, None, false);
                LRESULT(0)
            }
            WM_PAINT => {
                paint_preview(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(hwnd, PREVIEW_TIMER);
                PREVIEW_HWND.store(0, Ordering::SeqCst);
                if let Some(c) = CTRL.get() {
                    c.preview_on.store(false, Ordering::SeqCst);
                    *c.preview.lock().unwrap() = None; // free the buffer
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Draw the latest frame, scaled to the client area, via GDI.
unsafe fn paint_preview(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = BeginPaint(hwnd, &mut ps);

    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (cw, ch) = (rc.right - rc.left, rc.bottom - rc.top);

    // Grab the latest frame quickly, then release the lock before converting.
    let frame = CTRL
        .get()
        .and_then(|c| c.preview.lock().unwrap().as_ref().map(|f| {
            (f.width, f.height, f.y.clone(), f.uv.clone())
        }));

    if let Some((w, h, y, uv)) = frame {
        let bgra = nv12_to_bgra(w as usize, h as usize, &y, &uv);
        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w as i32;
        bmi.bmiHeader.biHeight = -(h as i32); // top-down
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = 0; // BI_RGB
        SetStretchBltMode(hdc, COLORONCOLOR);
        StretchDIBits(
            hdc,
            0,
            0,
            cw,
            ch,
            0,
            0,
            w as i32,
            h as i32,
            Some(bgra.as_ptr() as *const c_void),
            &bmi,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }

    let _ = EndPaint(hwnd, &ps);
}

/// Convert tightly-packed NV12 to 32-bit BGRA (BT.601) for a top-down DIB.
fn nv12_to_bgra(w: usize, h: usize, y: &[u8], uv: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    for row in 0..h {
        let uv_row = (row / 2) * w;
        for col in 0..w {
            let yy = y[row * w + col] as i32;
            let uv_i = uv_row + (col & !1);
            let u = uv[uv_i] as i32;
            let v = uv[uv_i + 1] as i32;

            let c = yy - 16;
            let d = u - 128;
            let e = v - 128;
            let r = (298 * c + 409 * e + 128) >> 8;
            let g = (298 * c - 100 * d - 208 * e + 128) >> 8;
            let b = (298 * c + 516 * d + 128) >> 8;

            let o = (row * w + col) * 4;
            out[o] = b.clamp(0, 255) as u8;
            out[o + 1] = g.clamp(0, 255) as u8;
            out[o + 2] = r.clamp(0, 255) as u8;
            out[o + 3] = 255;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::nv12_to_bgra;

    // Build a solid-colour NV12 image (every pixel the same Y/U/V).
    fn solid(w: usize, h: usize, y: u8, u: u8, v: u8) -> (Vec<u8>, Vec<u8>) {
        let yp = vec![y; w * h];
        let mut uv = vec![0u8; w * h / 2];
        for pair in uv.chunks_mut(2) {
            pair[0] = u;
            pair[1] = v;
        }
        (yp, uv)
    }

    #[test]
    fn output_is_bgra_sized_and_opaque() {
        let (y, uv) = solid(4, 2, 128, 128, 128);
        let out = nv12_to_bgra(4, 2, &y, &uv);
        assert_eq!(out.len(), 4 * 2 * 4);
        assert!(out.chunks(4).all(|p| p[3] == 255), "alpha must be opaque");
    }

    #[test]
    fn black_and_white_map_correctly() {
        // BT.601 limited range: Y=16 -> black, Y=235 -> white, neutral chroma.
        let (y, uv) = solid(2, 2, 16, 128, 128);
        assert_eq!(&nv12_to_bgra(2, 2, &y, &uv)[0..4], &[0, 0, 0, 255]);

        let (y, uv) = solid(2, 2, 235, 128, 128);
        assert_eq!(&nv12_to_bgra(2, 2, &y, &uv)[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn red_uses_bgra_byte_order() {
        // BT.601 red (Y=81, U=90, V=240) must come out as B=0, G=0, R=255.
        let (y, uv) = solid(2, 2, 81, 90, 240);
        assert_eq!(&nv12_to_bgra(2, 2, &y, &uv)[0..4], &[0, 0, 255, 255]);
    }
}
