//! Windows side of the glow: the command's client half and the helper
//! process that owns the frame windows.
//!
//! Client: find the helper's message-only control window and post it
//! "begin" or "end" with this process id. With no helper running, start one
//! (no inherited handles, no console, detached) that begins already holding
//! for this process, so nothing waits on it.
//!
//! Helper: one per desktop session (a named mutex). It keeps the frame up
//! while any holder lives plus the linger time, pulses it on a 40 ms timer,
//! and leaves after a minute with nothing to show. The frame strips are
//! `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE |
//! WS_EX_TOPMOST` popups, are shown only with `SWP_NOACTIVATE`, answer
//! `WM_MOUSEACTIVATE` with `MA_NOACTIVATE` and `WM_NCHITTEST` with
//! `HTTRANSPARENT`, and are excluded from capture with
//! `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`. A strip that can't be
//! excluded is still shown (unless exclusion is required), and the helper
//! hides it whenever an Otherhand command asks before a capture; the command
//! blacks out anything still showing. See the module notes in `mod.rs`.
//!
//! All helper state lives in one thread-local `Helper`, driven from the
//! control window's procedure, because a capture's "hide" arrives as a sent
//! message and has to be answered before the command takes its picture.

use super::{
    CONTROL_CLASS, CaptureHides, CaptureMode, FRAME_CLASS, GlowConfig, HelperArgs, Holders, Rect, capture_mode,
    keep_unexcluded_strip, pack_config, pulse_alpha, rebuild_due, render_strip, scaled_thickness, strips, unix_ms,
    unpack_config, write_note,
};
use serde_json::json;
use std::cell::RefCell;
use std::process::ExitCode;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::{DwmFlush, DwmIsCompositionEnabled};
use windows_sys::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, CreateCompatibleDC,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetMonitorInfoW, HBITMAP,
    HDC, HGDIOBJ, HMONITOR, MONITORINFO, ReleaseDC, SelectObject,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CreateMutexW, CreateProcessW, DETACHED_PROCESS, GetCurrentProcessId,
    GetExitCodeProcess, OpenProcess, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTF_FORCEOFFFEEDBACK,
    STARTUPINFOW,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW, GetMessageW, GetSystemMetrics,
    GetClassNameW, GetForegroundWindow, GetWindowDisplayAffinity, GetWindowRect, GetWindowThreadProcessId, HTTRANSPARENT, HWND_MESSAGE, HWND_TOPMOST, IsWindowVisible,
    MA_NOACTIVATE, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SM_REMOTESESSION, SMTO_ABORTIFHUNG,
    SMTO_BLOCK, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SendMessageTimeoutW,
    SetTimer, SetWindowDisplayAffinity, SetWindowPos, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow,
    WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const MSG_BEGIN: u32 = WM_APP + 0x4F1;
const MSG_END: u32 = WM_APP + 0x4F2;
/// Sent (not posted) by a command about to capture; answered once the frame
/// is off the screen.
const MSG_HIDE: u32 = WM_APP + 0x4F3;
/// Posted by that command when its capture is done.
const MSG_SHOW: u32 = WM_APP + 0x4F4;
/// Answers to `MSG_HIDE`.
const HIDE_NOTHING: usize = 1;
const HIDE_DONE: usize = 2;
const HIDE_ANSWER_MS: u32 = 400;
const STATUS_NOTE: &str = "glow status.json";
const SPAWN_NOTE: &str = "glow spawn.json";
const TICK_MS: u32 = 40;
const PRUNE_EVERY_MS: u64 = 250;
const RAISE_EVERY_MS: u64 = 500;
const MUTEX_NAME: &str = "Local\\OtherhandGlowHelper";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn control_window() -> HWND {
    let class = wide(CONTROL_CLASS);
    unsafe { FindWindowExW(HWND_MESSAGE, std::ptr::null_mut(), class.as_ptr(), std::ptr::null()) }
}

/// The settings travel with every begin (see `pack_config`).
fn config_param(cfg: &GlowConfig) -> LPARAM {
    pack_config(cfg) as LPARAM
}

