//! The frame is on screen before the first cursor input.
//!
//! This runs on Windows, where the helper can draw. A low-level mouse hook
//! records whether `OtherhandGlowFrame` windows were visible when the first
//! armed input event arrived.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, FindWindowExW, GetCursorPos, GetMessageW, IsWindowVisible,
    MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetCursorPos, SetWindowsHookExW,
    TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_QUIT,
};

use super::warn::{self, MIN_COUNTDOWN_MS};
use super::{FRAME_CLASS, prepare};

static ARMED: AtomicBool = AtomicBool::new(false);
static HITS: AtomicU32 = AtomicU32::new(0);
static VISIBLE_AT_HIT: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 && ARMED.load(Ordering::SeqCst) {
        VISIBLE_AT_HIT.store(frames_visible(), Ordering::SeqCst);
        HITS.fetch_add(1, Ordering::SeqCst);
    }
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
}

fn frames_visible() -> bool {
    let class: Vec<u16> = FRAME_CLASS
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let hwnd = unsafe {
        FindWindowExW(
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            class.as_ptr(),
            std::ptr::null(),
        )
    };
    !hwnd.is_null() && unsafe { IsWindowVisible(hwnd) } != 0
}

fn hook_thread(ready: std::sync::mpsc::Sender<u32>) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE) };
    let module = unsafe { GetModuleHandleW(std::ptr::null()) };
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(on_mouse), module, 0) };
    let _ = ready.send(unsafe { GetCurrentThreadId() });
    while !STOP.load(Ordering::SeqCst) {
        let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if got <= 0 || msg.message == WM_QUIT {
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    if !hook.is_null() {
        unsafe { UnhookWindowsHookEx(hook) };
    }
}

#[test]
fn the_glow_is_visible_before_the_first_input() {
    ARMED.store(false, Ordering::SeqCst);
    HITS.store(0, Ordering::SeqCst);
    VISIBLE_AT_HIT.store(false, Ordering::SeqCst);
    STOP.store(false, Ordering::SeqCst);
    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("glow-input-hook".into())
        .spawn(move || hook_thread(tx))
        .expect("hook thread");
    let hook_thread_id = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("hook installed");

    warn::script_live();
    let started = Instant::now();
    let prepared = prepare("click").expect("the warning must be shown before click runs");
    assert!(prepared.report.shown, "the frame was not confirmed visible");
    assert!(prepared.report.countdown_ms >= MIN_COUNTDOWN_MS);
    assert!(
        started.elapsed() >= Duration::from_millis(MIN_COUNTDOWN_MS),
        "input must wait out the countdown"
    );
    assert!(
        frames_visible(),
        "OtherhandGlowFrame was not visible after the countdown"
    );
    assert_eq!(
        HITS.load(Ordering::SeqCst),
        0,
        "input landed before the hook was armed"
    );

    let mut cursor = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut cursor) };
    ARMED.store(true, Ordering::SeqCst);
    unsafe { SetCursorPos(cursor.x.saturating_add(1), cursor.y) };
    let deadline = Instant::now() + Duration::from_secs(2);
    while HITS.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    unsafe { SetCursorPos(cursor.x, cursor.y) };
    ARMED.store(false, Ordering::SeqCst);
    warn::script_shown();
    drop(prepared);

    STOP.store(true, Ordering::SeqCst);
    unsafe { PostThreadMessageW(hook_thread_id, WM_QUIT, 0, 0) };
    let _ = thread.join();

    assert!(
        HITS.load(Ordering::SeqCst) >= 1,
        "the input hook never saw the cursor move"
    );
    assert!(
        VISIBLE_AT_HIT.load(Ordering::SeqCst),
        "the first input event ran while OtherhandGlowFrame was hidden"
    );
}
