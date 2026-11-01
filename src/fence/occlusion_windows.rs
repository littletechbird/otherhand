//! Which windows hide what is under them, for z-order aware redaction.

use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{GetWindowRgnBox, RGN_ERROR};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetWindowLongW, WS_EX_LAYERED, WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};

/// Whether nothing under this window can show through it. Anything that
/// might be see-through (layered, click-through, no redirection bitmap,
/// cloaked, shaped by a window region, tool windows and shell chrome such
/// as the taskbar, whose transparency effect shows the windows behind it)
/// is not an occluder, so it never takes redaction away.
pub(crate) fn is_occluder(hwnd: HWND, class: &str) -> bool {
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    if ex & (WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOREDIRECTIONBITMAP) != 0 {
        return false;
    }
    const SEE_THROUGH: [&str; 9] = [
        "Shell_TrayWnd",
        "Shell_SecondaryTrayWnd",
        "Progman",
        "WorkerW",
        "ApplicationFrameWindow",
        "Windows.UI.Core.CoreWindow",
        "#32768",
        "tooltips_class32",
        "ForegroundStaging",
    ];
    if SEE_THROUGH.iter().any(|c| class.eq_ignore_ascii_case(c)) {
        return false;
    }
    let mut cloaked: u32 = 0;
    let hr = unsafe {
        DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&mut cloaked as *mut u32).cast(), std::mem::size_of::<u32>() as u32)
    };
    if hr != 0 || cloaked != 0 {
        return false;
    }
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetWindowRgnBox(hwnd, &mut r) == RGN_ERROR }
}
