use crate::config::{Config, DetectionMode};
use crate::window_manager::{EveWindow, WindowManager};
use anyhow::{Context, Result};
use std::ffi::c_void;
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, RwLock};
use windows::core::PWSTR;
use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, TRUE, WPARAM};
use windows::Win32::System::Threading::{
    AttachThreadInput, GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindowVisible, SendMessageW, SetForegroundWindow,
    SetWindowPos, ShowWindow, HWND_TOP, SC_MINIMIZE, SC_RESTORE, SWP_NOZORDER,
    SW_MINIMIZE, SW_RESTORE, WM_SYSCOMMAND,
};

// AtomicU8 encoding for DetectionMode — fits in one atomic load so the
// enum-scan callback can read it without locks.
const MODE_TITLE: u8 = 0;
const MODE_PROCESS: u8 = 1;

fn mode_to_u8(m: DetectionMode) -> u8 {
    match m {
        DetectionMode::Title => MODE_TITLE,
        DetectionMode::Process => MODE_PROCESS,
    }
}

fn mode_from_u8(v: u8) -> DetectionMode {
    match v {
        MODE_PROCESS => DetectionMode::Process,
        _ => DetectionMode::Title,
    }
}

/// Lowercase + ensure `.exe` suffix so callback-side comparisons are
/// case-insensitive regardless of what the user typed.
fn normalize_exe(name: &str) -> String {
    let trimmed = name.trim().to_ascii_lowercase();
    if trimmed.is_empty() || trimmed.ends_with(".exe") {
        trimmed
    } else {
        format!("{trimmed}.exe")
    }
}

fn normalize_extras(extras: Vec<String>) -> Vec<String> {
    extras
        .into_iter()
        .map(|e| normalize_exe(&e))
        .filter(|e| !e.is_empty())
        .collect()
}

/// Shared, hot-swappable detection settings. The daemon's config
/// hot-reload loop calls `update` when `config.toml` changes; the window
/// scan reads through Arcs so the next scan (≤500 ms later) picks up the
/// new mode without a restart.
#[derive(Clone)]
pub struct DetectionConfig {
    mode: Arc<AtomicU8>,
    extras: Arc<RwLock<Vec<String>>>,
}

impl DetectionConfig {
    pub fn new(mode: DetectionMode, extras: Vec<String>) -> Self {
        Self {
            mode: Arc::new(AtomicU8::new(mode_to_u8(mode))),
            extras: Arc::new(RwLock::new(normalize_extras(extras))),
        }
    }

    pub fn update(&self, mode: DetectionMode, extras: Vec<String>) {
        self.mode.store(mode_to_u8(mode), Ordering::Relaxed);
        if let Ok(mut guard) = self.extras.write() {
            *guard = normalize_extras(extras);
        }
    }

    pub fn current_mode(&self) -> DetectionMode {
        mode_from_u8(self.mode.load(Ordering::Relaxed))
    }

    fn extras_snapshot(&self) -> Vec<String> {
        self.extras.read().map(|g| g.clone()).unwrap_or_default()
    }
}

pub struct WindowsManager {
    detection: DetectionConfig,
}

impl WindowsManager {
    pub fn new(mode: DetectionMode, extras: Vec<String>) -> Result<Self> {
        Ok(Self {
            detection: DetectionConfig::new(mode, extras),
        })
    }

    /// Returns a handle used by the daemon to push config changes into
    /// the running scan. Cheap to clone — just bumps Arc refcounts.
    pub fn detection_config(&self) -> DetectionConfig {
        self.detection.clone()
    }
}

pub(crate) fn hwnd_to_id(hwnd: HWND) -> u32 {
    hwnd.0 as usize as u32
}

pub(crate) fn id_to_hwnd(id: u32) -> HWND {
    HWND(id as usize as *mut c_void)
}

fn read_window_title(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    // +1 for the null terminator that GetWindowTextW writes
    let mut buf: Vec<u16> = vec![0; len as usize + 1];
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if copied <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..copied as usize])
}

/// Returns the lowercased exe filename (e.g. `"exefile.exe"`) of the
/// process that owns `hwnd`. Uses `PROCESS_QUERY_LIMITED_INFORMATION`
/// which works even for protected / elevated peers where
/// `PROCESS_QUERY_INFORMATION` would be denied.
fn process_exe_filename(hwnd: HWND) -> Option<String> {
    unsafe {
        let mut pid: u32 = 0;
        let _ = GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return None;
        }
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf: Vec<u16> = vec![0u16; 520];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        Path::new(&path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_ascii_lowercase())
    }
}

/// State handed into `EnumWindows` via LPARAM. Carries the output vec
/// plus per-scan detection knobs so the callback doesn't need to touch
/// any shared state.
struct EnumState<'a> {
    out: &'a mut Vec<EveWindow>,
    mode: DetectionMode,
    allowed_exes: &'a [String], // used only in Process mode
}

