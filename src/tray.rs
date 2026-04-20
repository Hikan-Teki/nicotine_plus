// System tray (notification area) support.
//
// Owned by a dedicated thread that creates a message-only window and
// runs its own GetMessage loop; events are forwarded to the main
// (eframe) thread through an mpsc channel polled each frame from
// ConfigPanel::update. This isolation matters — eframe's winit loop
// doesn't dispatch to arbitrary wndprocs we register from elsewhere,
// so the tray window has to have its own pump.
//
// Left-click on the icon fires `TrayEvent::Show`; right-click pops a
// native context menu with "Göster" / "Çıkış" entries.

use anyhow::{anyhow, Context, Result};
use std::mem::size_of;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;
use std::thread;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, BringWindowToTop, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, IsIconic,
    LoadIconW, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    ShowWindow, TrackPopupMenu, TranslateMessage, HCURSOR, HICON, HMENU, HWND_MESSAGE,
    IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, LR_SHARED, MF_SEPARATOR, MF_STRING, MSG,
    SM_CXSMICON, SM_CYSMICON, SW_HIDE, SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_LBUTTONUP,
    WM_RBUTTONUP, WNDCLASSEXW, WS_OVERLAPPED,
};

/// Shell_NotifyIconW delivers tray-icon events to our wndproc with this
/// message id; the actual event kind (WM_LBUTTONUP, WM_RBUTTONUP, …) is
/// packed into the low word of lParam.
const WM_TRAYICON: u32 = WM_APP + 1;
/// Unique id for our tray entry — only relevant when multiple icons per
/// window exist (we only have one).
const TRAY_ICON_UID: u32 = 1;
/// Context-menu command ids. Must be non-zero (TrackPopupMenu returns 0
/// to signal "user dismissed without choosing").
const MENU_SHOW: usize = 1;
const MENU_EXIT: usize = 2;

#[derive(Debug, Clone, Copy)]
pub enum TrayEvent {
    /// User wants the main window restored (left-click or "Göster").
    Show,
    /// User picked "Çıkış" from the context menu — quit the app.
    Exit,
}

/// Set once when the first (and only) Tray is spawned. The wndproc is a
/// bare C callback with no user data, so it reaches back to us via this
/// global. We only ever spawn one tray per process lifetime.
static EVENT_TX: OnceLock<Sender<TrayEvent>> = OnceLock::new();
/// egui context handle, set alongside EVENT_TX so the tray thread can
/// wake the main eframe loop after posting an event. Without this the
/// main window stays idle when hidden and never drains the channel.
static EGUI_CTX: OnceLock<egui::Context> = OnceLock::new();
/// Raw HWND of the eframe/winit main window. Stored as `isize` so the
/// OnceLock stays Send/Sync. We drive show/hide directly via Win32
/// because winit's `request_redraw` doesn't fire `RedrawRequested` for
/// hidden windows on Windows (Windows suppresses WM_PAINT while hidden),
/// which means eframe never calls `update()` and can't see any
/// `ViewportCommand::Visible(true)` we might queue.
static MAIN_HWND: OnceLock<isize> = OnceLock::new();

/// Wire the main-window HWND in before spawning the tray. Safe to call
/// more than once — only the first value sticks, matching the single-
/// tray-per-process lifecycle.
pub fn set_main_hwnd(hwnd: isize) {
    let _ = MAIN_HWND.set(hwnd);
}

fn main_hwnd() -> Option<HWND> {
    MAIN_HWND.get().map(|h| HWND(*h as *mut _))
}

/// Live tray-icon handle. Dropping this posts WM_DESTROY to the tray
/// thread so it unregisters the icon and exits its message loop.
pub struct Tray {
    rx: Receiver<TrayEvent>,
    hwnd: HwndSend,
    _thread: thread::JoinHandle<()>,
}

/// HWND wrapper so `Tray` can be moved across threads safely. We only
/// ever use this handle from the owning ConfigPanel thread in Drop.
struct HwndSend(HWND);
unsafe impl Send for HwndSend {}
unsafe impl Sync for HwndSend {}

