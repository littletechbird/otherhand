//! Hostile-agent tests: each one is an attempt to get past the fence, the
//! redaction, the secret filler or the focus guard. Every test names the
//! attack it tries. All of them run without a desktop.

use super::adapter::FencedAdapter;
use super::policy::{Fence, FenceFile, ProcessFacts, Rule, WindowFacts, normalize_path};
use super::probe::{ClipboardFacts, LiveWindow, Probe, ScreenRect};
use agent_desktop_core::{
    ActionOps, AdapterError, ClipboardContent, ClipboardFormat, Deadline, ErrorCode, ImageBuffer, ImageFormat,
    InputOps, NotificationFilter, NotificationInfo, ObservationOps, ProcessId, ScreenshotTarget, SystemOps,
    WindowFilter, WindowInfo, WindowState, InteractionPolicy,
    system::{launch_options::LaunchOptions, launch_result::LaunchResult},
};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

const PRIVATE: &str = "C:\\Users\\alex\\Private";

fn fence() -> Fence {
    let file: FenceFile = serde_json::from_str(
        r#"{"version":1,"programs":{"process_names":["MyBank.exe"]},"paths":["C:\\Users\\alex\\Private","\\\\nas\\finance"]}"#,
    )
    .unwrap();
    Fence::new(&file, &["C:\\Users\\alex\\.agent-desktop\\fence.json".into(), "C:\\Users\\alex\\.darkroom".into()])
}

fn live(id: &str, pid: u32, exe: &str, class: &str, rect: (i32, i32, i32, i32)) -> LiveWindow {
    LiveWindow {
        id: id.into(),
        pid,
        facts: WindowFacts {
            class: class.into(),
            title: "t".into(),
            process: ProcessFacts { image_path: Some(exe.into()), image_name: Some(super::policy::file_name(exe)) },
            folder: if super::policy::is_explorer_class(class) { Some("C:\\Windows".into()) } else { None },
        },
        rect: Some(ScreenRect { left: rect.0, top: rect.1, right: rect.2, bottom: rect.3 }),
        visible: true,
        minimized: false,
        occluder: true,
    }
}

/// A probe whose window list can change between calls, to race captures.
struct HostileProbe {
    lists: Mutex<Vec<Vec<LiveWindow>>>,
    calls: AtomicUsize,
    clipboard: Option<ClipboardFacts>,
    /// Paths the fake file system resolves, like a junction or 8.3 name.
    links: Vec<(&'static str, &'static str)>,
}

impl HostileProbe {
    fn steady(windows: Vec<LiveWindow>) -> Self {
        Self { lists: Mutex::new(vec![windows]), calls: AtomicUsize::new(0), clipboard: Some(ClipboardFacts::default()), links: vec![] }
    }
    fn current(&self) -> Vec<LiveWindow> {
        let lists = self.lists.lock().unwrap();
        lists[self.calls.load(Ordering::SeqCst).min(lists.len() - 1)].clone()
    }
}

impl Probe for HostileProbe {
    fn process(&self, pid: u32) -> ProcessFacts {
        self.current().into_iter().find(|w| w.pid == pid).map(|w| w.facts.process).unwrap_or(ProcessFacts {
            image_path: Some(format!("C:\\Windows\\p{pid}.exe")),
            image_name: Some(format!("p{pid}.exe")),
        })
    }
    fn window(&self, id: &str) -> Option<LiveWindow> {
        self.current().into_iter().find(|w| w.id == id)
    }
    fn top_level_windows(&self) -> Option<Vec<LiveWindow>> {
        let w = self.current();
        self.calls.fetch_add(1, Ordering::SeqCst);
        Some(w)
    }
    fn foreground(&self) -> Option<LiveWindow> {
        self.current().into_iter().next()
    }
    fn window_at(&self, x: i32, y: i32) -> Option<LiveWindow> {
        self.current().into_iter().find(|w| w.rect.is_some_and(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom))
    }
    fn resolve_path(&self, path: &str) -> String {
        let n = normalize_path(path);
        for (from, to) in &self.links {
            let f = normalize_path(from);
            if super::policy::path_under(&n, &f) {
                return format!("{}{}", normalize_path(to), &n[f.len()..]);
            }
        }
        path.to_string()
    }
    fn display_rect(&self, _index: usize) -> Option<ScreenRect> {
        None
    }
    fn virtual_screen(&self) -> Option<ScreenRect> {
        Some(ScreenRect { left: 0, top: 0, right: 100, bottom: 100 })
    }
    fn clipboard_facts(&self) -> Option<ClipboardFacts> {
        self.clipboard.clone()
    }
}

fn win_info(id: &str, pid: u32) -> WindowInfo {
    WindowInfo {
        id: id.into(),
        title: "t".into(),
        app: "a".into(),
        pid: ProcessId::new(pid),
        process_instance: None,
        bounds: None,
        state: WindowState { is_focused: false, accessible: true, minimized: None, visible: Some(true) },
    }
}