pub(super) fn client_begin(cfg: &GlowConfig) {
    let pid = unsafe { GetCurrentProcessId() };
    let ctl = control_window();
    if !ctl.is_null() && unsafe { PostMessageW(ctl, MSG_BEGIN, pid as WPARAM, config_param(cfg)) } != 0 {
        return;
    }
    spawn_helper(&HelperArgs { first_pid: Some(pid), config: *cfg });
}

pub(super) fn client_end() {
    let pid = unsafe { GetCurrentProcessId() };
    let ctl = control_window();
    if !ctl.is_null() {
        unsafe { PostMessageW(ctl, MSG_END, pid as WPARAM, 0) };
    }
    // No helper yet (still starting): it holds for this pid and lets go when
    // the pid check sees this process gone.
}

/// Before a capture: asks the helper to take down a frame that isn't
/// excluded from capture, then lists every strip still on screen and not
/// excluded, which the caller blacks out. Returns whether a hide was asked
/// for (so the frame must be shown again after).
pub(super) fn client_capture_begin() -> (bool, Vec<Rect>) {
    let mut ctl = control_window();
    if ctl.is_null() && SPAWNED.load(Ordering::SeqCst) {
        // This command just started the helper: give it a moment to come up,
        // so it can't put a frame up in the middle of the capture.
        for _ in 0..25 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            ctl = control_window();
            if !ctl.is_null() {
                break;
            }
        }
    }
    if ctl.is_null() {
        // No helper: anything still showing is checked below.
        return (false, leftover_strips());
    }
    let pid = unsafe { GetCurrentProcessId() };
    let mut answer: usize = 0;
    let sent = unsafe {
        SendMessageTimeoutW(ctl, MSG_HIDE, pid as WPARAM, 0, SMTO_ABORTIFHUNG | SMTO_BLOCK, HIDE_ANSWER_MS, &mut answer)
    };
    if sent != 0 && answer == HIDE_DONE {
        // The helper flushed already; one more frame makes sure this
        // process's capture sees the screen without the strips.
        unsafe { DwmFlush() };
    }
    // A show after a hide that timed out or found the helper busy is
    // harmless, so only a clear "nothing to hide" skips it.
    let must_show = !(sent != 0 && answer == HIDE_NOTHING);
    (must_show, leftover_strips())
}

pub(super) fn client_capture_end() {
    let ctl = control_window();
    if !ctl.is_null() {
        unsafe { PostMessageW(ctl, MSG_SHOW, GetCurrentProcessId() as WPARAM, 0) };
    }
}

/// Every visible frame strip, from any process, that Windows would put in a
/// capture (exclusion not set, or not readable).
pub(super) fn leftover_strips() -> Vec<Rect> {
    let class = wide(FRAME_CLASS);
    let mut out = Vec::new();
    let mut after: HWND = std::ptr::null_mut();
    for _ in 0..256 {
        let h = unsafe { FindWindowExW(std::ptr::null_mut(), after, class.as_ptr(), std::ptr::null()) };
        if h.is_null() {
            break;
        }
        after = h;
        if unsafe { IsWindowVisible(h) } == 0 {
            continue;
        }
        let mut affinity = 0u32;
        if unsafe { GetWindowDisplayAffinity(h, &mut affinity) } != 0 && affinity == WDA_EXCLUDEFROMCAPTURE {
            continue;
        }
        let mut r: RECT = unsafe { std::mem::zeroed() };
        if unsafe { GetWindowRect(h, &mut r) } != 0 {
            out.push(Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top });
        }
    }
    out
}

/// Starts the helper with no inherited handles, so it can't hold the
/// caller's stdout open, and with no console of its own.
static SPAWNED: AtomicBool = AtomicBool::new(false);

