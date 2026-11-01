//! Win32 facts for the fence: process image paths, top-level windows,
//! Explorer folders and fully resolved file paths.

use super::policy::{ProcessFacts, WindowFacts};
use super::probe::{ClipboardFacts, LiveWindow, Probe, ScreenRect};
use windows_sys::Win32::Foundation::{CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM, POINT, RECT};
use super::occlusion_windows::is_occluder;
use windows_sys::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetFinalPathNameByHandleW, OPEN_EXISTING,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, GA_ROOT, GetAncestor, GetClassNameW, GetForegroundWindow, GetSystemMetrics,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WindowFromPoint,
    GW_HWNDNEXT, GetTopWindow, GetWindow,
};

pub(crate) struct WinProbe {
    folders: std::sync::OnceLock<Vec<(isize, String)>>,
}

impl WinProbe {
    pub(crate) fn new() -> Self {
        Self { folders: std::sync::OnceLock::new() }
    }

    fn explorer_folders(&self) -> &[(isize, String)] {
        self.folders.get_or_init(explorer_folders)
    }

    fn live(&self, hwnd: HWND) -> Option<LiveWindow> {
        if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
            return None;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        let class = class_name(hwnd);
        // A store app's frame belongs to ApplicationFrameHost; the app itself
        // owns the CoreWindow inside it, so that process is the one judged.
        let pid = if class.eq_ignore_ascii_case("ApplicationFrameWindow") { core_window_pid(hwnd).unwrap_or(pid) } else { pid };
        let process = self.process(pid);
        let folder = if is_explorer_class(&class) {
            self.explorer_folders()
                .iter()
                .find(|(h, _)| *h == hwnd as isize)
                .map(|(_, f)| self.resolve_path(f))
        } else if class == "#32770" {
            dialog_folder(hwnd).map(|f| self.resolve_path(&f))
        } else {
            None
        };
        let occluder = is_occluder(hwnd, &class);
        Some(LiveWindow {
            id: format!("w-{}", hwnd as usize),
            pid,
            facts: WindowFacts { class, title: window_text(hwnd), process, folder },
            rect: frame_rect(hwnd),
            visible: unsafe { IsWindowVisible(hwnd) } != 0,
            minimized: unsafe { IsIconic(hwnd) } != 0,
            occluder,
        })
    }
}

use super::policy::is_explorer_class;

fn core_window_pid(frame: HWND) -> Option<u32> {
    let class = to_wide("Windows.UI.Core.CoreWindow");
    let child = unsafe { FindWindowExW(frame, std::ptr::null_mut(), class.as_ptr(), std::ptr::null()) };
    if child.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(child, &mut pid) };
    (pid != 0).then_some(pid)
}

/// The folder a common Open/Save dialog is showing, read from its address
/// breadcrumb ("Address: C:\..."). English UI text; other languages fall
/// back to the title and tree checks.
fn dialog_folder(hwnd: HWND) -> Option<String> {
    use uiautomation::types::{ControlType, Handle};
    let automation = uiautomation::UIAutomation::new_direct().ok()?;
    let root = automation.element_from_handle(Handle::from(windows::Win32::Foundation::HWND(hwnd))).ok()?;
    let bar = automation
        .create_matcher()
        .from(root)
        .control_type(ControlType::ToolBar)
        .contains_name("Address: ")
        .depth(12)
        .timeout(400)
        .find_first()
        .ok()?;
    let name = bar.get_name().ok()?;
    let path = name.strip_prefix("Address: ")?.trim().to_string();
    (path.len() > 2 && (path.as_bytes().get(1) == Some(&b':') || path.starts_with("\\\\"))).then_some(path)
}

fn clipboard_facts() -> ClipboardFacts {
    use windows_sys::Win32::System::DataExchange::{GetClipboardOwner, IsClipboardFormatAvailable, RegisterClipboardFormatW};
    let owner = unsafe { GetClipboardOwner() };
    let owner_pid = if owner.is_null() {
        None
    } else {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(owner, &mut pid) };
        Some(pid)
    };
    let marked_sensitive = ["ExcludeClipboardContentFromMonitorProcessing", "Clipboard Viewer Ignore"].iter().any(|name| {
        let w = to_wide(name);
        let id = unsafe { RegisterClipboardFormatW(w.as_ptr()) };
        id != 0 && unsafe { IsClipboardFormatAvailable(id) } != 0
    });
    ClipboardFacts { owner_pid, marked_sensitive }
}

fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    wide_to_string(&buf[..n.max(0) as usize])
}

fn window_text(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    wide_to_string(&buf[..n.max(0) as usize])
}

fn frame_rect(hwnd: HWND) -> Option<ScreenRect> {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    let hr = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut r as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if hr != 0 && unsafe { GetWindowRect(hwnd, &mut r) } == 0 {
        return None;
    }
    Some(ScreenRect { left: r.left, top: r.top, right: r.right, bottom: r.bottom })
}