/// Stands in for the real adapter. Launches report a fixed pid so a name
/// that secretly starts a fenced program can be caught after the fact.
struct Inner {
    launched_pid: u32,
    clipboard_text: String,
    notifications: Vec<NotificationInfo>,
}
impl Default for Inner {
    fn default() -> Self {
        Self { launched_pid: 10, clipboard_text: "hello".into(), notifications: vec![] }
    }
}
impl ObservationOps for Inner {
    fn list_windows(&self, _f: &WindowFilter, _d: Deadline) -> Result<Vec<WindowInfo>, AdapterError> {
        Ok(vec![])
    }
}
impl ActionOps for Inner {}
impl InputOps for Inner {
    fn get_clipboard_content(&self, _f: ClipboardFormat, _d: Deadline) -> Result<Option<ClipboardContent>, AdapterError> {
        Ok(Some(ClipboardContent::Text(self.clipboard_text.clone())))
    }
}
impl SystemOps for Inner {
    fn screenshot(&self, _t: ScreenshotTarget, _d: Deadline) -> Result<ImageBuffer, AdapterError> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 100, 100);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(&vec![255u8; 100 * 100 * 4]).unwrap();
        drop(w);
        Ok(ImageBuffer { data: out, format: ImageFormat::Png, width: 100, height: 100, scale_factor: 1.0 })
    }
    fn launch_app(&self, id: &str, _o: &LaunchOptions, _l: &agent_desktop_core::InteractionLease) -> Result<LaunchResult, AdapterError> {
        Ok(LaunchResult { app: id.into(), pid: ProcessId::new(self.launched_pid), process_instance: None, window: None, cdp: None, renderer: None, suggestion: None })
    }
    fn list_notifications(&self, _f: &NotificationFilter, _p: InteractionPolicy, _d: Deadline, _l: Option<&agent_desktop_core::InteractionLease>) -> Result<Vec<NotificationInfo>, AdapterError> {
        Ok(self.notifications.clone())
    }
}

fn lease() -> agent_desktop_core::InteractionLease {
    agent_desktop_core::InteractionLease::guarded(deadline(), ()).unwrap()
}

fn deadline() -> Deadline {
    Deadline::after(5_000).unwrap()
}

fn notepad() -> LiveWindow {
    live("w-1", 10, "C:\\Windows\\notepad.exe", "Notepad", (0, 0, 50, 50))
}
fn bank() -> LiveWindow {
    live("w-2", 20, "C:\\Apps\\MyBank.exe", "BankFrame", (50, 50, 80, 80))
}

fn adapter(inner: &Inner, probe: HostileProbe) -> FencedAdapter<'_> {
    FencedAdapter::new(inner, fence(), Box::new(probe))
}

fn fenced<T: std::fmt::Debug>(r: Result<T, AdapterError>) -> bool {
    matches!(r, Err(ref e) if e.code == ErrorCode::Fenced)
}

// ---------- (a) path tricks ----------

#[test]
fn attack_path_spellings_all_resolve_to_the_fenced_root() {
    let f = fence();
    for p in [
        "C:\\Users\\alex\\Private::$INDEX_ALLOCATION",
        "C:\\Users\\alex\\.agent-desktop\\fence.json::$DATA",
        "C:\\Users\\alex\\.agent-desktop\\fence.json:hidden",
        "C:\\Users\\alex\\Private.\\x.txt",
        "C:\\Users\\alex\\Private \\x.txt",
        "C:\\Users\\alex\\Private. . .\\x.txt",
        "C:\\Users\\alex\\.agent-desktop\\fence.json.",
        "\\\\?\\C:\\Users\\alex\\Private\\x",
        "\\\\?\\UNC\\nas\\finance\\x",
        "\\\\.\\C:\\Users\\alex\\Private",
        "\\\\localhost\\C$\\Users\\alex\\Private\\x",
        "\\\\127.0.0.1\\c$\\Users\\alex\\Private",
        "\\\\[::1]\\c$\\Users\\alex\\Private",
        "\\\\?\\UNC\\localhost\\c$\\Users\\alex\\Private",
        "c:\\USERS\\ALEX\\private\\X",
        "C:\\Users\\alex\\Public\\..\\Private\\x",
        "C:\\..\\..\\Users\\alex\\Private",
        "C:/Users/alex/Private/x",
        "C:\\\\Users\\\\alex\\\\Private",
        "\"C:\\Users\\alex\\Private\"",
        "C:\\Users\\alex\\.darkroom\\machine.key",
    ] {
        assert!(f.path_is_fenced(p), "{p} got through");
    }
    for p in ["C:\\Users\\alex\\PrivateStuff\\x", "C:\\Users\\alex\\Private2", "\\\\otherhost\\c$\\Users\\alex\\Private", "C:\\Users\\alex\\Private.txt"] {
        assert!(!f.path_is_fenced(p), "{p} was over-blocked");
    }
}

