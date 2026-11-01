//! Non-activating routes for standard Win32 controls in headless mode.
//!
//! The UI Automation proxies for classic Win32 controls move keyboard focus
//! before they act (ValuePattern on an Edit, Invoke on a Button, Toggle on a
//! check box), and moving focus into a background window activates it, so
//! the target app takes the person's foreground. For those controls the same
//! effect is reachable with the control's own window messages, which never
//! activate anything:
//!
//! - Edit / RichEdit set-value: `EM_SETSEL(0, -1)` then `EM_REPLACESEL`, read
//!   back with `WM_GETTEXT` (never read back from a password edit).
//! - Push button click: `WM_COMMAND(BN_CLICKED)` posted to the parent, which
//!   is exactly the notification a real click produces.
//! - Check box toggle / check / uncheck: `BM_SETCHECK` (automatic styles
//!   only) plus the same `BN_CLICKED` notification, verified with
//!   `BM_GETCHECK`.
//!
//! Classic Win32 menu bars can only drop down on the active window, so a
//! headless expand (or a click that would open a submenu) is refused up
//! front instead of activating the app and having the menu close again.
//! The test for "classic menu item" does not lean on `FrameworkId` alone:
//! sandbox run 3 showed Notepad's menu bar items slipping past a
//! `FrameworkId == "Win32"` check (their items come from the MSAA proxy,
//! which can report an empty framework), so [`classic_menu_item`] also
//! accepts an empty or MSAA framework and any menu-bar item whose top-level
//! window owns a real `HMENU`.
//!
//! Anything else returns `None` and runs through the normal UIA chain. The
//! fence checks run before this module is reached and are unchanged.

use agent_desktop_core::{ActionRequest, ActionResult, AdapterError, NativeHandle};

#[cfg(target_os = "windows")]
pub fn try_headless_action(
    handle: &NativeHandle,
    request: &ActionRequest,
) -> Option<Result<ActionResult, AdapterError>> {
    imp::try_headless_action(handle, request)
}

#[cfg(not(target_os = "windows"))]
pub fn try_headless_action(
    _handle: &NativeHandle,
    _request: &ActionRequest,
) -> Option<Result<ActionResult, AdapterError>> {
    None
}

/// What a check-box action should leave the box at, from its style and its
/// current state. `None` means "not a case this module handles".
pub fn checkbox_target(kind: u32, current: u32, action: CheckAction) -> Option<u32> {
    let three_state = matches!(kind, BS_3STATE | BS_AUTO3STATE);
    if !matches!(kind, BS_CHECKBOX | BS_AUTOCHECKBOX | BS_3STATE | BS_AUTO3STATE) {
        return None;
    }
    Some(match action {
        CheckAction::Toggle if three_state => (current + 1) % 3,
        CheckAction::Toggle => u32::from(current == 0),
        CheckAction::Check => 1,
        CheckAction::Uncheck => 0,
    })
}

/// What the live check reads about a menu item before deciding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MenuFacts {
    /// UIA control type is `MenuItem`.
    pub is_menu_item: bool,
    /// UIA `FrameworkId`, empty when it could not be read.
    pub framework: String,
    /// The item's parent in the control view is a `MenuBar`.
    pub parent_is_menu_bar: bool,
    /// The item's top-level window has a classic menu (`GetMenu != NULL`).
    pub owner_has_win32_menu: bool,
}

/// Frameworks whose menus are drawn by the app itself, not by USER32.
const OWN_DRAWN_MENU_FRAMEWORKS: &[&str] =
    &["chrome", "xaml", "directui", "wpf", "winform", "internetexplorer", "gecko", "silverlight", "qt"];

/// Whether a menu item belongs to a classic USER32 menu, which only opens on
/// the active window. An item whose framework could not be read, or reads
/// as the MSAA proxy, fails closed. A known self-drawn framework is still a
/// classic menu when it sits on a menu bar that USER32 owns (a WinForms
/// `MainMenu`, for example).
pub fn classic_menu_item(facts: &MenuFacts) -> bool {
    if !facts.is_menu_item {
        return false;
    }
    let framework = facts.framework.trim().to_ascii_lowercase();
    match framework.as_str() {
        "win32" | "" | "msaa" => true,
        f if OWN_DRAWN_MENU_FRAMEWORKS.contains(&f) => facts.owner_has_win32_menu && facts.parent_is_menu_bar,
        _ => facts.owner_has_win32_menu,
    }
}