fn spawn_helper(args: &HelperArgs) {
    let Ok(exe) = std::env::current_exe() else { return };
    let mut line = format!("\"{}\"", exe.display());
    for a in args.to_args() {
        line.push(' ');
        line.push_str(&a);
    }
    let mut line = wide(&line);
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    // No "app starting" cursor while the helper loads.
    startup.dwFlags = STARTF_FORCEOFFFEEDBACK;
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        CreateProcessW(
            std::ptr::null(),
            line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS | CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut info,
        )
    };
    let error = if ok == 0 { unsafe { GetLastError() } } else { 0 };
    SPAWNED.store(ok != 0, Ordering::SeqCst);
    if ok != 0 {
        unsafe {
            CloseHandle(info.hThread);
            CloseHandle(info.hProcess);
        }
    }
    write_note(
        SPAWN_NOTE,
        &json!({
            "at_unix_ms": unix_ms(),
            "client_pid": unsafe { GetCurrentProcessId() },
            "started": ok != 0,
            "helper_pid": if ok != 0 { Some(info.dwProcessId) } else { None },
            "create_process_error": error,
            "exe": exe.display().to_string(),
        }),
    );
}

fn process_alive(pid: u32) -> bool {
    let h: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if h.is_null() {
        return false;
    }
    let mut code = 0u32;
    let ok = unsafe { GetExitCodeProcess(h, &mut code) };
    unsafe { CloseHandle(h) };
    ok != 0 && code == 259
}

unsafe extern "system" fn frame_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        _ => unsafe { DefWindowProcW(hwnd, msg, w, l) },
    }
}

thread_local! {
    static HELPER: RefCell<Option<Helper>> = const { RefCell::new(None) };
}

unsafe extern "system" fn control_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if !matches!(msg, MSG_BEGIN | MSG_END | MSG_HIDE | MSG_SHOW | WM_TIMER) {
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    HELPER.with(|cell| {
        // Busy (a sent hide arriving while the helper is mid-update): say so,
        // and the command masks whatever is showing.
        let Ok(mut slot) = cell.try_borrow_mut() else { return 0 };
        let Some(helper) = slot.as_mut() else { return 0 };
        let t = helper.now();
        match msg {
            MSG_BEGIN => helper.on_begin(w as u32, l as u64),
            MSG_END => helper.holders.end(w as u32, t),
            MSG_HIDE => return helper.on_hide(w as u32, t) as LRESULT,
            MSG_SHOW => helper.hides.show(w as u32),
            _ => helper.on_tick(t),
        }
        helper.update(t);
        0
    })
}

static REGISTER: Once = Once::new();

fn register_classes() {
    REGISTER.call_once(|| {
        for (name, proc_) in [
            (FRAME_CLASS, frame_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            (CONTROL_CLASS, control_proc),
        ] {
            let class = wide(name);
            let mut wc: WNDCLASSEXW = unsafe { std::mem::zeroed() };
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(proc_);
            wc.hInstance = unsafe { GetModuleHandleW(std::ptr::null()) };
            wc.lpszClassName = class.as_ptr();
            unsafe { RegisterClassExW(&wc) };
        }
    });
}

/// One strip window and the pixels it shows.
struct Strip {
    hwnd: HWND,
    rect: Rect,
    excluded: bool,
    dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
}

impl Strip {
    fn create(rect: Rect, monitor: Rect, t: i32, cfg: &GlowConfig, errors: &mut Errors) -> Option<Self> {
        let color = cfg.color;
        let class = wide(FRAME_CLASS);
        let title = wide("Otherhand glow");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                rect.x,
                rect.y,
                rect.w,
                rect.h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            errors.create_window = unsafe { GetLastError() };
            return None;
        }
        // Excluded from capture before it is ever painted or shown.
        let excluded = unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) } != 0;
        if !excluded {
            errors.set_display_affinity = unsafe { GetLastError() };
            if !keep_unexcluded_strip(cfg) {
                unsafe { DestroyWindow(hwnd) };
                return None;
            }
        }
        let pixels = render_strip(rect, monitor, t, color);
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: rect.w,
            biHeight: -rect.h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..unsafe { std::mem::zeroed() }
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        let bitmap = unsafe { CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, std::ptr::null_mut(), 0) };
        if dc.is_null() || bitmap.is_null() || bits.is_null() {
            errors.dib = unsafe { GetLastError() }.max(1);
            unsafe {
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                if !dc.is_null() {
                    DeleteDC(dc);
                }
                DestroyWindow(hwnd);
            }
            return None;
        }
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len()) };
        let old = unsafe { SelectObject(dc, bitmap) };
        Some(Self { hwnd, rect, excluded, dc, bitmap, old })
    }

    fn present(&self, alpha: u8, errors: &mut Errors) -> bool {
        let screen = unsafe { GetDC(std::ptr::null_mut()) };
        let pos = POINT { x: self.rect.x, y: self.rect.y };
        let size = SIZE { cx: self.rect.w, cy: self.rect.h };
        let src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let ok = unsafe { UpdateLayeredWindow(self.hwnd, screen, &pos, &size, self.dc, &src, 0, &blend, ULW_ALPHA) };
        if ok == 0 {
            errors.update_layered_window = unsafe { GetLastError() }.max(1);
        }
        unsafe { ReleaseDC(std::ptr::null_mut(), screen) };
        ok != 0
    }

    /// Back to the top of the topmost band, shown or hidden, never activated.
    fn place(&self, shown: bool) {
        let show = if shown { SWP_SHOWWINDOW } else { SWP_HIDEWINDOW };
        unsafe { SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | show) };
    }

    fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }
}