#[test]
fn attack_typed_text_hides_a_fenced_path() {
    let inner = Inner::default();
    let a = adapter(&inner, HostileProbe::steady(vec![notepad()]));
    let home = super::env_lookup("USERPROFILE");
    for t in [
        "open C:\\Users\\alex\\Private:x now",
        "file:///C:/Users/alex/Priv%61te/x.txt",
        "file:///C:/Users/alex/Private/x.txt",
        "..\\Private\\canary.txt",
        "Private\\canary.txt",
        "\\\\localhost\\c$\\Users\\alex\\Private\\x",
        "C:\\Users\\alex\\Private.\\x.txt",
        "C:\\Users\\alex\\Desktop\\bank.lnk",
        "C:\\Users\\alex\\Desktop\\bank.lnk.",
    ] {
        assert!(fenced(a.check_text("type", t)), "{t} got through");
    }
    if home.is_some_and(|h| h.contains("alex")) {
        assert!(fenced(a.check_text("type", "%USERPROFILE%\\Private\\x.txt")));
    }
    assert!(a.check_text("type", "meet at the private dining room at 7").is_ok());
    assert!(a.check_text("type", "C:\\Users\\alex\\Documents\\notes.txt").is_ok());
}

#[test]
fn attack_env_variable_names_a_fenced_folder() {
    let lookup = |n: &str| (n == "SECRETDIR").then(|| PRIVATE.to_string());
    let t = super::policy::expand_env("%SECRETDIR%\\x.txt and %UNKNOWN% and 100%", &lookup);
    assert_eq!(t, "C:\\Users\\alex\\Private\\x.txt and %UNKNOWN% and 100%");
    assert!(fence().path_is_fenced(&t[..t.find(" and").unwrap()]));
}

#[test]
fn attack_junction_8dot3_and_subst_spellings_are_resolved_before_the_check() {
    let inner = Inner::default();
    let mut probe = HostileProbe::steady(vec![notepad()]);
    probe.links = vec![("C:\\Temp\\innocent link", PRIVATE), ("C:\\Users\\alex\\PRIVAT~1", PRIVATE), ("X:\\", PRIVATE)];
    let a = adapter(&inner, probe);
    for t in ["C:\\Temp\\innocent link\\x.txt", "C:\\Users\\alex\\PRIVAT~1\\x.txt", "X:\\x.txt"] {
        assert!(fenced(a.check_text("type", t)), "{t} got through");
        assert!(fenced(a.check_path("launch_app", t)), "{t} got through as a path");
    }
}

// ---------- (a) process tricks ----------

#[test]
fn attack_launch_name_spellings() {
    let inner = Inner::default();
    let a = adapter(&inner, HostileProbe::steady(vec![notepad()]));
    let lease = lease();
    for id in ["MyBank.exe.", "MyBank.exe ", "\"MyBank.exe\"", "MyBank.exe::$DATA", "MYBANK", "C:\\Apps\\.\\MyBank.exe", "KeePass", "keepass.EXE"] {
        assert!(fenced(a.launch_app(id, &LaunchOptions::default(), &lease)), "{id} got through");
    }
}

#[test]
fn attack_launch_arguments_env_and_cwd() {
    let inner = Inner::default();
    let a = adapter(&inner, HostileProbe::steady(vec![notepad()]));
    let lease = lease();
    let with = |args: &[&str], cwd: Option<&str>, env: &[(&str, &str)]| LaunchOptions {
        args: args.iter().map(|s| s.to_string()).collect(),
        cwd: cwd.map(std::path::PathBuf::from),
        env: env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        ..Default::default()
    };
    assert!(fenced(a.launch_app("notepad", &with(&["canary.txt"], Some(PRIVATE), &[]), &lease)), "cwd inside the fence");
    assert!(fenced(a.launch_app("notepad", &with(&["Private\\canary.txt"], Some("C:\\Users\\alex"), &[]), &lease)), "relative arg");
    assert!(fenced(a.launch_app("cmd", &with(&["/c", "type C:\\Users\\alex\\Private\\x"], None, &[]), &lease)), "path inside a shell line");
    assert!(fenced(a.launch_app("notepad", &with(&[], None, &[("HOME", PRIVATE)]), &lease)), "env value");
    assert!(fenced(a.launch_app("C:\\Users\\alex\\Desktop\\bank.lnk", &with(&[], None, &[]), &lease)), "shortcut");
    assert!(a.launch_app("notepad", &with(&["C:\\Users\\alex\\Documents\\a.txt"], None, &[]), &lease).is_ok());
}

