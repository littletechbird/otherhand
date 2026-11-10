//! The client's handshake with the glow helper.
//!
//! A focus-taking command sends `MSG_BEGIN` and waits until the helper has
//! drawn the strips and flushed them. A control window whose process image
//! is not this executable is ignored, so a stand-in cannot acknowledge the
//! warning.

use super::warn::{FrameAck, parse_ack};
use super::windows::{MSG_BEGIN, spawn_helper};
use super::{GlowConfig, HelperArgs, pack_config};
use std::path::PathBuf;
use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE, SMTO_ABORTIFHUNG, SMTO_BLOCK,
    SendMessageTimeoutW,
};

const ANSWER_MS: u32 = 2_000;
const WAIT_FOR_HELPER_MS: u64 = 3_000;

pub(super) fn client_warn(cfg: &GlowConfig) -> Result<FrameAck, String> {
    let packed = pack_config(cfg) as LPARAM;
    if let Some(ack) = send_begin(packed) {
        return Ok(ack);
    }
    let args = HelperArgs {
        first_pid: None,
        config: *cfg,
    };
    if !spawn_helper(&args) {
        return Err("the glow helper process did not start".into());
    }
    let steps = WAIT_FOR_HELPER_MS / 20;
    for _ in 0..steps {
        if let Some(ack) = send_begin(packed) {
            return Ok(ack);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("the glow helper did not show a visible frame in time".into())
}

fn send_begin(packed: LPARAM) -> Option<FrameAck> {
    let hwnd = trusted_control()?;
    let mut answer: usize = 0;
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd,
            MSG_BEGIN,
            GetCurrentProcessId() as WPARAM,
            packed,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            ANSWER_MS,
            &mut answer,
        )
    };
    if sent == 0 {
        return None;
    }
    parse_ack(answer)
}

fn trusted_control() -> Option<HWND> {
    let class = wide(super::CONTROL_CLASS);
    let mut after: HWND = std::ptr::null_mut();
    for _ in 0..32 {
        let hwnd = unsafe { FindWindowExW(HWND_MESSAGE, after, class.as_ptr(), std::ptr::null()) };
        if hwnd.is_null() {
            return None;
        }
        after = hwnd;
        if window_is_ours(hwnd) {
            return Some(hwnd);
        }
    }
    None
}

fn window_is_ours(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid == 0 {
        return false;
    }
    let Ok(ours) = std::env::current_exe() else {
        return false;
    };
    let Some(theirs) = process_image(pid) else {
        return false;
    };
    super::warn::same_image(&ours, &theirs)
}

fn process_image(pid: u32) -> Option<PathBuf> {
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return None;
    }
    let mut buffer = [0u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe { QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size) };
    unsafe { CloseHandle(handle) };
    if ok == 0 || size == 0 {
        return None;
    }
    let text = String::from_utf16_lossy(&buffer[..size as usize]);
    Some(PathBuf::from(text))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