unsafe extern "system" fn enum_collect_eve(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let state = &mut *(lparam.0 as *mut EnumState);

    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }

    let title = read_window_title(hwnd);
    if title.is_empty() {
        return TRUE;
    }

    let matches = match state.mode {
        DetectionMode::Title => {
            // Classic vanilla-launcher behavior: only `EVE - <name>`
            // windows, and filter out the launcher itself.
            title.starts_with("EVE - ") && !title.contains("Launcher")
        }
        DetectionMode::Process => {
            // ISBoxer / Inner Space rewrites titles, so match by the
            // actual exe. `exefile.exe` is always accepted; any user
            // additions are merged on top.
            match process_exe_filename(hwnd) {
                Some(name) => {
                    name == "exefile.exe"
                        || state.allowed_exes.iter().any(|e| e == &name)
                }
                None => false,
            }
        }
    };

    if !matches {
        return TRUE;
    }

    // Strip the vanilla `EVE - ` prefix for display when present —
    // works in both modes so ISBoxer users on standard EVE titles
    // still see clean character names. Anything else is shown raw
    // (ISBoxer Character Set names, custom window titles, etc.).
    let display = title
        .strip_prefix("EVE - ")
        .map(str::to_string)
        .unwrap_or(title);

    state.out.push(EveWindow {
        id: hwnd_to_id(hwnd),
        title: display,
    });

    TRUE
}

/// SetForegroundWindow is restricted on modern Windows — for reliable
/// focus stealing from another process, the standard pattern is to attach
/// our input queue to the target's. But that's slow. The fast path:
/// SetForegroundWindow works directly when our process "received the last
/// input event" — and a `RegisterHotKey` WM_HOTKEY counts. So we try the
/// cheap call first and only fall back to the AttachThreadInput dance if
/// Windows rejects it (typical when activation is triggered from the
/// passive low-level mouse hook, not a hotkey).
fn force_activate(target: HWND) {
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground == target {
            // Already focused — skip everything. Common case for repeated
            // hotkey presses against the active window.
            return;
        }

        // SW_RESTORE triggers the show animation, so only fire it when we
        // genuinely need to un-minimize.
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
        }

        // Fast path: try direct SetForegroundWindow first.
        if SetForegroundWindow(target).as_bool() {
            return;
        }

        // Fallback path: Windows refused the foreground change. Briefly
        // attach our thread's input queue to the target and current
        // foreground's queues so we look like the same input session.
        let target_thread = GetWindowThreadProcessId(target, None);
        let current_thread = GetCurrentThreadId();
        let foreground_thread = if foreground.0.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, None)
        };

        let attached_target = target_thread != 0
            && target_thread != current_thread
            && AttachThreadInput(current_thread, target_thread, true).as_bool();
        let attached_foreground = foreground_thread != 0
            && foreground_thread != current_thread
            && foreground_thread != target_thread
            && AttachThreadInput(current_thread, foreground_thread, true).as_bool();

        let _ = SetForegroundWindow(target);
        let _ = BringWindowToTop(target);
        let _ = SetFocus(Some(target));

        if attached_target {
            let _ = AttachThreadInput(current_thread, target_thread, false);
        }
        if attached_foreground {
            let _ = AttachThreadInput(current_thread, foreground_thread, false);
        }
    }
}

impl WindowManager for WindowsManager {
    fn get_eve_windows(&self) -> Result<Vec<EveWindow>> {
        let mut windows: Vec<EveWindow> = Vec::new();
        let extras = self.detection.extras_snapshot();
        let mut state = EnumState {
            out: &mut windows,
            mode: self.detection.current_mode(),
            allowed_exes: &extras,
        };
        unsafe {
            EnumWindows(
                Some(enum_collect_eve),
                LPARAM(&mut state as *mut _ as isize),
            )
            .context("EnumWindows failed")?;
        }
        Ok(windows)
    }

    fn activate_window(&self, window_id: u32) -> Result<()> {
        force_activate(id_to_hwnd(window_id));
        Ok(())
    }

    fn stack_windows(&self, windows: &[EveWindow], config: &Config) -> Result<()> {
        let x = ((config.display_width - config.eve_width) / 2) as i32;
        let y = 0;
        let width = config.eve_width as i32;
        let height = (config.display_height - config.panel_height) as i32;

        for window in windows {
            unsafe {
                SetWindowPos(
                    id_to_hwnd(window.id),
                    Some(HWND_TOP),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOZORDER,
                )
                .ok();
            }
        }
        Ok(())
    }

    fn get_active_window(&self) -> Result<u32> {
        let hwnd = unsafe { GetForegroundWindow() };
        Ok(hwnd_to_id(hwnd))
    }

    fn minimize_window(&self, window_id: u32) -> Result<()> {
        let hwnd = id_to_hwnd(window_id);
        unsafe {
            // SC_MINIMIZE via WM_SYSCOMMAND is friendlier to applications
            // than ShowWindow(SW_MINIMIZE) — it goes through the normal
            // window state machine.
            SendMessageW(
                hwnd,
                WM_SYSCOMMAND,
                Some(WPARAM(SC_MINIMIZE as usize)),
                Some(LPARAM(0)),
            );
        }
        let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        Ok(())
    }

    fn restore_window(&self, window_id: u32) -> Result<()> {
        let hwnd = id_to_hwnd(window_id);
        unsafe {
            SendMessageW(
                hwnd,
                WM_SYSCOMMAND,
                Some(WPARAM(SC_RESTORE as usize)),
                Some(LPARAM(0)),
            );
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        Ok(())
    }
}

// SAFETY: WindowsManager has no state and Win32 window APIs are thread-safe
// for the operations we perform.
unsafe impl Send for WindowsManager {}
unsafe impl Sync for WindowsManager {}