#[test]
fn attack_innocent_name_that_starts_a_fenced_program() {
    // An App Paths alias or protocol handler: "budget" really starts MyBank.
    let inner = Inner { launched_pid: 20, ..Default::default() };
    let a = adapter(&inner, HostileProbe::steady(vec![notepad(), bank()]));
    let lease = lease();
    assert!(fenced(a.launch_app("budget", &LaunchOptions::default(), &lease)));
}

#[test]
fn attack_renamed_password_manager_still_matches_by_path_rules_only() {
    // Known limit, kept visible: a renamed copy outside every fenced path
    // is not matched by name. exe_paths and folder roots are the strong rules.
    let f = fence();
    let renamed = ProcessFacts { image_path: Some("C:\\Temp\\notes.exe".into()), image_name: Some("notes.exe".into()) };
    assert_eq!(f.match_process(&renamed), None);
    let inside = ProcessFacts { image_path: Some("C:\\Users\\alex\\Private\\notes.exe".into()), image_name: Some("notes.exe".into()) };
    assert_eq!(f.match_process(&inside), Some(Rule::Path));
}

#[test]
fn attack_title_and_class_spoofing_can_only_add_protection() {
    let f = fence();
    let spoof = WindowFacts { class: "Notepad".into(), title: "Windows Security".into(), process: ProcessFacts { image_path: Some("C:\\Temp\\x.exe".into()), image_name: Some("x.exe".into()) }, folder: None };
    assert!(f.match_window(&spoof).is_some(), "a lookalike title is fenced, not trusted");
    let unknown = WindowFacts { class: "X".into(), title: "x".into(), process: ProcessFacts::default(), folder: None };
    assert_eq!(f.match_window(&unknown), Some(Rule::UnknownIdentity), "unreadable owner fails closed");
}

#[test]
fn attack_explorer_or_file_dialog_on_a_fenced_folder() {
    let f = fence();
    let explorer = |folder: Option<&str>| WindowFacts {
        class: "CabinetWClass".into(),
        title: "Windows".into(),
        process: ProcessFacts { image_path: Some("C:\\Windows\\explorer.exe".into()), image_name: Some("explorer.exe".into()) },
        folder: folder.map(String::from),
    };
    assert!(f.match_window(&explorer(Some(PRIVATE))).is_some());
    assert!(f.match_window(&explorer(None)).is_some(), "unknown Explorer folder fails closed");
    assert!(f.match_window(&explorer(Some("C:\\Windows"))).is_none());
    let dialog = WindowFacts { class: "#32770".into(), title: "Open".into(), process: ProcessFacts { image_path: Some("C:\\Windows\\notepad.exe".into()), image_name: Some("notepad.exe".into()) }, folder: Some(format!("{PRIVATE}\\sub")) };
    assert!(f.match_window(&dialog).is_some(), "an Open dialog showing a fenced folder");
}

// ---------- (b) screenshots and trees ----------

fn black_at(img: &ImageBuffer, x: usize, y: usize) -> bool {
    let mut r = png::Decoder::new(std::io::Cursor::new(&img.data)).read_info().unwrap();
    let mut buf = vec![0u8; r.output_buffer_size().unwrap()];
    r.next_frame(&mut buf).unwrap();
    let i = (y * 100 + x) * 4;
    buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 0
}

#[test]
fn attack_fenced_window_closes_or_moves_during_the_capture() {
    // Listed before the capture, gone from the list after it.
    let inner = Inner::default();
    let probe = HostileProbe { lists: Mutex::new(vec![vec![notepad(), bank()], vec![notepad()]]), ..HostileProbe::steady(vec![]) };
    let a = adapter(&inner, probe);
    let img = a.screenshot(ScreenshotTarget::FullScreen, deadline()).unwrap();
    assert!(black_at(&img, 65, 65), "window that left after the capture is still black");
    // Appears only after the capture started.
    let inner = Inner::default();
    let probe = HostileProbe { lists: Mutex::new(vec![vec![notepad()], vec![notepad(), bank()]]), ..HostileProbe::steady(vec![]) };
    let a = adapter(&inner, probe);
    let img = a.screenshot(ScreenshotTarget::FullScreen, deadline()).unwrap();
    assert!(black_at(&img, 65, 65), "window that arrived during the capture is black");
    assert!(!black_at(&img, 10, 10));
}

#[test]
fn attack_minimized_fenced_window_restored_mid_capture() {
    let inner = Inner::default();
    let mut min = bank();
    min.minimized = true;
    let probe = HostileProbe { lists: Mutex::new(vec![vec![notepad(), min], vec![notepad(), bank()]]), ..HostileProbe::steady(vec![]) };
    let a = adapter(&inner, probe);
    let img = a.screenshot(ScreenshotTarget::FullScreen, deadline()).unwrap();
    assert!(black_at(&img, 65, 65));
}