fn image_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        (ok != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn image_name_from_snapshot(pid: u32) -> Option<String> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32ProcessID == pid {
                    found = Some(wide_to_string(&entry.szExeFile));
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> i32 {
    let list = unsafe { &mut *(lparam as *mut Vec<isize>) };
    list.push(hwnd as isize);
    1
}

fn final_path(path: &str) -> Option<String> {
    let wide = to_wide(path);
    unsafe {
        let h = CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut buf = vec![0u16; 2048];
        let n = GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), buf.len() as u32, 0);
        CloseHandle(h);
        (n > 0 && (n as usize) < buf.len()).then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

/// `file:///C:/x` and `file://server/share` to Windows paths.
pub(crate) fn file_url_to_path(url: &str) -> Option<String> {
    let rest = url.strip_prefix("file:///").map(|r| r.to_string()).or_else(|| {
        url.strip_prefix("file://").map(|r| format!("//{r}"))
    })?;
    Some(super::policy::percent_decode(&rest).replace('/', "\\"))
}

/// HWND and folder of every open Explorer window, read through the shell's
/// window list. Empty on any COM failure; Explorer windows then fall back
/// to their title, which the title rules still see.
fn explorer_folders() -> Vec<(isize, String)> {
    use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Shell::{IShellWindows, IWebBrowser2, ShellWindows};
    use windows::core::Interface;
    let mut out = Vec::new();
    let Ok(windows) = (unsafe { CoCreateInstance::<_, IShellWindows>(&ShellWindows, None, CLSCTX_ALL) }) else {
        return out;
    };
    let count = unsafe { windows.Count() }.unwrap_or(0);
    for i in 0..count {
        let Ok(disp) = (unsafe { windows.Item(&VARIANT::from(i)) }) else { continue };
        let Ok(browser) = disp.cast::<IWebBrowser2>() else { continue };
        let hwnd = unsafe { browser.HWND() }.map(|h| h.0).unwrap_or(0);
        let url = unsafe { browser.LocationURL() }.map(|b| b.to_string()).unwrap_or_default();
        if let Some(path) = file_url_to_path(&url) {
            out.push((hwnd, path));
        }
    }
    out
}

impl Probe for WinProbe {
    fn process(&self, pid: u32) -> ProcessFacts {
        let image_path = image_path(pid);
        let image_name = image_path
            .as_deref()
            .map(super::policy::file_name)
            .or_else(|| image_name_from_snapshot(pid));
        ProcessFacts { image_path, image_name }
    }

    fn window(&self, id: &str) -> Option<LiveWindow> {
        let raw: usize = id.strip_prefix("w-")?.parse().ok()?;
        let hwnd = raw as HWND;
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        self.live(if root.is_null() { hwnd } else { root })
    }

    fn top_level_windows(&self) -> Option<Vec<LiveWindow>> {
        let mut handles: Vec<isize> = Vec::new();
        let ok = unsafe { EnumWindows(Some(collect), (&mut handles as *mut Vec<isize>) as LPARAM) };
        if ok == 0 {
            return None;
        }
        Some(
            handles
                .into_iter()
                .filter(|h| unsafe { IsWindowVisible(*h as HWND) } != 0)
                .filter_map(|h| self.live(h as HWND))
                .collect(),
        )
    }

    fn z_ordered_windows(&self) -> Option<Vec<LiveWindow>> {
        let mut out = Vec::new();
        let mut h = unsafe { GetTopWindow(std::ptr::null_mut()) };
        if h.is_null() {
            return None;
        }
        let mut guard = 0;
        while !h.is_null() && guard < 20_000 {
            if unsafe { IsWindowVisible(h) } != 0 {
                if let Some(w) = self.live(h) {
                    out.push(w);
                }
            }
            h = unsafe { GetWindow(h, GW_HWNDNEXT) };
            guard += 1;
        }
        if guard >= 20_000 {
            return None;
        }
        Some(out)
    }

    fn foreground(&self) -> Option<LiveWindow> {
        self.live(unsafe { GetForegroundWindow() })
    }

    fn window_at(&self, x: i32, y: i32) -> Option<LiveWindow> {
        let hwnd = unsafe { WindowFromPoint(POINT { x, y }) };
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        self.live(if root.is_null() { hwnd } else { root })
    }

    fn resolve_path(&self, path: &str) -> String {
        let cleaned = path.trim().trim_matches('"').replace('/', "\\");
        let mut probe = std::path::PathBuf::from(&cleaned);
        let mut rest: Vec<std::ffi::OsString> = Vec::new();
        loop {
            if let Some(found) = probe.to_str().and_then(final_path) {
                let mut full = std::path::PathBuf::from(found);
                for part in rest.iter().rev() {
                    full.push(part);
                }
                return full.to_string_lossy().into_owned();
            }
            match (probe.file_name().map(|f| f.to_os_string()), probe.parent()) {
                (Some(name), Some(parent)) if !parent.as_os_str().is_empty() => {
                    rest.push(name);
                    probe = parent.to_path_buf();
                }
                _ => return cleaned,
            }
        }
    }

    fn display_rect(&self, index: usize) -> Option<ScreenRect> {
        use windows_sys::Win32::Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR};
        unsafe extern "system" fn each(_m: HMONITOR, _dc: HDC, r: *mut RECT, lp: LPARAM) -> i32 {
            let list = unsafe { &mut *(lp as *mut Vec<ScreenRect>) };
            let r = unsafe { *r };
            list.push(ScreenRect { left: r.left, top: r.top, right: r.right, bottom: r.bottom });
            1
        }
        let mut list: Vec<ScreenRect> = Vec::new();
        unsafe {
            EnumDisplayMonitors(std::ptr::null_mut(), std::ptr::null(), Some(each), (&mut list as *mut Vec<ScreenRect>) as LPARAM)
        };
        list.get(index).copied()
    }

    fn clipboard_facts(&self) -> Option<ClipboardFacts> {
        Some(clipboard_facts())
    }

    fn virtual_screen(&self) -> Option<ScreenRect> {
        unsafe {
            let left = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let top = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            (w > 0 && h > 0).then_some(ScreenRect { left, top, right: left + w, bottom: top + h })
        }
    }
}
