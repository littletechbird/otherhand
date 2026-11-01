//! `FencedAdapter` wraps the platform adapter so every read, action, capture
//! and launch passes the exclusion fence first. Fenced windows are left out of
//! listings, refused everywhere else, and blacked out in screenshots.

use agent_desktop_core::{
    AccessibilityNode, ActionOps, AdapterError, AdapterSession, AppInfo, AppInventory,
    AppInventoryQuery, ClipboardContent, ClipboardFormat, CursorOverlayControl, Deadline,
    DeliverySemantics, DismissAllNotificationsRequest, DismissNotificationRequest, DisplayInfo,
    DragParams, ElementState, ErrorCode, HitTestResult, ImageBuffer, InputOps, InteractionLease,
    InteractionPolicy, KeyCombo, LiveElement, MouseEvent, NativeHandle, NotificationActionRequest,
    NotificationFilter, NotificationInfo, ObservationOps, ObservationRequest, ObservationRoot,
    ObservedTree, OperationReport, PermissionReport, PlatformAdapter, Point, PointerObservation,
    ProcessId, ProcessIdentity, Rect, RefEntry, ScreenshotTarget, SessionAffinity, SignalBaseline,
    SignalFilter, SnapshotSurface, SurfaceInfo, SystemOps, TreeOptions, WaitWakeup, WindowFilter,
    WindowInfo, WindowOp, actions::action::Action, actions::action_request::ActionRequest,
    actions::action_result::ActionResult,
    system::{
        launch_options::LaunchOptions, launch_result::LaunchResult, process_state::ProcessState,
    },
};

use super::audit;
use super::policy::{Fence, Rule, expand_env, normalize_path, path_tokens, percent_decode};
use super::probe::{LiveWindow, Probe, ScreenRect};

pub(crate) struct FencedAdapter<'a> {
    inner: &'a dyn PlatformAdapter,
    fence: Fence,
    probe: Box<dyn Probe>,
}

/// Blacks out any glow strip that was still on screen, not excluded from
/// capture, when the pixels were taken (the helper didn't answer in time).
/// With strips to cover and no known capture area, the capture is refused
/// rather than returned with the frame in it.
fn mask_glow(image: ImageBuffer, area: Option<ScreenRect>, strips: &[crate::glow::Rect]) -> Result<ImageBuffer, AdapterError> {
    if strips.is_empty() {
        return Ok(image);
    }
    let refuse = || {
        AdapterError::new(ErrorCode::Internal, "The screen-border glow could not be kept out of the screenshot")
            .with_suggestion("Retry the screenshot; the glow fades out a moment after the last action.")
    };
    let area = area.ok_or_else(refuse)?;
    let boxes: Vec<ScreenRect> = strips
        .iter()
        .map(|r| ScreenRect { left: r.x, top: r.y, right: r.x + r.w, bottom: r.y + r.h })
        .filter_map(|r| r.intersect(&area))
        .collect();
    if boxes.is_empty() {
        return Ok(image);
    }
    let mapped = super::redact::map_to_image(&boxes, area, image.width, image.height);
    let image = super::redact::black_out(image, &mapped).map_err(|_| refuse())?;
    audit::append("glow_masked", &[("regions", &mapped.len().to_string())]);
    Ok(image)
}

pub(crate) fn fenced_error(action: &str, rule: Rule, target: &str) -> AdapterError {
    audit::append(
        "fence_refused",
        &[("action", action), ("rule", rule.as_str()), ("target", target)],
    );
    AdapterError::new(ErrorCode::Fenced, "Target is inside the user's exclusion fence")
        .with_suggestion("The user has fenced this program, window or path. Do not retry or look for another route to it.")
        .with_details(serde_json::json!({ "fence": "FENCED", "action": action, "rule": rule.as_str() }))
        .with_disposition(DeliverySemantics::not_delivered())
}

impl<'a> FencedAdapter<'a> {
    pub(crate) fn new(inner: &'a dyn PlatformAdapter, fence: Fence, probe: Box<dyn Probe>) -> Self {
        Self { inner, fence, probe }
    }

    fn target_name(w: &LiveWindow) -> String {
        w.facts.process.image_name.clone().unwrap_or_else(|| "unknown".into()).to_lowercase()
    }