#[test]
fn attack_overlap_and_second_monitor_geometry() {
    // Fenced window under a normal one and partly off the left screen edge.
    let area = ScreenRect { left: -1920, top: 0, right: 0, bottom: 1080 };
    let fenced = ScreenRect { left: -2000, top: 100, right: -1000, bottom: 600 };
    let clipped = fenced.intersect(&area).unwrap();
    let boxes = super::redact::map_to_image(&[clipped], area, 3840, 2160);
    assert_eq!(boxes.len(), 1);
    let b = boxes[0];
    assert!(b.x0 == 0 && b.x1 >= 1840 && b.y0 <= 200 && b.y1 >= 1200, "{b:?}");
}

#[test]
fn attack_capture_with_unreadable_window_list_or_bad_image_is_refused() {
    struct Blind;
    impl Probe for Blind {
        fn process(&self, _p: u32) -> ProcessFacts { ProcessFacts::default() }
        fn window(&self, _id: &str) -> Option<LiveWindow> { None }
        fn top_level_windows(&self) -> Option<Vec<LiveWindow>> { None }
        fn foreground(&self) -> Option<LiveWindow> { None }
        fn window_at(&self, _x: i32, _y: i32) -> Option<LiveWindow> { None }
        fn resolve_path(&self, p: &str) -> String { p.into() }
        fn display_rect(&self, _i: usize) -> Option<ScreenRect> { None }
        fn virtual_screen(&self) -> Option<ScreenRect> { None }
    }
    let inner = Inner::default();
    let a = FencedAdapter::new(&inner, fence(), Box::new(Blind));
    assert!(fenced(a.screenshot(ScreenshotTarget::FullScreen, deadline())));
    let jpg = ImageBuffer { data: vec![1, 2, 3], format: ImageFormat::Jpg, width: 1, height: 1, scale_factor: 1.0 };
    assert!(super::redact::black_out(jpg, &[]).is_err());
    let truncated = ImageBuffer { data: vec![0x89, b'P', b'N', b'G'], format: ImageFormat::Png, width: 1, height: 1, scale_factor: 1.0 };
    assert!(super::redact::black_out(truncated, &[]).is_err());
}

#[test]
fn attack_tree_and_pointer_reads_of_a_fenced_window() {
    let inner = Inner::default();
    let a = adapter(&inner, HostileProbe::steady(vec![notepad(), bank()]));
    assert!(fenced(a.get_tree(&win_info("w-2", 20), &Default::default(), deadline())));
    assert!(fenced(a.get_tree(&win_info("w-1", 20), &Default::default(), deadline())), "allowed window id, fenced pid");
    let ev = agent_desktop_core::MouseEvent { point: agent_desktop_core::Point { x: 60.0, y: 60.0 }, kind: agent_desktop_core::MouseEventKind::Move, button: agent_desktop_core::MouseButton::Left, modifiers: vec![] };
    assert!(fenced(a.mouse_event(ev, &lease())), "pointer over the fenced window");
}

// ---------- clipboard and notifications ----------

#[test]
fn attack_read_a_password_manager_copy_from_the_clipboard() {
    let inner = Inner::default();
    let mut p = HostileProbe::steady(vec![notepad(), bank()]);
    p.clipboard = Some(ClipboardFacts { owner_pid: None, marked_sensitive: true });
    assert!(fenced(adapter(&inner, p).get_clipboard_content(ClipboardFormat::Text, deadline())), "marked sensitive");
    let mut p = HostileProbe::steady(vec![notepad(), bank()]);
    p.clipboard = Some(ClipboardFacts { owner_pid: Some(20), marked_sensitive: false });
    assert!(fenced(adapter(&inner, p).get_clipboard_content(ClipboardFormat::Text, deadline())), "owned by a fenced app");
    let mut p = HostileProbe::steady(vec![notepad()]);
    p.clipboard = None;
    assert!(fenced(adapter(&inner, p).get_clipboard_content(ClipboardFormat::Text, deadline())), "unknown owner");
    let inner = Inner { clipboard_text: format!("{PRIVATE}\\statement.pdf"), ..Default::default() };
    assert!(fenced(adapter(&inner, HostileProbe::steady(vec![notepad()])).get_clipboard_content(ClipboardFormat::Text, deadline())), "fenced path on the clipboard");
    let inner = Inner::default();
    assert!(adapter(&inner, HostileProbe::steady(vec![notepad()])).get_clipboard_content(ClipboardFormat::Text, deadline()).is_ok());
    let files = ClipboardContent::FileUrls(vec![format!("file:///{}/x.pdf", PRIVATE.replace('\\', "/"))]);
    assert!(fenced(adapter(&inner, HostileProbe::steady(vec![notepad()])).set_clipboard_content(&files, &lease())));
}

