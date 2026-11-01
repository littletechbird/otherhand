//! Windows half of the focus guard: observation plus the shield that puts
//! the person's foreground window back while a headless command runs.
//!
//! One shield is live per process at a time; nested guards (batch entries
//! inside a batch) share it through a depth count. The hook thread owns a
//! message loop for an out-of-context EVENT_SYSTEM_FOREGROUND WinEvent hook
//! and two low-level input hooks. The input hooks only look at whether a
//! real (not injected) mouse button press or Alt / Win / Tab key press just
//! happened, so the person's own window switch is never fought. They keep
//! no keys, no positions and no text: one timestamp, nothing else.

use super::{Observation, ShieldReport, is_person_switch};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::SystemInformation::GetTickCount;
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows_sys::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, LASTINPUTINFO, VK_LMENU, VK_LWIN, VK_MENU, VK_RMENU, VK_RWIN, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetCursorPos, GetForegroundWindow,
    GetMessageW, GetWindowThreadProcessId, IsWindow, KBDLLHOOKSTRUCT, LLKHF_INJECTED,
    LLMHF_INJECTED, LSFW_LOCK, LSFW_UNLOCK, LockSetForegroundWindow, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetCursorPos, SetForegroundWindow,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MBUTTONDOWN,
    WM_NCLBUTTONDOWN, WM_NCRBUTTONDOWN, WM_QUIT, WM_RBUTTONDOWN, WM_SYSKEYDOWN, WM_XBUTTONDOWN,
};

/// Most restores one command may try, so a window that keeps grabbing the
/// foreground can't turn the shield into a tug of war.
const MAX_RESTORES: u32 = 40;
/// Gap between restore attempts from the sampler (the hook is not limited).
const SAMPLER_GAP_US: u64 = 4_000;

static TARGET: AtomicIsize = AtomicIsize::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static LOCK_HELD: AtomicBool = AtomicBool::new(false);
static PERSON_SWITCH: AtomicBool = AtomicBool::new(false);
/// GetTickCount of the last real click or switch key, plus one (0 = none).
static LAST_REAL_SWITCH: AtomicU64 = AtomicU64::new(0);
static LAST_SAMPLER_RESTORE: AtomicU64 = AtomicU64::new(0);
static RESTORE_SERIAL: Mutex<()> = Mutex::new(());

struct Live {
    depth: u32,
    thread_id: u32,
    join: Option<std::thread::JoinHandle<()>>,
    hooked: bool,
}

static LIVE: Mutex<Option<Live>> = Mutex::new(None);

pub(super) fn observe() -> Option<Observation> {
    let mut p = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut p) } == 0 {
        return None;
    }
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    let tick = if unsafe { GetLastInputInfo(&mut info) } != 0 { info.dwTime } else { unsafe { GetTickCount() } };
    Some(Observation { foreground: unsafe { GetForegroundWindow() } as isize, cursor: (p.x, p.y), last_input_tick: tick })
}

pub(super) fn set_cursor(at: (i32, i32)) -> bool {
    unsafe { SetCursorPos(at.0, at.1) != 0 }
}

fn person_switched_recently() -> bool {
    let raw = LAST_REAL_SWITCH.load(Ordering::SeqCst);
    let last = (raw != 0).then(|| (raw - 1) as u32);
    is_person_switch(unsafe { GetTickCount() }, last)
}

/// Puts `target` back in front. Attaching to the thread that holds the
/// foreground lets SetForegroundWindow through without sending any input.
fn restore(target: isize) -> bool {
    if !ACTIVE.load(Ordering::SeqCst) || target == 0 {
        return false;
    }
    if person_switched_recently() {
        PERSON_SWITCH.store(true, Ordering::SeqCst);
        return false;
    }
    let _serial = RESTORE_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fg = unsafe { GetForegroundWindow() };
    if fg as isize == target {
        return true;
    }
    let t = target as HWND;
    if unsafe { IsWindow(t) } == 0 {
        return false;
    }
    if ATTEMPTS.fetch_add(1, Ordering::SeqCst) >= MAX_RESTORES {
        return false;
    }
    let locked = LOCK_HELD.load(Ordering::SeqCst);
    if locked {
        unsafe { LockSetForegroundWindow(LSFW_UNLOCK) };
    }
    let fg_thread = if fg.is_null() { 0 } else { unsafe { GetWindowThreadProcessId(fg, std::ptr::null_mut()) } };
    let me = unsafe { GetCurrentThreadId() };
    let attached = fg_thread != 0 && fg_thread != me && unsafe { AttachThreadInput(me, fg_thread, 1) } != 0;
    unsafe { SetForegroundWindow(t) };
    if attached {
        unsafe { AttachThreadInput(me, fg_thread, 0) };
    }
    if locked {
        unsafe { LockSetForegroundWindow(LSFW_LOCK) };
    }
    unsafe { GetForegroundWindow() as isize == target }
}