impl Tray {
    /// Non-blocking poll. Returns at most one pending event per call;
    /// callers loop in update() to drain the channel.
    pub fn try_recv(&self) -> Option<TrayEvent> {
        self.rx.try_recv().ok()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = PostMessageW(Some(self.hwnd.0), WM_DESTROY, WPARAM(0), LPARAM(0));
        }
    }
}

/// Create the tray icon. Blocks until the tray thread has finished
/// registering the icon (or failed). Idempotent across process lifetime
/// only in the sense that calling it twice returns an error — the
/// EVENT_TX OnceLock can't be re-initialized.
pub fn spawn(ctx: egui::Context) -> Result<Tray> {
    let (event_tx, event_rx) = channel::<TrayEvent>();
    EVENT_TX
        .set(event_tx)
        .map_err(|_| anyhow!("tray zaten oluşturulmuş"))?;
    let _ = EGUI_CTX.set(ctx);

    // Signal back once the icon is registered, so callers can count on
    // the tray being live before we return.
    let (ready_tx, ready_rx) = channel::<Result<usize, String>>();
    let handle = thread::spawn(move || unsafe {
        match register_tray() {
            Ok(hwnd) => {
                let _ = ready_tx.send(Ok(hwnd.0 as usize));
                run_message_loop();
                unregister_tray(hwnd);
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e.to_string()));
            }
        }
    });

    let hwnd_raw = ready_rx
        .recv()
        .context("tray hazır sinyali alınamadı")?
        .map_err(|e| anyhow!(e))?;
    Ok(Tray {
        rx: event_rx,
        hwnd: HwndSend(HWND(hwnd_raw as *mut _)),
        _thread: handle,
    })
}

unsafe fn register_tray() -> Result<HWND> {
    let instance = GetModuleHandleW(None)?;

    // Register the window class. Returns 0 on failure, but succeeds
    // harmlessly if the class is already registered (e.g. another Tray
    // was dropped earlier this process), which we want.
    let class_name = w!("InariTrayMsgWnd");
    let wnd_class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tray_wndproc),
        hInstance: instance.into(),
        lpszClassName: class_name,
        hCursor: HCURSOR::default(),
        hIcon: HICON::default(),
        hIconSm: HICON::default(),
        hbrBackground: Default::default(),
        lpszMenuName: PCWSTR::null(),
        style: Default::default(),
        cbClsExtra: 0,
        cbWndExtra: 0,
    };
    RegisterClassExW(&wnd_class);

    // HWND_MESSAGE: no-render, no-taskbar parent. The resulting window
    // only exists as a message target for Shell_NotifyIcon callbacks.
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class_name,
        w!("Inari Tray"),
        WS_OVERLAPPED,
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        Some(instance.into()),
        None,
    )
    .context("CreateWindowExW tray için başarısız")?;

    // Pull the embedded application icon (build.rs embeds it as resource
    // id 1). Small-icon dimensions keep the PNG from being downscaled
    // inside the shell notification area.
    // MAKEINTRESOURCEW(1): resource id 1 lives in the exe's ICON
    // section (see build.rs's `1 ICON "inari.ico"`). Win32 encodes
    // resource ids as a PCWSTR whose low word holds the id, so the
    // "pointer" is intentionally not a real address — hence the
    // allow below.
    #[allow(clippy::manual_dangling_ptr)]
    let icon_resource = PCWSTR(1usize as *const u16);
    let hicon = LoadImageW(
        Some(instance.into()),
        icon_resource,
        IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON),
        GetSystemMetrics(SM_CYSMICON),
        LR_DEFAULTCOLOR | LR_SHARED,
    )
    .map(|h| HICON(h.0))
    .unwrap_or_else(|_| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());

    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_UID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        hIcon: hicon,
        ..Default::default()
    };
    let tooltip: Vec<u16> = "Inari\0".encode_utf16().collect();
    let copy_len = tooltip.len().min(nid.szTip.len());
    nid.szTip[..copy_len].copy_from_slice(&tooltip[..copy_len]);

    if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
        let _ = DestroyWindow(hwnd);
        return Err(anyhow!("Shell_NotifyIconW(NIM_ADD) başarısız"));
    }

    Ok(hwnd)
}