/// Whether a headless action on a classic menu item has to be refused.
/// Expand always is. A click is when it would open a submenu or the item
/// sits on the menu bar itself, where even a plain item's default action
/// enters menu mode on the owning window.
pub fn refuse_menu_action(facts: &MenuFacts, is_expand: bool, has_expand_collapse: bool) -> bool {
    classic_menu_item(facts) && (is_expand || has_expand_collapse || facts.parent_is_menu_bar)
}

/// Push-button styles whose click is a plain `BN_CLICKED`.
pub fn is_push_kind(kind: u32) -> bool {
    matches!(kind, 0x0 | 0x1 | 0xB | 0xC | 0xD | 0xE | 0xF)
}

/// Builds the `WM_COMMAND` wParam for a `BN_CLICKED` from a control id.
pub fn bn_clicked_wparam(control_id: i32) -> usize {
    (control_id as u32 & 0xFFFF) as usize
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckAction {
    Toggle,
    Check,
    Uncheck,
}

pub const BS_CHECKBOX: u32 = 0x2;
pub const BS_AUTOCHECKBOX: u32 = 0x3;
pub const BS_3STATE: u32 = 0x5;
pub const BS_AUTO3STATE: u32 = 0x6;

#[cfg(target_os = "windows")]
mod imp {
    use super::{CheckAction, MenuFacts, bn_clicked_wparam, checkbox_target, is_push_kind, refuse_menu_action};
    use crate::actions::chain::{DeliveryOutcome, build_step};
    use crate::tree::element::uia_element;
    use crate::tree::live_read::corroborate_verified_process;
    use agent_desktop_core::{
        Action, ActionRequest, ActionResult, AdapterError, DeliverySemantics, ErrorCode,
        NativeHandle,
    };
    use uiautomation::UIElement;
    use uiautomation::patterns::UIExpandCollapsePattern;
    use uiautomation::types::ControlType;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GA_ROOT, GWL_STYLE, GetAncestor, GetClassNameW, GetDlgCtrlID, GetMenu, GetParent, GetWindowLongW,
        IsWindowVisible, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COMMAND, WM_GETTEXT,
        WM_GETTEXTLENGTH,
    };

    const EM_SETSEL: u32 = 0x00B1;
    const EM_REPLACESEL: u32 = 0x00C2;
    const BM_GETCHECK: u32 = 0x00F0;
    const BM_SETCHECK: u32 = 0x00F1;
    const ES_PASSWORD: u32 = 0x0020;
    const ES_READONLY: u32 = 0x0800;
    const BS_TYPEMASK: u32 = 0x000F;
    const SEND_TIMEOUT_MS: u32 = 2_000;

    const EDIT_LABEL: &str = "Win32.EM_REPLACESEL";
    const BUTTON_LABEL: &str = "Win32.WM_COMMAND(BN_CLICKED)";
    const CHECK_LABEL: &str = "Win32.BM_SETCHECK+BN_CLICKED";
    const CHECK_ALREADY_LABEL: &str = "Win32.BM_GETCHECK(already)";

    pub(super) fn try_headless_action(
        handle: &NativeHandle,
        request: &ActionRequest,
    ) -> Option<Result<ActionResult, AdapterError>> {
        if request.policy.is_headed() || handle.is_null() {
            return None;
        }
        let element = uia_element(handle).ok()?;
        let el = &element.0;
        let outcome = match &request.action {
            Action::Expand => menu_refusal(el, "expand"),
            Action::Click => menu_refusal(el, "click").or_else(|| click(el)),
            Action::SetValue(value) => edit_set(el, value),
            Action::Toggle => checkbox(el, CheckAction::Toggle),
            Action::Check => checkbox(el, CheckAction::Check),
            Action::Uncheck => checkbox(el, CheckAction::Uncheck),
            _ => None,
        }?;
        Some(outcome.and_then(|steps| {
            corroborate_verified_process(element)?;
            Ok(ActionResult::from_execution(&request.action, steps))
        }))
    }

    type Steps = Result<Vec<agent_desktop_core::ActionStep>, AdapterError>;

    fn hwnd_of(el: &UIElement) -> Option<HWND> {
        let handle = el.get_native_window_handle().ok()?;
        let hwnd: windows::Win32::Foundation::HWND = handle.into();
        (!hwnd.0.is_null()).then_some(hwnd.0 as HWND)
    }

    fn class_of(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    fn style_of(hwnd: HWND) -> u32 {
        unsafe { GetWindowLongW(hwnd, GWL_STYLE) as u32 }
    }

    fn usable(hwnd: HWND) -> bool {
        unsafe { IsWindowEnabled(hwnd) != 0 && IsWindowVisible(hwnd) != 0 }
    }

    fn send(hwnd: HWND, msg: u32, w: usize, l: isize) -> Result<usize, AdapterError> {
        let mut out: usize = 0;
        let ok = unsafe { SendMessageTimeoutW(hwnd, msg, w, l, SMTO_ABORTIFHUNG, SEND_TIMEOUT_MS, &mut out) };
        if ok == 0 {
            return Err(AdapterError::new(ErrorCode::ActionFailed, "The control did not answer its window message in time")
                .with_suggestion("Take a fresh snapshot and check the control before retrying.")
                .with_disposition(DeliverySemantics::uncertain()));
        }
        Ok(out)
    }

    fn not_delivered(code: ErrorCode, message: &str, suggestion: &str) -> AdapterError {
        AdapterError::new(code, message.to_string())
            .with_suggestion(suggestion.to_string())
            .with_disposition(DeliverySemantics::not_delivered())
    }

    /// Win32 menu-bar items open only on the active window. Everything the
    /// decision needs is read here, before any pattern is called.
    fn menu_refusal(el: &UIElement, what: &str) -> Option<Steps> {
        if el.get_control_type().ok() != Some(ControlType::MenuItem) {
            return None;
        }
        let facts = menu_facts(el);
        let has_expand_collapse = el.get_pattern::<UIExpandCollapsePattern>().is_ok();
        if !refuse_menu_action(&facts, what == "expand", has_expand_collapse) {
            return None;
        }
        Some(Err(not_delivered(
            ErrorCode::PolicyDenied,
            "A classic Win32 menu only opens on the active window, so opening it headless would take the person's foreground",
            "Use the app's own command another way (a toolbar button or a dialog), or ask the user for --headed",
        )
        .with_details(serde_json::json!({
            "kind": "win32_menu_headless",
            "framework": facts.framework,
            "on_menu_bar": facts.parent_is_menu_bar,
            "owner_has_menu": facts.owner_has_win32_menu,
        }))))
    }

    /// Reads the menu facts. A failed read leaves the fact at its
    /// fail-closed default (an unreadable framework counts as classic).
    fn menu_facts(el: &UIElement) -> MenuFacts {
        let mut facts = MenuFacts {
            is_menu_item: true,
            framework: el.get_framework_id().unwrap_or_default(),
            ..MenuFacts::default()
        };
        let Ok(client) = crate::tree::automation::automation_client() else {
            return facts;
        };
        let Ok(walker) = client.get_control_view_walker() else {
            return facts;
        };
        let mut current = walker.get_parent(el).ok();
        if let Some(parent) = &current {
            facts.parent_is_menu_bar = parent.get_control_type().ok() == Some(ControlType::MenuBar);
        }
        let mut owner = hwnd_of(el);
        let mut hops = 0;
        while owner.is_none() && hops < 8 {
            let Some(node) = current.take() else { break };
            owner = hwnd_of(&node);
            current = walker.get_parent(&node).ok();
            hops += 1;
        }
        if let Some(hwnd) = owner {
            let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
            let top = if root.is_null() { hwnd } else { root };
            facts.owner_has_win32_menu = !unsafe { GetMenu(top) }.is_null();
        }
        facts
    }

    fn click(el: &UIElement) -> Option<Steps> {
        let hwnd = hwnd_of(el)?;
        if !class_of(hwnd).eq_ignore_ascii_case("Button") || !usable(hwnd) {
            return None;
        }
        let kind = style_of(hwnd) & BS_TYPEMASK;
        if checkbox_target(kind, 0, CheckAction::Toggle).is_some() {
            return toggle_box(hwnd, kind, CheckAction::Toggle);
        }
        if !is_push_kind(kind) {
            return None;
        }
        Some(post_clicked(hwnd).map(|()| vec![build_step(BUTTON_LABEL, DeliveryOutcome::DeliveredUnverified)]))
    }

    fn post_clicked(hwnd: HWND) -> Result<(), AdapterError> {
        let parent = unsafe { GetParent(hwnd) };
        if parent.is_null() {
            return Err(not_delivered(ErrorCode::ActionFailed, "The button has no parent window to notify", "Use another route for this control"));
        }
        let id = unsafe { GetDlgCtrlID(hwnd) };
        let posted = unsafe { PostMessageW(parent, WM_COMMAND, bn_clicked_wparam(id), hwnd as isize) };
        if posted == 0 {
            return Err(not_delivered(ErrorCode::ActionFailed, "The click notification could not be posted", "Take a fresh snapshot and retry"));
        }
        Ok(())
    }

    fn checkbox(el: &UIElement, action: CheckAction) -> Option<Steps> {
        let hwnd = hwnd_of(el)?;
        if !class_of(hwnd).eq_ignore_ascii_case("Button") || !usable(hwnd) {
            return None;
        }
        toggle_box(hwnd, style_of(hwnd) & BS_TYPEMASK, action)
    }

    fn toggle_box(hwnd: HWND, kind: u32, action: CheckAction) -> Option<Steps> {
        let current = send(hwnd, BM_GETCHECK, 0, 0).ok()? as u32;
        let target = checkbox_target(kind, current, action)?;
        Some((|| {
            if current == target {
                return Ok(vec![build_step(CHECK_ALREADY_LABEL, DeliveryOutcome::SatisfiedNoDelivery)]);
            }
            let automatic = matches!(kind, super::BS_AUTOCHECKBOX | super::BS_AUTO3STATE);
            if automatic {
                send(hwnd, BM_SETCHECK, target as usize, 0)?;
            }
            post_clicked(hwnd)?;
            let mut verified = false;
            for _ in 0..30 {
                std::thread::sleep(std::time::Duration::from_millis(10));
                if send(hwnd, BM_GETCHECK, 0, 0).ok().map(|v| v as u32) == Some(target) {
                    verified = true;
                    break;
                }
            }
            Ok(vec![build_step(CHECK_LABEL, DeliveryOutcome::from_delivery(true, verified))])
        })())
    }

    fn edit_set(el: &UIElement, value: &str) -> Option<Steps> {
        let hwnd = hwnd_of(el)?;
        let class = class_of(hwnd).to_ascii_lowercase();
        if !(class == "edit" || class.starts_with("richedit")) || !usable(hwnd) {
            return None;
        }
        let style = style_of(hwnd);
        if style & ES_READONLY != 0 {
            return None;
        }
        let password = style & ES_PASSWORD != 0;
        let mut wide: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
        let written = send(hwnd, EM_SETSEL, 0, -1)
            .and_then(|_| send(hwnd, EM_REPLACESEL, 1, wide.as_ptr() as isize));
        let outcome = match written {
            Err(e) => Err(e),
            Ok(_) if password => Ok(DeliveryOutcome::DeliveredUnobserved),
            Ok(_) => Ok(DeliveryOutcome::from_delivery(true, reads_back(hwnd, &wide[..wide.len() - 1]))),
        };
        wide.iter_mut().for_each(|c| *c = 0);
        drop(wide);
        Some(outcome.map(|o| vec![build_step(EDIT_LABEL, o)]))
    }

    /// Whether the edit now holds exactly `expected`. The copy read back is
    /// wiped before it is dropped; nothing about it leaves this function.
    fn reads_back(hwnd: HWND, expected: &[u16]) -> bool {
        let Ok(len) = send(hwnd, WM_GETTEXTLENGTH, 0, 0) else {
            return false;
        };
        if len != expected.len() {
            return false;
        }
        let mut buf = vec![0u16; len + 1];
        let got = send(hwnd, WM_GETTEXT, len + 1, buf.as_mut_ptr() as isize).unwrap_or(0);
        let same = got == len && buf[..len] == *expected;
        buf.iter_mut().for_each(|c| *c = 0);
        same
    }
}

#[cfg(test)]
#[path = "quiet_tests.rs"]
mod tests;