    pub(crate) fn check_pid(&self, action: &str, pid: u32) -> Result<(), AdapterError> {
        let facts = self.probe.process(pid);
        let name = facts.image_name.clone().unwrap_or_else(|| "unknown".into()).to_lowercase();
        if let Some(rule) = self.fence.match_process(&facts) {
            return Err(fenced_error(action, rule, &name));
        }
        let Some(windows) = self.probe.top_level_windows() else {
            return Err(fenced_error(action, Rule::UnknownIdentity, &name));
        };
        for w in windows.iter().filter(|w| w.pid == pid && w.visible) {
            if let Some(rule) = self.fence.match_window(&w.facts) {
                return Err(fenced_error(action, rule, &name));
            }
        }
        Ok(())
    }

    pub(crate) fn check_window_id(&self, action: &str, id: &str, pid: u32) -> Result<(), AdapterError> {
        match self.probe.window(id) {
            Some(w) => {
                if let Some(rule) = self.fence.match_window(&w.facts) {
                    return Err(fenced_error(action, rule, &Self::target_name(&w)));
                }
                if pid != 0 && w.pid != pid {
                    return self.check_pid(action, pid);
                }
                Ok(())
            }
            None => self.check_pid(action, pid),
        }
    }

    fn check_window(&self, action: &str, win: &WindowInfo) -> Result<(), AdapterError> {
        self.check_window_id(action, &win.id, win.pid.get())
    }

    fn check_entry(&self, action: &str, entry: &RefEntry) -> Result<(), AdapterError> {
        match entry.source.source_window_id.as_deref() {
            Some(id) => self.check_window_id(action, id, entry.process.pid.get()),
            None => self.check_pid(action, entry.process.pid.get()),
        }
    }

    fn check_live(&self, action: &str, w: Option<LiveWindow>) -> Result<(), AdapterError> {
        let Some(w) = w else {
            return Err(fenced_error(action, Rule::UnknownIdentity, "unknown"));
        };
        if let Some(rule) = self.fence.match_window(&w.facts) {
            return Err(fenced_error(action, rule, &Self::target_name(&w)));
        }
        Ok(())
    }