#[test]
fn attack_read_a_fenced_app_through_its_notifications() {
    let n = |app: &str, title: &str, body: &str| NotificationInfo { index: 0, app_name: app.into(), title: title.into(), body: Some(body.into()), actions: vec![] };
    let inner = Inner {
        notifications: vec![n("MyBank", "Your code is 123456", ""), n("KeePass", "Copied", ""), n("Mail", "Saved", &format!("{PRIVATE}\\tax.pdf")), n("Mail", "Lunch?", "noon")],
        ..Default::default()
    };
    let a = adapter(&inner, HostileProbe::steady(vec![notepad()]));
    let list = a.list_notifications(&NotificationFilter::default(), InteractionPolicy::default(), deadline(), None).unwrap();
    assert_eq!(list.iter().map(|n| n.title.as_str()).collect::<Vec<_>>(), vec!["Lunch?"]);
}

#[test]
fn attack_press_a_button_on_a_fenced_apps_toast_by_index() {
    let inner = Inner {
        notifications: vec![NotificationInfo { index: 0, app_name: "MyBank".into(), title: "Code".into(), body: None, actions: vec!["Copy".into()] }],
        ..Default::default()
    };
    let a = adapter(&inner, HostileProbe::steady(vec![notepad()]));
    let identity = agent_desktop_core::NotificationIdentity::default();
    let req = agent_desktop_core::NotificationActionRequest { index: 0, identity: &identity, action_name: "Copy", policy: InteractionPolicy::default() };
    assert!(fenced(a.notification_action(req, &lease())));
}

// ---------- (e) the fence file and the floor ----------