impl Drop for Strip {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.hwnd);
            SelectObject(self.dc, self.old);
            DeleteObject(self.bitmap);
            DeleteDC(self.dc);
        }
    }
}

unsafe extern "system" fn collect_monitor(m: HMONITOR, _dc: HDC, _r: *mut RECT, data: LPARAM) -> windows_sys::core::BOOL {
    let out = unsafe { &mut *(data as *mut Vec<(Rect, u32)>) };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if unsafe { GetMonitorInfoW(m, &mut info) } != 0 {
        let r = info.rcMonitor;
        let (mut dx, mut dy) = (96u32, 96u32);
        if unsafe { GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) } != 0 {
            dx = 96;
        }
        out.push((Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }, dx));
    }
    1
}

fn monitors() -> Vec<(Rect, u32)> {
    let mut out: Vec<(Rect, u32)> = Vec::new();
    unsafe {
        EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(collect_monitor), &mut out as *mut _ as LPARAM)
    };
    out
}

/// Windows error codes from the last frame build (0 = fine).
#[derive(Debug, Default, Clone, Copy)]
struct Errors {
    create_window: u32,
    set_display_affinity: u32,
    dib: u32,
    update_layered_window: u32,
}

fn build_frame(cfg: &GlowConfig, errors: &mut Errors) -> (Vec<Strip>, usize, usize) {
    let mut out = Vec::new();
    let found = monitors();
    let mut wanted = 0;
    for (monitor, dpi) in &found {
        let t = scaled_thickness(cfg.thickness, *dpi);
        for rect in strips(*monitor, t) {
            if rect.w <= 0 || rect.h <= 0 {
                continue;
            }
            wanted += 1;
            if let Some(s) = Strip::create(rect, *monitor, t, cfg, errors) {
                out.push(s);
            }
        }
    }
    (out, found.len(), wanted)
}

/// Everything the helper keeps between messages.
struct Helper {
    started: Instant,
    cfg: GlowConfig,
    holders: Holders,
    hides: CaptureHides,
    frame: Vec<Strip>,
    shown: bool,
    last_build: Option<u64>,
    last_prune: u64,
    last_raise: u64,
    monitors: usize,
    wanted: usize,
    errors: Errors,
    hide_requests: u64,
    dirty: bool,
    last_note: String,
    /// Foreground changes seen while the helper runs (newest last), so a
    /// change between two commands, when no command's guard is watching,
    /// still has a record of where the foreground went and whether this
    /// helper had anything to do with it.
    last_fg: HWND,
    fg_seen: Vec<serde_json::Value>,
}