    pub(crate) fn check_text(&self, action: &str, text: &str) -> Result<(), AdapterError> {
        let expanded = expand_env(text, &super::env_lookup);
        let mut forms = vec![text.to_string(), percent_decode(text)];
        if expanded != text {
            forms.push(percent_decode(&expanded));
            forms.push(expanded);
        }
        for form in &forms {
            if self.fence.text_mentions_fenced_root(form) || self.fence.relative_mentions_root(form) {
                return Err(fenced_error(action, Rule::Path, "text"));
            }
            for token in path_tokens(form) {
                if self.probe.is_shortcut(&token) {
                    return Err(fenced_error(action, Rule::UnknownIdentity, "shortcut"));
                }
                if self.fence.path_is_fenced(&token) || self.fence.path_is_fenced(&self.probe.resolve_path(&token)) {
                    return Err(fenced_error(action, Rule::Path, "text"));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn check_path(&self, action: &str, path: &str) -> Result<(), AdapterError> {
        let expanded = expand_env(path, &super::env_lookup);
        for p in [path, expanded.as_str()] {
            if self.probe.is_shortcut(p) {
                return Err(fenced_error(action, Rule::UnknownIdentity, "shortcut"));
            }
            let resolved = self.probe.resolve_path(p);
            if self.fence.path_is_fenced(p) || self.fence.path_is_fenced(&resolved) {
                return Err(fenced_error(action, Rule::Path, "path"));
            }
            let n = normalize_path(&resolved);
            if self.fence.exe_paths().iter().any(|root| super::policy::path_under(&n, root)) {
                return Err(fenced_error(action, Rule::ExePath, "path"));
            }
        }
        Ok(())
    }

    fn check_launch(&self, id: &str, options: &LaunchOptions) -> Result<(), AdapterError> {
        let expanded = expand_env(id, &super::env_lookup);
        let clean = normalize_path(&expanded);
        let name = super::policy::file_name(&clean);
        let image = if name.ends_with(".exe") { name.clone() } else { format!("{name}.exe") };
        let has_path = clean.contains('\\');
        let facts = super::policy::ProcessFacts {
            image_path: has_path.then(|| self.probe.resolve_path(&expanded)),
            image_name: Some(image.clone()),
        };
        if let Some(rule) = self.fence.match_process(&facts) {
            return Err(fenced_error("launch_app", rule, &image));
        }
        if has_path || self.probe.is_shortcut(&expanded) {
            self.check_path("launch_app", id)?;
        }
        let cwd = options.cwd.as_ref().and_then(|c| c.to_str());
        for arg in &options.args {
            self.check_text("launch_app", arg)?;
            if self.probe.is_shortcut(arg) {
                return Err(fenced_error("launch_app", Rule::UnknownIdentity, "shortcut"));
            }
            if let Some(cwd) = cwd {
                let a = arg.trim().trim_matches('"');
                if !a.contains(':') && !a.starts_with("\\\\") {
                    self.check_text("launch_app", &format!("{}\\{}", cwd.trim_end_matches(['\\', '/']), a))?;
                }
            }
        }
        for (key, value) in &options.env {
            self.check_text("launch_app", key)?;
            self.check_text("launch_app", value)?;
        }
        if let Some(cwd) = cwd {
            self.check_path("launch_app", cwd)?;
        }
        Ok(())
    }

    /// The on-screen parts of every visible fenced window right now (the
    /// window rectangle minus opaque unfenced windows above it); `None` when
    /// the window list cannot be read, which refuses the capture.
    fn fenced_rects(&self) -> Option<Vec<ScreenRect>> {
        let windows = self.probe.z_ordered_windows()?;
        Some(super::probe::exposed_fenced_regions(&windows, |w| self.fence.match_window(&w.facts).is_some()))
    }

    pub(crate) fn notification_fenced(&self, n: &NotificationInfo) -> bool {
        let app = n.app_name.trim().to_lowercase();
        if !app.is_empty() {
            let as_exe = if app.ends_with(".exe") { app.clone() } else { format!("{app}.exe") };
            let facts = super::policy::ProcessFacts { image_path: None, image_name: Some(as_exe) };
            if self.fence.match_process(&facts).is_some() {
                return true;
            }
        }
        for title in [n.app_name.as_str(), n.title.as_str()] {
            let w = super::policy::WindowFacts {
                class: String::new(),
                title: title.to_string(),
                process: super::policy::ProcessFacts { image_path: None, image_name: Some("notification".into()) },
                folder: Some(String::new()),
            };
            if self.fence.match_window(&w).is_some() {
                return true;
            }
        }
        let body = n.body.clone().unwrap_or_default();
        self.check_text_quiet(&n.title) || self.check_text_quiet(&body)
    }

    /// Looks the notification up before acting on it, so a toast from a
    /// fenced app can't be dismissed or have its buttons pressed by index.
    fn check_notification_target(&self, action: &str, index: usize, identity: &agent_desktop_core::NotificationIdentity, policy: InteractionPolicy, lease: &InteractionLease) -> Result<(), AdapterError> {
        let named = NotificationInfo {
            index,
            app_name: identity.expected_app.clone().unwrap_or_default(),
            title: identity.expected_title.clone().unwrap_or_default(),
            body: None,
            actions: vec![],
        };
        if self.notification_fenced(&named) {
            return Err(fenced_error(action, Rule::ProcessName, "notification"));
        }
        let all = self.inner.list_notifications(&NotificationFilter::default(), policy, lease.deadline(), Some(lease)).map_err(|_| fenced_error(action, Rule::UnknownIdentity, "notification"))?;
        if all.iter().any(|n| n.index == index && self.notification_fenced(n)) {
            return Err(fenced_error(action, Rule::ProcessName, "notification"));
        }
        Ok(())
    }

    fn check_text_quiet(&self, text: &str) -> bool {
        let t = percent_decode(&expand_env(text, &super::env_lookup));
        self.fence.text_mentions_fenced_root(&t) || path_tokens(&t).iter().any(|p| self.fence.path_is_fenced(p))
    }

    fn check_clipboard_read(&self) -> Result<(), AdapterError> {
        let Some(facts) = self.probe.clipboard_facts() else {
            return Err(fenced_error("clipboard_get", Rule::UnknownIdentity, "clipboard"));
        };
        if facts.marked_sensitive {
            return Err(fenced_error("clipboard_get", Rule::ProcessName, "clipboard"));
        }
        if let Some(pid) = facts.owner_pid.filter(|p| *p != 0) {
            self.check_pid("clipboard_get", pid)?;
        }
        Ok(())
    }

    fn check_clipboard_content(&self, action: &str, content: &ClipboardContent) -> Result<(), AdapterError> {
        match content {
            ClipboardContent::Text(t) => self.check_text(action, t),
            ClipboardContent::FileUrls(urls) => urls.iter().try_for_each(|u| self.check_text(action, u)),
            ClipboardContent::Image(_) => Ok(()),
        }
    }

    fn window_visible_to_agent(&self, win: &WindowInfo) -> bool {
        self.check_window_quiet(&win.id, win.pid.get())
    }

    fn check_window_quiet(&self, id: &str, pid: u32) -> bool {
        match self.probe.window(id) {
            Some(w) => self.fence.match_window(&w.facts).is_none(),
            None => self.fence.match_process(&self.probe.process(pid)).is_none(),
        }
    }

    fn app_visible_to_agent(&self, app: &AppInfo) -> bool {
        self.fence.match_process(&self.probe.process(app.pid.get())).is_none()
    }

    fn capture_rect(&self, target: &ScreenshotTarget, deadline: Deadline) -> Option<ScreenRect> {
        let from_rect = |r: &Rect| ScreenRect {
            left: r.x.round() as i32,
            top: r.y.round() as i32,
            right: (r.x + r.width).round() as i32,
            bottom: (r.y + r.height).round() as i32,
        };
        match target {
            ScreenshotTarget::FullScreen => self.probe.virtual_screen(),
            ScreenshotTarget::Screen(i) => self
                .inner
                .list_displays(deadline)
                .ok()
                .and_then(|d| d.get(*i).map(|d| from_rect(&d.bounds)))
                .or_else(|| self.probe.display_rect(*i)),
            ScreenshotTarget::Display { expected, .. } => Some(from_rect(&expected.bounds)),
            ScreenshotTarget::ExactWindow(win) => self.probe.window(&win.id).and_then(|w| w.rect),
        }
    }

    /// Black out every visible fenced window that overlaps the capture.
    /// Any failure to list windows or decode the image refuses the capture.
    fn redact(&self, mut image: ImageBuffer, area: Option<ScreenRect>, before: Vec<ScreenRect>, action: &str) -> Result<ImageBuffer, AdapterError> {
        // The windows are listed before and after the capture and both sets
        // are blacked out, so a fenced window that moved, closed, appeared or
        // was restored while the pixels were taken is still covered.
        let Some(after) = self.fenced_rects() else {
            return Err(fenced_error(action, Rule::UnknownIdentity, "window_list"));
        };
        let fenced: Vec<ScreenRect> = before.into_iter().chain(after).collect();
        if fenced.is_empty() {
            return Ok(image);
        }
        let Some(area) = area else {
            return Err(fenced_error(action, Rule::UnknownIdentity, "capture_area"));
        };
        let boxes: Vec<ScreenRect> = fenced.iter().filter_map(|r| r.intersect(&area)).collect();
        if boxes.is_empty() {
            return Ok(image);
        }
        let mapped = super::redact::map_to_image(&boxes, area, image.width, image.height);
        image = super::redact::black_out(image, &mapped)
            .map_err(|_| fenced_error(action, Rule::UnknownIdentity, "image_decode"))?;
        audit::append("fence_redacted", &[("action", action), ("regions", &mapped.len().to_string())]);
        Ok(image)
    }

    fn check_request(&self, request: &ActionRequest) -> Result<(), AdapterError> {
        match &request.action {
            Action::SetValue(text) | Action::TypeText(text) | Action::Select(text) => {
                self.check_text("execute_action", text)
            }
            _ => Ok(()),
        }
    }

    fn check_point(&self, action: &str, p: &Point) -> Result<(), AdapterError> {
        self.check_live(action, self.probe.window_at(p.x.round() as i32, p.y.round() as i32))
    }
}

impl ObservationOps for FencedAdapter<'_> {
    fn is_nonspatial_target(&self, handle: &NativeHandle) -> bool {
        self.inner.is_nonspatial_target(handle)
    }
    fn observe_tree(&self, root: ObservationRoot<'_>, request: &ObservationRequest) -> Result<ObservedTree, AdapterError> {
        match root {
            ObservationRoot::Window(win) => self.check_window("observe_tree", win)?,
            ObservationRoot::Element { entry, .. } => self.check_entry("observe_tree", entry)?,
        }
        self.inner.observe_tree(root, request)
    }
    fn list_windows(&self, filter: &WindowFilter, deadline: Deadline) -> Result<Vec<WindowInfo>, AdapterError> {
        let mut list = self.inner.list_windows(filter, deadline)?;
        list.retain(|w| self.window_visible_to_agent(w));
        Ok(list)
    }
    fn list_apps(&self, deadline: Deadline) -> Result<Vec<AppInfo>, AdapterError> {
        let mut list = self.inner.list_apps(deadline)?;
        list.retain(|a| self.app_visible_to_agent(a));
        Ok(list)
    }
    fn list_apps_inventory(&self, deadline: Deadline) -> Result<AppInventory, AdapterError> {
        let mut inv = self.inner.list_apps_inventory(deadline)?;
        inv.apps.retain(|a| self.app_visible_to_agent(a));
        Ok(inv)
    }
    fn list_apps_scoped(&self, name: &str, bundle_id: Option<&str>, deadline: Deadline) -> Result<Vec<AppInfo>, AdapterError> {
        let mut list = self.inner.list_apps_scoped(name, bundle_id, deadline)?;
        list.retain(|a| self.app_visible_to_agent(a));
        Ok(list)
    }
    fn query_app_inventory(&self, query: AppInventoryQuery<'_>, deadline: Deadline) -> Result<AppInventory, AdapterError> {
        let mut inv = self.inner.query_app_inventory(query, deadline)?;
        inv.apps.retain(|a| self.app_visible_to_agent(a));
        Ok(inv)
    }
    fn get_tree(&self, win: &WindowInfo, opts: &TreeOptions, deadline: Deadline) -> Result<AccessibilityNode, AdapterError> {
        self.check_window("get_tree", win)?;
        self.inner.get_tree(win, opts, deadline)
    }
    fn resolve_element_strict(&self, entry: &RefEntry, deadline: Deadline) -> Result<NativeHandle, AdapterError> {
        self.check_entry("resolve_element", entry)?;
        self.inner.resolve_element_strict(entry, deadline)
    }
    fn resolve_locator_anchor(&self, entry: &RefEntry, deadline: Deadline) -> Result<NativeHandle, AdapterError> {
        self.check_entry("resolve_locator_anchor", entry)?;
        self.inner.resolve_locator_anchor(entry, deadline)
    }
    fn list_surfaces(&self, process: ProcessIdentity, deadline: Deadline) -> Result<Vec<SurfaceInfo>, AdapterError> {
        self.check_pid("list_surfaces", process.pid.get())?;
        self.inner.list_surfaces(process, deadline)
    }
    fn resolve_shell_surface(&self, surface: SnapshotSurface, deadline: Deadline) -> Result<WindowInfo, AdapterError> {
        let win = self.inner.resolve_shell_surface(surface, deadline)?;
        self.check_window("resolve_shell_surface", &win)?;
        Ok(win)
    }
    fn get_live_value(&self, handle: &NativeHandle, deadline: Deadline) -> Result<Option<String>, AdapterError> {
        self.inner.get_live_value(handle, deadline)
    }
    fn get_text_selection(&self, handle: &NativeHandle, deadline: Deadline) -> Result<Option<std::ops::Range<usize>>, AdapterError> {
        self.inner.get_text_selection(handle, deadline)
    }
    fn get_live_state(&self, handle: &NativeHandle, deadline: Deadline) -> Result<Option<ElementState>, AdapterError> {
        self.inner.get_live_state(handle, deadline)
    }
    fn get_live_actions(&self, handle: &NativeHandle, deadline: Deadline) -> Result<Option<Vec<String>>, AdapterError> {
        self.inner.get_live_actions(handle, deadline)
    }
    fn get_live_element(&self, handle: &NativeHandle, deadline: Deadline) -> Result<LiveElement, AdapterError> {
        self.inner.get_live_element(handle, deadline)
    }
    fn get_element_bounds(&self, handle: &NativeHandle, deadline: Deadline) -> Result<Option<Rect>, AdapterError> {
        self.inner.get_element_bounds(handle, deadline)
    }
    fn get_insertion_caret(&self, handle: &NativeHandle, deadline: Deadline) -> Result<Option<Rect>, AdapterError> {
        self.inner.get_insertion_caret(handle, deadline)
    }
    fn get_focused_insertion_caret(&self, process: Option<ProcessId>, deadline: Deadline) -> Result<Option<Rect>, AdapterError> {
        match process {
            Some(pid) => self.check_pid("focused_caret", pid.get())?,
            None => self.check_live("focused_caret", self.probe.foreground())?,
        }
        self.inner.get_focused_insertion_caret(process, deadline)
    }
    fn observe_pointer(&self, handle: &NativeHandle, deadline: Deadline) -> Result<PointerObservation, AdapterError> {
        self.inner.observe_pointer(handle, deadline)
    }
    fn pointer_position_refusal(&self, handle: &NativeHandle, deadline: Deadline) -> Option<AdapterError> {
        self.inner.pointer_position_refusal(handle, deadline)
    }
    fn hit_test(&self, handle: &NativeHandle, point: Point, deadline: Deadline) -> Result<HitTestResult, AdapterError> {
        self.inner.hit_test(handle, point, deadline)
    }
    fn confirm_pointer_receiver(&self, entry: &RefEntry, point: Point, deadline: Deadline) -> Result<(), AdapterError> {
        self.check_entry("confirm_pointer_receiver", entry)?;
        self.inner.confirm_pointer_receiver(entry, point, deadline)
    }
}

impl ActionOps for FencedAdapter<'_> {
    fn execute_action(&self, handle: &NativeHandle, request: ActionRequest, lease: &InteractionLease) -> Result<ActionResult, AdapterError> {
        self.check_request(&request)?;
        // Headless: standard Win32 controls are driven with their own window
        // messages, which never activate the app (see the windows `quiet`
        // module). Runs only after the fence has passed the request.
        #[cfg(target_os = "windows")]
        if !request.policy.is_headed() {
            if let Some(outcome) = agent_desktop_windows::quiet::try_headless_action(handle, &request) {
                return outcome;
            }
        }
        self.inner.execute_action(handle, request, lease)
    }
    fn scroll_into_view(&self, handle: &NativeHandle, lease: &InteractionLease) -> Result<(), AdapterError> {
        self.inner.scroll_into_view(handle, lease)
    }
    fn scroll_ref_into_view(&self, entry: &RefEntry, handle: &NativeHandle, lease: &InteractionLease) -> Result<Option<NativeHandle>, AdapterError> {
        self.check_entry("scroll_into_view", entry)?;
        self.inner.scroll_ref_into_view(entry, handle, lease)
    }
}

impl InputOps for FencedAdapter<'_> {
    fn physical_pointer_refusal(&self) -> Option<AdapterError> {
        self.inner.physical_pointer_refusal()
    }
    fn keyboard_needs_window_activation(&self) -> bool {
        self.inner.keyboard_needs_window_activation()
    }
    fn mouse_event(&self, event: MouseEvent, lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.check_point("mouse_event", &event.point)?;
        crate::focus_guard::note_tool_input();
        self.inner.mouse_event(event, lease)
    }
    fn key_event(&self, combo: &KeyCombo, down: bool, lease: &InteractionLease) -> Result<(), AdapterError> {
        self.check_live("key_event", self.probe.foreground())?;
        crate::focus_guard::note_tool_input();
        self.inner.key_event(combo, down, lease)
    }
    fn drag(&self, params: DragParams, lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.check_point("drag", &params.from)?;
        self.check_point("drag", &params.to)?;
        crate::focus_guard::note_tool_input();
        self.inner.drag(params, lease)
    }
    fn clear_clipboard(&self, lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.inner.clear_clipboard(lease)
    }
    fn get_clipboard_content(&self, format: ClipboardFormat, deadline: Deadline) -> Result<Option<ClipboardContent>, AdapterError> {
        self.check_clipboard_read()?;
        let content = self.inner.get_clipboard_content(format, deadline)?;
        if let Some(c) = &content {
            self.check_clipboard_content("clipboard_get", c)?;
        }
        Ok(content)
    }
    fn set_clipboard_content(&self, content: &ClipboardContent, lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.check_clipboard_content("clipboard_set", content)?;
        self.inner.set_clipboard_content(content, lease)
    }
}

impl SystemOps for FencedAdapter<'_> {
    fn update_cursor_overlay(&self, control: &CursorOverlayControl) -> Result<(), AdapterError> {
        self.inner.update_cursor_overlay(control)
    }
    fn run_cursor_overlay_child(&self) -> Option<Result<(), AdapterError>> {
        self.inner.run_cursor_overlay_child()
    }
    fn acquire_interaction_lease(&self, deadline: Deadline) -> Result<InteractionLease, AdapterError> {
        self.inner.acquire_interaction_lease(deadline)
    }
    fn permission_report(&self, deadline: Deadline) -> Result<PermissionReport, AdapterError> {
        self.inner.permission_report(deadline)
    }
    fn unknown_accessibility_means_unsupported(&self) -> bool {
        self.inner.unknown_accessibility_means_unsupported()
    }
    fn activate_renderer_accessibility(&self, process: ProcessIdentity, lease: &InteractionLease) -> Result<(), AdapterError> {
        self.check_pid("activate_renderer_accessibility", process.pid.get())?;
        self.inner.activate_renderer_accessibility(process, lease)
    }
    fn request_permissions(&self, lease: &InteractionLease) -> Result<PermissionReport, AdapterError> {
        self.inner.request_permissions(lease)
    }
    fn start_permission_guide(&self) -> Result<&'static str, AdapterError> {
        self.inner.start_permission_guide()
    }
    fn focus_window(&self, win: &WindowInfo, lease: &InteractionLease) -> Result<(), AdapterError> {
        self.check_window("focus_window", win)?;
        self.inner.focus_window(win, lease)
    }
    fn launch_app(&self, id: &str, options: &LaunchOptions, lease: &InteractionLease) -> Result<LaunchResult, AdapterError> {
        self.check_launch(id, options)?;
        let result = self.inner.launch_app(id, options, lease)?;
        // A name can reach a fenced program through App Paths, PATH or a
        // protocol handler; whatever actually started is checked too.
        self.check_pid("launch_app", result.pid.get())?;
        if let Some(w) = &result.window {
            self.check_window("launch_app", w)?;
        }
        Ok(result)
    }
    fn process_owns_tcp_listener(&self, process: ProcessIdentity, port: u16, deadline: Deadline) -> Result<bool, AdapterError> {
        self.inner.process_owns_tcp_listener(process, port, deadline)
    }
    fn process_state(&self, process: ProcessIdentity, deadline: Deadline) -> Result<ProcessState, AdapterError> {
        self.inner.process_state(process, deadline)
    }
    fn supported_surfaces(&self) -> Vec<SnapshotSurface> {
        self.inner.supported_surfaces()
    }
    fn check_surface_support(&self, requested: SnapshotSurface) -> Result<(), AdapterError> {
        self.inner.check_surface_support(requested)
    }
    fn subscribe_wait(&self, filter: &SignalFilter, deadline: Deadline) -> Result<Option<Box<dyn WaitWakeup>>, AdapterError> {
        self.inner.subscribe_wait(filter, deadline)
    }
    fn open_system_surface(&self, surface: SnapshotSurface, policy: InteractionPolicy, lease: &InteractionLease) -> Result<WindowInfo, AdapterError> {
        let win = self.inner.open_system_surface(surface, policy, lease)?;
        self.check_window("open_system_surface", &win)?;
        Ok(win)
    }
    fn open_session(&self, affinity: &SessionAffinity, deadline: Deadline) -> Result<Box<dyn AdapterSession>, AdapterError> {
        self.inner.open_session(affinity, deadline)
    }
    fn capture_signal_baseline(&self, filter: &SignalFilter, deadline: Deadline) -> Result<SignalBaseline, AdapterError> {
        self.inner.capture_signal_baseline(filter, deadline)
    }
    fn close_app(&self, app: &AppInfo, force: bool, lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.check_pid("close_app", app.pid.get())?;
        self.inner.close_app(app, force, lease)
    }
    fn is_protected_process(&self, identifier: &str) -> bool {
        self.inner.is_protected_process(identifier)
    }
    fn is_blocked_combo(&self, combo: &KeyCombo) -> bool {
        self.inner.is_blocked_combo(combo)
    }
    fn screenshot(&self, target: ScreenshotTarget, deadline: Deadline) -> Result<ImageBuffer, AdapterError> {
        if let ScreenshotTarget::ExactWindow(win) = &target {
            self.check_window("screenshot", win)?;
        }
        let area = self.capture_rect(&target, deadline);
        let before = self.fenced_rects().ok_or_else(|| fenced_error("screenshot", Rule::UnknownIdentity, "window_list"))?;
        let glow = crate::glow::hide_for_capture();
        let image = self.inner.screenshot(target, deadline)?;
        let image = mask_glow(image, area, &glow.strips_to_mask())?;
        drop(glow);
        self.redact(image, area, before, "screenshot")
    }
    fn screenshot_window_frame(&self, window: &WindowInfo, deadline: Deadline) -> Result<ImageBuffer, AdapterError> {
        self.check_window("screenshot", window)?;
        let area = self.probe.window(&window.id).and_then(|w| w.rect);
        let before = self.fenced_rects().ok_or_else(|| fenced_error("screenshot", Rule::UnknownIdentity, "window_list"))?;
        let glow = crate::glow::hide_for_capture();
        let image = self.inner.screenshot_window_frame(window, deadline)?;
        let image = mask_glow(image, area, &glow.strips_to_mask())?;
        drop(glow);
        self.check_window("screenshot", window)?;
        self.redact(image, area, before, "screenshot")
    }
    fn list_displays(&self, deadline: Deadline) -> Result<Vec<DisplayInfo>, AdapterError> {
        self.inner.list_displays(deadline)
    }
    fn focused_window(&self, deadline: Deadline) -> Result<Option<WindowInfo>, AdapterError> {
        let win = self.inner.focused_window(deadline)?;
        if let Some(w) = &win {
            self.check_window("focused_window", w)?;
        }
        Ok(win)
    }
    fn press_key_for_app(&self, process: ProcessIdentity, combo: &KeyCombo, policy: InteractionPolicy, lease: &InteractionLease) -> Result<ActionResult, AdapterError> {
        self.check_pid("press_key", process.pid.get())?;
        self.check_live("press_key", self.probe.foreground())?;
        crate::focus_guard::note_tool_input();
        self.inner.press_key_for_app(process, combo, policy, lease)
    }
    fn press_key_for_window(&self, window: &WindowInfo, combo: &KeyCombo, policy: InteractionPolicy, lease: &InteractionLease) -> Result<ActionResult, AdapterError> {
        self.check_window("press_key", window)?;
        self.check_live("press_key", self.probe.foreground())?;
        crate::focus_guard::note_tool_input();
        self.inner.press_key_for_window(window, combo, policy, lease)
    }
    fn wait_for_menu(&self, process: ProcessIdentity, open: bool, deadline: Deadline) -> Result<(), AdapterError> {
        self.check_pid("wait_for_menu", process.pid.get())?;
        self.inner.wait_for_menu(process, open, deadline)
    }
    fn window_op(&self, win: &WindowInfo, op: WindowOp, lease: &InteractionLease) -> Result<OperationReport, AdapterError> {
        self.check_window("window_op", win)?;
        self.inner.window_op(win, op, lease)
    }
    fn resolve_window_strict(&self, win: &WindowInfo, deadline: Deadline) -> Result<WindowInfo, AdapterError> {
        self.check_window("resolve_window", win)?;
        let live = self.inner.resolve_window_strict(win, deadline)?;
        self.check_window("resolve_window", &live)?;
        Ok(live)
    }
    fn check_notification_support(&self, deadline: Deadline) -> Result<(), AdapterError> {
        self.inner.check_notification_support(deadline)
    }
    fn list_notifications(&self, filter: &NotificationFilter, policy: InteractionPolicy, deadline: Deadline, lease: Option<&InteractionLease>) -> Result<Vec<NotificationInfo>, AdapterError> {
        let mut list = self.inner.list_notifications(filter, policy, deadline, lease)?;
        list.retain(|n| !self.notification_fenced(n));
        Ok(list)
    }
    fn dismiss_notification(&self, request: DismissNotificationRequest<'_>, lease: &InteractionLease) -> Result<NotificationInfo, AdapterError> {
        self.check_notification_target("dismiss_notification", request.index, request.identity, request.policy, lease)?;
        let info = self.inner.dismiss_notification(request, lease)?;
        if self.notification_fenced(&info) {
            return Err(fenced_error("dismiss_notification", Rule::ProcessName, "notification"));
        }
        Ok(info)
    }
    fn dismiss_all_notifications(&self, request: DismissAllNotificationsRequest<'_>, lease: &InteractionLease) -> Result<(Vec<NotificationInfo>, Vec<String>), AdapterError> {
        let (mut list, rest) = self.inner.dismiss_all_notifications(request, lease)?;
        list.retain(|n| !self.notification_fenced(n));
        Ok((list, rest))
    }
    fn notification_action(&self, request: NotificationActionRequest<'_>, lease: &InteractionLease) -> Result<ActionResult, AdapterError> {
        self.check_notification_target("notification_action", request.index, request.identity, request.policy, lease)?;
        self.inner.notification_action(request, lease)
    }
}

#[cfg(test)]
#[path = "adapter_tests.rs"]
mod tests;