#[test]
fn attack_point_the_fence_somewhere_else_or_at_nothing() {
    let home = std::path::PathBuf::from("/home/alex");
    let s = super::fence_sources_from(Some("/tmp/empty.json".into()), Some(home.clone()));
    assert_eq!(s[0].path, home.join(".agent-desktop").join("fence.json"), "the user's own file is still read");
    assert!(s[1].required, "an override must exist");
    let dir = std::env::temp_dir().join(format!("nah-attack-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".agent-desktop")).unwrap();
    std::fs::write(dir.join(".agent-desktop").join("fence.json"), r#"{"version":1,"programs":{"process_names":["MyBank.exe"]}}"#).unwrap();
    let probe = HostileProbe::steady(vec![]);
    let sources = super::fence_sources_from(Some(dir.join("missing.json").into_os_string()), Some(dir.clone()));
    assert!(fenced(super::load_from(&probe, &sources, &[], Some(dir.clone()))), "missing override fails closed");
    std::fs::write(dir.join("empty.json"), "{}").unwrap();
    let sources = super::fence_sources_from(Some(dir.join("empty.json").into_os_string()), Some(dir.clone()));
    let f = super::load_from(&probe, &sources, &[], Some(dir.clone())).unwrap();
    let bank = ProcessFacts { image_path: Some("C:\\Apps\\MyBank.exe".into()), image_name: Some("MyBank.exe".into()) };
    assert!(f.match_process(&bank).is_some(), "an empty override can't drop the user's rules");
    assert!(f.path_is_fenced(dir.join(".agent-desktop").to_str().unwrap()), "the fence folder itself is fenced");
    for bad in [r#"{"version":1,"floor":[]}"#, r#"{"version":2}"#, r#"{"version":1,"programs":{"disable_floor":true}}"#, "not json"] {
        std::fs::write(dir.join(".agent-desktop").join("fence.json"), bad).unwrap();
        let sources = super::fence_sources_from(None, Some(dir.clone()));
        assert!(fenced(super::load_from(&probe, &sources, &[], Some(dir.clone()))), "{bad} must fail closed");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn attack_blank_profile_variable_hides_the_home_folder() {
    let h = super::audit::home_from_env(Some("".into()), Some("/home/alex".into()));
    assert_eq!(h, Some(std::path::PathBuf::from("/home/alex")));
    assert_eq!(super::audit::home_from_env(Some("".into()), Some("".into())), None);
}

#[test]
fn attack_floor_survives_any_fence_file() {
    let f = Fence::new(&FenceFile::default(), &[]);
    for exe in ["keepass.exe", "1password.exe", "bitwarden.exe", "consent.exe", "credentialuibroker.exe", "lockapp.exe"] {
        let facts = ProcessFacts { image_path: Some(format!("C:\\x\\{exe}")), image_name: Some(exe.into()) };
        assert!(f.match_process(&facts).is_some(), "{exe}");
    }
}

#[test]
fn attack_move_the_vault_or_the_audit_log_with_darkroom_home() {
    let home = std::path::PathBuf::from("/home/alex");
    let dirs = super::audit::darkroom_dirs_from(Some("/tmp/fake vault".into()), Some(home.clone()));
    assert_eq!(dirs[0], home.join(".darkroom"), "real vault stays protected and keeps its audit log");
    assert_eq!(dirs[1], std::path::PathBuf::from("/tmp/fake vault"));
}

// ---------- (c) secrets ----------

#[test]
fn attack_lookalike_and_downgraded_origins() {
    use crate::secret::origin::{page_origin, web_origin};
    assert_eq!(web_origin("https://example.com/login").as_deref(), Some("https://example.com"));
    assert_eq!(web_origin("https://example.com:443/").as_deref(), Some("https://example.com"));
    assert_eq!(web_origin("https://example.com./").as_deref(), Some("https://example.com"));
    for (url, why) in [
        ("example.com/login", "no scheme"),
        ("https://xn--exmple-cua.com/", "punycode"),
        ("https://exаmple.com/", "cyrillic a"),
        ("https://example.com\\@evil.test/", "backslash authority"),
        ("javascript:alert(1)", "script"),
        ("data:text/html,x", "data"),
    ] {
        assert_eq!(web_origin(url), None, "{why}");
    }
    assert_eq!(web_origin("https://example.com@evil.test/").as_deref(), Some("https://evil.test"));
    assert_eq!(web_origin("https://example.com.evil.test/").as_deref(), Some("https://example.com.evil.test"));
    assert_eq!(web_origin("http://example.com/").as_deref(), Some("http://example.com"), "http stays http");
    assert_eq!(web_origin("https://example.com:8443/").as_deref(), Some("https://example.com:8443"));
    assert_eq!(page_origin(Some("https://evil.test/"), Some("https://example.com/")), None, "address bar typed over");
    assert_eq!(page_origin(Some("https://example.com/a"), Some("example.com/a")).as_deref(), Some("https://example.com"));
    assert_eq!(page_origin(None, Some("https://example.com/")), None, "no page URL, no fill");
    use crate::secret::vault::origin_allowed;
    let allow = vec!["https://example.com".to_string()];
    for o in ["https://example.com.evil.test", "http://example.com", "https://evil.test", "https://example.com:8443", "https://sub.example.com"] {
        assert!(!origin_allowed(&allow, &[o.to_string()]), "{o}");
    }
}

#[test]
fn attack_renamed_copy_claims_a_native_handle() {
    use crate::secret::origin::native_origins;
    let fake = ProcessFacts { image_path: Some("C:\\Users\\alex\\Downloads\\notepad.exe".into()), image_name: Some("notepad.exe".into()) };
    assert!(!native_origins(&fake).contains(&"app://notepad.exe".to_string()));
    let real = ProcessFacts { image_path: Some("C:\\Windows\\System32\\notepad.exe".into()), image_name: Some("notepad.exe".into()) };
    assert!(native_origins(&real).contains(&"app://notepad.exe".to_string()));
    let tricky = ProcessFacts { image_path: Some("C:\\Windows.\\..\\Temp\\notepad.exe".into()), image_name: Some("notepad.exe".into()) };
    assert!(!native_origins(&tricky).contains(&"app://notepad.exe".to_string()), "dot tricks don't reach the trusted folder");
    let no_path = ProcessFacts { image_path: None, image_name: Some("notepad.exe".into()) };
    assert!(native_origins(&no_path).is_empty(), "unknown image path gets no origin");
}

#[test]
fn attack_secret_never_reaches_audit_or_errors() {
    use super::audit::record;
    for key in ["secret", "plaintext", "password", "token_value", "value", "title", "text"] {
        assert!(record("fill_ok", &[(key, "CANARY-not-a-secret-7f3a")]).is_none(), "{key}");
    }
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("secret").join("fixtures");
    let handles: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("handles.json")).unwrap()).unwrap();
    for (name, origin) in [("web", "app://notepad.exe"), ("open", "app://notepad.exe"), ("revoked", "app://notepad.exe")] {
        let err = crate::secret::vault::resolve_entry(&dir, handles[name].as_str().unwrap(), &[origin.into()]).err().unwrap();
        assert!(!format!("{err:?}").contains("CANARY"), "{name}");
    }
    let ok = crate::secret::vault::resolve_entry(&dir, handles["notepad"].as_str().unwrap(), &["app://notepad.exe".into()]).ok().unwrap();
    assert!(ok.fill_visible, "only the notepad canary is allowed into visible fields");
    let pw = crate::secret::vault::resolve_entry(&dir, handles["password_only"].as_str().unwrap(), &["app://notepad.exe".into()]).ok().unwrap();
    assert!(!pw.fill_visible, "a handle is password-field only unless the person says otherwise");
    let debug = format!("{:?}", ok.plain.as_str().len());
    assert!(!debug.contains("CANARY"));
}

// ---------- (d) focus guard ----------

#[test]
fn attack_steal_focus_and_put_it_back_before_the_check() {
    use crate::focus_guard::{Observation, fold};
    let o = |fg: isize, x: i32, tick: u32| Observation { foreground: fg, cursor: (x, 0), last_input_tick: tick };
    let c = fold(&o(42, 1, 7), &[o(99, 1, 7), o(42, 1, 7)], &o(42, 1, 7), false);
    assert!(c.stolen() && c.foreground_changed, "steal and restore inside one command");
    let c = fold(&o(42, 1, 7), &[o(42, 300, 7), o(42, 1, 7)], &o(42, 1, 7), false);
    assert!(c.stolen() && c.cursor_moved, "cursor jumped and came back");
}

#[test]
fn attack_injected_input_passes_itself_off_as_the_person() {
    use crate::focus_guard::{Observation, fold, user_only};
    let o = |x: i32, tick: u32| Observation { foreground: 42, cursor: (x, 0), last_input_tick: tick };
    let person = fold(&o(1, 7), &[], &o(9, 8), false);
    assert!(user_only(person), "the person moving the mouse is not theft");
    let tool = fold(&o(1, 7), &[], &o(9, 8), true);
    assert!(!user_only(tool), "input the tool sent itself can't hide behind the fresh input time");
}

// ---------- (e) redaction follows the z-order ----------

fn rect(l: i32, t: i32, r: i32, b: i32) -> ScreenRect {
    ScreenRect { left: l, top: t, right: r, bottom: b }
}

fn area(rs: &[ScreenRect]) -> i64 {
    rs.iter().map(|r| i64::from(r.right - r.left) * i64::from(r.bottom - r.top)).sum()
}

#[test]
fn an_opaque_unfenced_window_on_top_is_not_blacked_out() {
    let mut top = notepad();
    top.rect = Some(rect(100, 100, 300, 300));
    let mut under = bank();
    under.rect = Some(rect(0, 0, 400, 400));
    let f = fence();
    let regions = super::probe::exposed_fenced_regions(&[top, under], |w| f.match_window(&w.facts).is_some());
    assert_eq!(area(&regions), 400 * 400 - 200 * 200, "only the uncovered part of the fenced window is black");
    for r in &regions {
        assert!(r.intersect(&rect(100, 100, 300, 300)).is_none(), "nothing black over the window on top: {r:?}");
    }
}

#[test]
fn a_fenced_window_on_top_stays_fully_black() {
    let mut top = bank();
    top.rect = Some(rect(0, 0, 400, 400));
    let mut under = notepad();
    under.rect = Some(rect(100, 100, 300, 300));
    let f = fence();
    let regions = super::probe::exposed_fenced_regions(&[top, under], |w| f.match_window(&w.facts).is_some());
    assert_eq!(area(&regions), 400 * 400);
}

#[test]
fn a_see_through_window_on_top_takes_nothing_away() {
    let mut glass = notepad();
    glass.rect = Some(rect(0, 0, 400, 400));
    glass.occluder = false;
    let mut under = bank();
    under.rect = Some(rect(0, 0, 400, 400));
    let f = fence();
    let regions = super::probe::exposed_fenced_regions(&[glass, under], |w| f.match_window(&w.facts).is_some());
    assert_eq!(area(&regions), 400 * 400, "a layered or cloaked window could show the fenced pixels");
}

#[test]
fn hidden_and_minimized_windows_neither_cover_nor_count() {
    let mut gone = notepad();
    gone.rect = Some(rect(0, 0, 400, 400));
    gone.minimized = true;
    let mut under = bank();
    under.rect = Some(rect(0, 0, 400, 400));
    let f = fence();
    let regions = super::probe::exposed_fenced_regions(&[gone, under], |w| f.match_window(&w.facts).is_some());
    assert_eq!(area(&regions), 400 * 400);
}

#[test]
fn a_fully_covered_fenced_window_leaves_nothing_to_black_out() {
    let mut top = notepad();
    top.rect = Some(rect(0, 0, 500, 500));
    let mut under = bank();
    under.rect = Some(rect(10, 10, 400, 400));
    let f = fence();
    let regions = super::probe::exposed_fenced_regions(&[top, under], |w| f.match_window(&w.facts).is_some());
    assert!(regions.is_empty());
}

#[test]
fn rectangle_minus_keeps_exactly_the_uncovered_bands() {
    let r = rect(0, 0, 10, 10);
    let pieces = r.minus(&rect(3, 3, 6, 6));
    assert_eq!(area(&pieces), 100 - 9);
    for (i, a) in pieces.iter().enumerate() {
        for b in pieces.iter().skip(i + 1) {
            assert!(a.intersect(b).is_none(), "bands must not overlap");
        }
    }
    assert_eq!(r.minus(&rect(20, 20, 30, 30)), vec![r]);
    assert!(r.minus(&rect(-1, -1, 11, 11)).is_empty());
}
