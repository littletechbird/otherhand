//! The origin a secret is about to be used on. A native window's origin is
//! its process image (`app://<full path>` or `app://<image name>`). A
//! Chromium window's origin is the page's site read from its address bar,
//! never the browser executable, so a handle for one site can't be filled
//! into another.

use crate::fence::policy::{ProcessFacts, file_name, normalize_path};

const CHROMIUM_IMAGES: &[&str] = &["chrome.exe", "msedge.exe", "brave.exe", "vivaldi.exe", "opera.exe"];

pub(crate) fn is_chromium(facts: &ProcessFacts) -> bool {
    facts
        .image_name
        .as_deref()
        .is_some_and(|n| CHROMIUM_IMAGES.contains(&n.to_lowercase().as_str()))
}

/// Folders a standard user can't write to. A bare-name origin such as
/// `app://notepad.exe` is only offered for programs that live here, so a
/// renamed copy in Downloads or Temp can't claim a handle meant for the real one.
const TRUSTED_IMAGE_ROOTS: &[&str] = &["c:\\windows", "c:\\program files", "c:\\program files (x86)"];

pub(crate) fn native_origins(facts: &ProcessFacts) -> Vec<String> {
    let mut out = Vec::new();
    let Some(path) = facts.image_path.as_deref() else {
        return out;
    };
    let full = normalize_path(path);
    out.push(format!("app://{}", full.replace('\\', "/")));
    let trusted = TRUSTED_IMAGE_ROOTS.iter().any(|root| crate::fence::policy::path_under(&full, root));
    if trusted {
        out.push(format!("app://{}", file_name(&full)));
    }
    out
}

/// `scheme://host[:port]` from a full page URL. The scheme must be written
/// out: Chromium's address bar hides `http://` as well as `https://`, so a
/// bare host can't tell a cleartext page from a secure one and is refused.
pub(crate) fn web_origin(address: &str) -> Option<String> {
    let a = address.trim();
    if a.is_empty() || a.contains(char::is_whitespace) || a.contains('\\') {
        return None;
    }
    let (scheme, rest) = a.split_once("://")?;
    let scheme = scheme.to_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?.to_lowercase();
    let host = authority.rsplit('@').next()?.to_string();
    let host = match host.strip_suffix(":443").filter(|_| scheme == "https").or_else(|| host.strip_suffix(":80").filter(|_| scheme == "http")) {
        Some(h) => h.to_string(),
        None => host,
    };
    let bare = host.split(':').next().unwrap_or("");
    let bare = bare.strip_suffix('.').unwrap_or(bare);
    if bare.is_empty() || !bare.contains('.') && bare != "localhost" {
        return None;
    }
    // Lookalike and encoded hosts are refused rather than guessed at.
    if !bare.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') || bare.split('.').any(|l| l.starts_with("xn--")) {
        return None;
    }
    let port = host.strip_prefix(bare).unwrap_or("").trim_start_matches('.');
    Some(format!("{scheme}://{bare}{port}"))
}

/// The page's own URL, from the document element, checked against the
/// address bar when that shows a full URL. Text typed into the address bar
/// without navigating then can't pass for the page's site.
pub(crate) fn page_origin(document_url: Option<&str>, address_bar: Option<&str>) -> Option<String> {
    let page = web_origin(document_url?)?;
    if let Some(bar) = address_bar.map(str::trim).filter(|b| !b.is_empty()) {
        if bar.contains("://") && web_origin(bar).as_deref() != Some(page.as_str()) {
            return None;
        }
    }
    Some(page)
}

/// The URL Chromium exposes as the value of the page's document element.
#[cfg(target_os = "windows")]
pub(crate) fn chromium_document_url(window_id: &str) -> Option<String> {
    use uiautomation::types::{ControlType, Handle, UIProperty};
    let raw: usize = window_id.strip_prefix("w-")?.parse().ok()?;
    let hwnd = windows::Win32::Foundation::HWND(raw as *mut core::ffi::c_void);
    let automation = uiautomation::UIAutomation::new_direct().ok()?;
    let root = automation.element_from_handle(Handle::from(hwnd)).ok()?;
    let doc = automation.create_matcher().from(root).control_type(ControlType::Document).depth(40).timeout(1500).find_first().ok()?;
    doc.get_property_value(UIProperty::ValueValue).ok()?.get_string().ok().filter(|s| s.contains("://"))
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn chromium_document_url(_window_id: &str) -> Option<String> {
    None
}

#[cfg(target_os = "windows")]
pub(crate) fn chromium_address(window_id: &str) -> Option<String> {
    use uiautomation::types::{ControlType, Handle, UIProperty};
    let raw: usize = window_id.strip_prefix("w-")?.parse().ok()?;
    let hwnd = windows::Win32::Foundation::HWND(raw as *mut core::ffi::c_void);
    let automation = uiautomation::UIAutomation::new_direct().ok()?;
    let root = automation.element_from_handle(Handle::from(hwnd)).ok()?;
    let edit = automation
        .create_matcher()
        .from(root)
        .control_type(ControlType::Edit)
        .contains_name("Address and search bar")
        .depth(40)
        .timeout(1500)
        .find_first()
        .ok()?;
    edit.get_property_value(UIProperty::ValueValue).ok()?.get_string().ok()
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn chromium_address(_window_id: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_origins_come_from_the_address_text() {
        assert_eq!(web_origin("example.com/login?x=1"), None, "no scheme: http and https look the same");
        assert_eq!(web_origin("https://Accounts.Example.com/a").as_deref(), Some("https://accounts.example.com"));
        assert_eq!(web_origin("http://localhost:8080/x").as_deref(), Some("http://localhost:8080"));
        assert_eq!(web_origin("https://user@evil.test/").as_deref(), Some("https://evil.test"));
        assert_eq!(web_origin("search words here"), None);
        assert_eq!(web_origin("file:///C:/x"), None);
        assert_eq!(web_origin("javascript:alert(1)"), None);
    }

    #[test]
    fn native_origins_offer_path_and_name_and_chromium_is_detected() {
        let f = ProcessFacts { image_path: Some("C:\\Windows\\System32\\notepad.exe".into()), image_name: Some("notepad.exe".into()) };
        assert_eq!(native_origins(&f), vec!["app://c:/windows/system32/notepad.exe", "app://notepad.exe"]);
        assert!(!is_chromium(&f));
        let c = ProcessFacts { image_path: None, image_name: Some("Chrome.exe".into()) };
        assert!(is_chromium(&c));
    }
}