unsafe fn unregister_tray(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_UID,
        ..Default::default()
    };
    let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    let _ = DestroyWindow(hwnd);
}

unsafe fn run_message_loop() {
    let mut msg = MSG::default();
    // GetMessage returns FALSE on WM_QUIT; our wndproc calls
    // PostQuitMessage on WM_DESTROY so dropping the Tray cleanly exits.
    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

extern "system" fn tray_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAYICON => {
                // Shell_NotifyIcon packs the event kind (WM_LBUTTONUP /
                // WM_RBUTTONUP / WM_MOUSEMOVE / …) into the low word of
                // lParam. High word is the icon id.
                let event = (lparam.0 as u32) & 0xFFFF;
                if event == WM_LBUTTONUP {
                    show_main_window();
                    send_event(TrayEvent::Show);
                } else if event == WM_RBUTTONUP {
                    show_context_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                // Context-menu commands arrive here when TrackPopupMenu
                // is NOT called with TPM_RETURNCMD. We use TPM_RETURNCMD
                // below, so this branch is a no-op — kept so any stray
                // synthesized commands are still handled.
                dispatch_menu_command(wparam.0 & 0xFFFF);
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

fn send_event(event: TrayEvent) {
    if let Some(tx) = EVENT_TX.get() {
        let _ = tx.send(event);
    }
    // Wake the eframe loop; when the main window is hidden it's idle
    // and would otherwise never poll the channel.
    if let Some(ctx) = EGUI_CTX.get() {
        ctx.request_repaint();
    }
}

fn dispatch_menu_command(id: usize) {
    match id {
        MENU_SHOW => {
            show_main_window();
            send_event(TrayEvent::Show);
        }
        MENU_EXIT => {
            // Surface the window first so eframe's winit loop gets a
            // WM_PAINT and actually runs `update()` to see the exit
            // request. A purely-hidden window would swallow the WM_CLOSE
            // repaint-less, and the app would linger.
            show_main_window();
            send_event(TrayEvent::Exit);
            if let Some(hwnd) = main_hwnd() {
                unsafe {
                    let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
        }
        _ => {}
    }
}

/// Force the main window visible and in the foreground. Uses the raw
/// HWND — eframe/winit can't drive this path on its own for a window
/// that's currently hidden.
fn show_main_window() {
    let Some(hwnd) = main_hwnd() else {
        return;
    };
    unsafe {
        // SW_RESTORE covers the minimized-then-hidden case; SW_SHOW
        // alone wouldn't un-minimize.
        let cmd = if IsIconic(hwnd).as_bool() {
            SW_RESTORE
        } else {
            SW_SHOW
        };
        let _ = ShowWindow(hwnd, cmd);
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
    }
}

/// Hide the main window at the Win32 level. Bypasses
/// `ViewportCommand::Visible(false)` because eframe's hidden state
/// prevents later show commands from being processed.
pub fn hide_main_window() {
    let Some(hwnd) = main_hwnd() else {
        return;
    };
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

unsafe fn show_context_menu(hwnd: HWND) {
    let menu: HMENU = match CreatePopupMenu() {
        Ok(m) => m,
        Err(_) => return,
    };
    // Labels intentionally Turkish — matches the rest of the Inari UI.
    let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, w!("Göster"));
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, w!("Çıkış"));

    let mut pt = POINT::default();
    let _ = GetCursorPos(&mut pt);

    // Classic tray-menu quirk: without SetForegroundWindow the popup
    // menu dismisses itself on the first outside click without letting
    // the user pick anything. Win32 documents this in the Shell sample.
    let _ = SetForegroundWindow(hwnd);

    let selected = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        Some(0),
        hwnd,
        None,
    );
    let _ = DestroyMenu(menu);

    // TPM_RETURNCMD: chosen command id returned directly; 0 means
    // "user dismissed." Dispatch through the same path WM_COMMAND uses.
    if selected.0 != 0 {
        dispatch_menu_command(selected.0 as usize);
    }
}