const FG_SEEN_KEEP: usize = 16;

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

impl Helper {
    fn watch_foreground(&mut self) {
        let fg = unsafe { GetForegroundWindow() };
        if fg == self.last_fg {
            return;
        }
        self.last_fg = fg;
        let mut pid = 0u32;
        if !fg.is_null() {
            unsafe { GetWindowThreadProcessId(fg, &mut pid) };
        }
        let own = pid != 0 && pid == unsafe { GetCurrentProcessId() };
        self.fg_seen.push(json!({
            "at_unix_ms": unix_ms(),
            "hwnd": fg as usize,
            "pid": pid,
            "class": if fg.is_null() { String::new() } else { class_of(fg) },
            "helper_window": own,
        }));
        if self.fg_seen.len() > FG_SEEN_KEEP {
            self.fg_seen.remove(0);
        }
        self.dirty = true;
    }

    fn now(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn on_begin(&mut self, pid: u32, packed: u64) {
        if let Some(next) = unpack_config(packed) {
            if next.color != self.cfg.color || next.thickness != self.cfg.thickness || next.strict != self.cfg.strict {
                self.frame.clear();
                self.shown = false;
                self.last_build = None;
            }
            self.holders.set_linger(next.linger_ms);
            self.cfg = next;
        }
        self.holders.begin(pid);
    }

    fn on_tick(&mut self, t: u64) {
        self.watch_foreground();
        if t.saturating_sub(self.last_prune) >= PRUNE_EVERY_MS {
            self.holders.prune(t, process_alive);
            self.last_prune = t;
        }
    }

    fn mode(&self) -> Option<CaptureMode> {
        capture_mode(self.frame.iter().filter(|s| s.excluded).count(), self.frame.len())
    }

    /// Takes a frame that would show in a capture off the screen and waits
    /// until the screen has been redrawn without it.
    fn on_hide(&mut self, pid: u32, t: u64) -> usize {
        self.hide_requests += 1;
        if self.mode() == Some(CaptureMode::Excluded) {
            return HIDE_NOTHING;
        }
        // Registered even with no frame yet, so one isn't built and shown in
        // the middle of the capture.
        self.hides.hide(pid, t);
        if self.shown {
            for s in &self.frame {
                s.place(false);
            }
            self.shown = false;
            unsafe { DwmFlush() };
        }
        self.dirty = true;
        HIDE_DONE
    }

    /// Puts the frame where it should be after any message.
    fn update(&mut self, t: u64) {
        let hidden_for_capture = self.hides.active(t, process_alive);
        if self.holders.visible(t) {
            if rebuild_due(self.frame.is_empty(), self.last_build, t) {
                let (frame, monitors, wanted) = build_frame(&self.cfg, &mut self.errors);
                self.frame = frame;
                self.monitors = monitors;
                self.wanted = wanted;
                self.last_build = Some(t);
                self.shown = false;
                self.dirty = true;
            }
            let alpha = pulse_alpha(t);
            let errors = &mut self.errors;
            let before = self.frame.len();
            self.frame.retain(|s| s.present(alpha, errors));
            if self.frame.len() != before {
                self.dirty = true;
            }
            if hidden_for_capture {
                return self.note(t);
            }
            if !self.shown || t.saturating_sub(self.last_raise) >= RAISE_EVERY_MS {
                // Re-raised inside the topmost band now and then, because the
                // shell's own chrome is topmost too. Never activates.
                for s in &self.frame {
                    s.place(true);
                }
                if !self.shown {
                    self.dirty = true;
                }
                self.shown = !self.frame.is_empty();
                self.last_raise = t;
            }
        } else if !self.frame.is_empty() {
            self.frame.clear();
            self.shown = false;
            self.last_build = None;
            self.dirty = true;
        } else if self.holders.should_exit(t) {
            self.write_status("exited");
            unsafe { PostQuitMessage(0) };
            return;
        }
        self.note(t);
    }

    fn note(&mut self, _t: u64) {
        if self.dirty {
            self.dirty = false;
            let state = if self.frame.is_empty() {
                if self.last_build.is_some() { "failed" } else { "idle" }
            } else if self.shown {
                "shown"
            } else {
                "hidden_for_capture"
            };
            self.write_status(state);
        }
    }

    fn write_status(&mut self, state: &str) {
        let doc = json!({
            "helper_pid": unsafe { GetCurrentProcessId() },
            "state": state,
            "mode": self.mode().map(CaptureMode::as_str),
            "monitors": self.monitors,
            "strips_wanted": self.wanted,
            "strips_made": self.frame.len(),
            "strips_excluded": self.frame.iter().filter(|s| s.excluded).count(),
            "strips_visible": self.frame.iter().filter(|s| s.visible()).count(),
            "require_capture_exclusion": self.cfg.strict,
            "color": super::format_color(self.cfg.color),
            "errors": {
                "create_window": self.errors.create_window,
                "set_display_affinity": self.errors.set_display_affinity,
                "dib": self.errors.dib,
                "update_layered_window": self.errors.update_layered_window,
            },
            "dwm_composition": composition_enabled(),
            "remote_session": unsafe { GetSystemMetrics(SM_REMOTESESSION) } != 0,
            "hide_requests": self.hide_requests,
            "foreground_seen": self.fg_seen,
            "up_ms": self.now(),
        });
        // Only the fields that matter, so the pulse doesn't rewrite the file.
        let key = format!("{state}|{}|{}|{}", doc["strips_made"], doc["strips_visible"], self.fg_seen.last().map(|v| v["at_unix_ms"].to_string()).unwrap_or_default());
        if key == self.last_note && state != "exited" {
            return;
        }
        self.last_note = key;
        let mut doc = doc;
        doc["at_unix_ms"] = json!(unix_ms());
        write_note(STATUS_NOTE, &doc);
    }
}

fn composition_enabled() -> bool {
    let mut on = 0;
    unsafe { DwmIsCompositionEnabled(&mut on) == 0 && on != 0 }
}

pub(super) fn run_helper(args: HelperArgs) -> ExitCode {
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    let name = wide(MUTEX_NAME);
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if mutex.is_null() {
        return ExitCode::FAILURE;
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        // Another helper is up or starting: hand it this pid and leave.
        unsafe { CloseHandle(mutex) };
        if let Some(pid) = args.first_pid {
            for _ in 0..50 {
                let ctl = control_window();
                if !ctl.is_null() {
                    unsafe { PostMessageW(ctl, MSG_BEGIN, pid as WPARAM, config_param(&args.config)) };
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
        return ExitCode::SUCCESS;
    }
    register_classes();
    let class = wide(CONTROL_CLASS);
    let ctl = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    if ctl.is_null() {
        unsafe { CloseHandle(mutex) };
        return ExitCode::FAILURE;
    }
    let mut holders = Holders::new(args.config.linger_ms, 0);
    if let Some(pid) = args.first_pid {
        holders.begin(pid);
    }
    let mut helper = Helper {
        started: Instant::now(),
        cfg: args.config,
        holders,
        hides: CaptureHides::default(),
        frame: Vec::new(),
        shown: false,
        last_build: None,
        last_prune: 0,
        last_raise: 0,
        monitors: 0,
        wanted: 0,
        errors: Errors::default(),
        hide_requests: 0,
        dirty: false,
        last_note: String::new(),
        last_fg: unsafe { GetForegroundWindow() },
        fg_seen: Vec::new(),
    };
    helper.write_status("starting");
    HELPER.with(|cell| *cell.borrow_mut() = Some(helper));
    unsafe { SetTimer(ctl, 1, TICK_MS, None) };
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    while unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) } > 0 {
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    HELPER.with(|cell| drop(cell.borrow_mut().take()));
    unsafe {
        DestroyWindow(ctl);
        CloseHandle(mutex);
    }
    ExitCode::SUCCESS
}