pub(super) fn restore_from_sampler(target: isize) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0);
    let last = LAST_SAMPLER_RESTORE.load(Ordering::SeqCst);
    if now.saturating_sub(last) < SAMPLER_GAP_US {
        return;
    }
    LAST_SAMPLER_RESTORE.store(now, Ordering::SeqCst);
    let _ = restore(target);
}

unsafe extern "system" fn on_foreground(_h: HWINEVENTHOOK, event: u32, hwnd: HWND, _o: i32, _c: i32, _t: u32, _ms: u32) {
    if event != EVENT_SYSTEM_FOREGROUND {
        return;
    }
    let target = TARGET.load(Ordering::SeqCst);
    if hwnd as isize != target {
        let _ = restore(target);
    }
}

fn note_real_switch() {
    LAST_REAL_SWITCH.store(u64::from(unsafe { GetTickCount() }) + 1, Ordering::SeqCst);
}

unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam != 0 {
        let info = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let press = matches!(
            wparam as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN
        );
        if press && info.flags & LLMHF_INJECTED == 0 {
            note_real_switch();
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && lparam != 0 {
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let vk = info.vkCode as u16;
        let switch_key = matches!(vk, VK_MENU | VK_LMENU | VK_RMENU | VK_LWIN | VK_RWIN | VK_TAB);
        if down && switch_key && info.flags & LLKHF_INJECTED == 0 {
            note_real_switch();
        }
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

/// Owns the hooks. Peeks once first so the thread has a message queue
/// before anyone posts WM_QUIT to it.
fn hook_thread(ready: std::sync::mpsc::Sender<(u32, bool)>) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let fg_hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            std::ptr::null_mut(),
            Some(on_foreground),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    let mouse = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), module, 0) };
    let keys = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), module, 0) };
    let _ = ready.send((unsafe { GetCurrentThreadId() }, !fg_hook.is_null()));
    loop {
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 || msg.message == WM_QUIT {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    unsafe {
        if !fg_hook.is_null() {
            UnhookWinEvent(fg_hook);
        }
        if !mouse.is_null() {
            UnhookWindowsHookEx(mouse);
        }
        if !keys.is_null() {
            UnhookWindowsHookEx(keys);
        }
    }
}

pub(super) struct Shield {
    target: isize,
    attempts_at_raise: u32,
}

impl Shield {
    /// Starts restoring `target`. Also tries LockSetForegroundWindow, which
    /// only the foreground process may use; a background CLI usually can't,
    /// and the shield works either way.
    pub(super) fn raise(target: isize) -> Self {
        let mut live = LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(l) = live.as_mut() {
            l.depth += 1;
            return Self { target: TARGET.load(Ordering::SeqCst), attempts_at_raise: ATTEMPTS.load(Ordering::SeqCst) };
        }
        TARGET.store(target, Ordering::SeqCst);
        ATTEMPTS.store(0, Ordering::SeqCst);
        PERSON_SWITCH.store(false, Ordering::SeqCst);
        LAST_SAMPLER_RESTORE.store(0, Ordering::SeqCst);
        LOCK_HELD.store(unsafe { LockSetForegroundWindow(LSFW_LOCK) } != 0, Ordering::SeqCst);
        ACTIVE.store(true, Ordering::SeqCst);
        let (tx, rx) = std::sync::mpsc::channel();
        let join = std::thread::Builder::new().name("focus-shield".into()).spawn(move || hook_thread(tx)).ok();
        let (thread_id, hooked) = rx.recv_timeout(std::time::Duration::from_millis(500)).unwrap_or((0, false));
        *live = Some(Live { depth: 1, thread_id, join, hooked });
        Self { target, attempts_at_raise: 0 }
    }

    pub(super) fn restore_now(&self) {
        let _ = restore(self.target);
    }

    pub(super) fn lower(self) -> ShieldReport {
        let mut live = LIVE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let report = ShieldReport {
            restore_attempts: ATTEMPTS.load(Ordering::SeqCst).saturating_sub(self.attempts_at_raise),
            person_switch: PERSON_SWITCH.load(Ordering::SeqCst),
            lock_held: LOCK_HELD.load(Ordering::SeqCst),
            hook_installed: live.as_ref().is_some_and(|l| l.hooked),
        };
        let Some(l) = live.as_mut() else {
            return report;
        };
        l.depth = l.depth.saturating_sub(1);
        if l.depth > 0 {
            return report;
        }
        ACTIVE.store(false, Ordering::SeqCst);
        if LOCK_HELD.swap(false, Ordering::SeqCst) {
            unsafe { LockSetForegroundWindow(LSFW_UNLOCK) };
        }
        if l.thread_id != 0 {
            unsafe { PostThreadMessageW(l.thread_id, WM_QUIT, 0, 0) };
        }
        if let Some(j) = l.join.take() {
            let _ = j.join();
        }
        *live = None;
        report
    }
}
