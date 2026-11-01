use super::*;
use crate::fence::policy::{FenceFile, ProcessFacts, WindowFacts};
use agent_desktop_core::{ImageFormat, WindowState};

struct FakeProbe {
    windows: Vec<LiveWindow>,
    list_fails: bool,
}

fn live(id: &str, pid: u32, exe: &str, class: &str, title: &str, rect: (i32, i32, i32, i32)) -> LiveWindow {
    LiveWindow {
        id: id.into(),
        pid,
        facts: WindowFacts {
            class: class.into(),
            title: title.into(),
            process: ProcessFacts { image_path: Some(exe.into()), image_name: Some(crate::fence::policy::file_name(exe)) },
            folder: None,
        },
        rect: Some(ScreenRect { left: rect.0, top: rect.1, right: rect.2, bottom: rect.3 }),
        visible: true,
        minimized: false,
        occluder: true,
    }
}

impl Probe for FakeProbe {
    fn process(&self, pid: u32) -> ProcessFacts {
        self.windows.iter().find(|w| w.pid == pid).map(|w| w.facts.process.clone()).unwrap_or_default()
    }
    fn window(&self, id: &str) -> Option<LiveWindow> {
        self.windows.iter().find(|w| w.id == id).cloned()
    }
    fn top_level_windows(&self) -> Option<Vec<LiveWindow>> {
        (!self.list_fails).then(|| self.windows.clone())
    }
    fn foreground(&self) -> Option<LiveWindow> {
        self.windows.first().cloned()
    }
    fn window_at(&self, x: i32, y: i32) -> Option<LiveWindow> {
        self.windows.iter().find(|w| w.rect.is_some_and(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)).cloned()
    }
    fn resolve_path(&self, path: &str) -> String {
        path.replace("C:\\Link", "C:\\Users\\alex\\Private")
    }
    fn display_rect(&self, _index: usize) -> Option<ScreenRect> {
        None
    }
    fn virtual_screen(&self) -> Option<ScreenRect> {
        Some(ScreenRect { left: 0, top: 0, right: 100, bottom: 100 })
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

struct Inner;
impl ObservationOps for Inner {
    fn list_windows(&self, _f: &WindowFilter, _d: Deadline) -> Result<Vec<WindowInfo>, AdapterError> {
        Ok(vec![win_info("w-1", 10), win_info("w-2", 20), win_info("w-3", 30)])
    }
    fn list_apps(&self, _d: Deadline) -> Result<Vec<AppInfo>, AdapterError> {
        Ok([10u32, 20, 30]
            .iter()
            .map(|p| AppInfo { name: format!("p{p}"), pid: ProcessId::new(*p), bundle_id: None, process_instance: None, presentation: None })
            .collect())
    }
}
impl ActionOps for Inner {}
impl InputOps for Inner {}
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
}

fn fence() -> Fence {
    let file: FenceFile = serde_json::from_str(r#"{"programs":{"process_names":["mybank.exe"]},"paths":["C:\\Users\\alex\\Private"]}"#).unwrap();
    Fence::new(&file, &[])
}

fn probe(list_fails: bool) -> Box<dyn Probe> {
    Box::new(FakeProbe {
        windows: vec![
            live("w-1", 10, "C:\\Windows\\notepad.exe", "Notepad", "notes", (0, 0, 50, 50)),
            live("w-2", 20, "C:\\Apps\\MyBank.exe", "BankFrame", "balance", (50, 50, 80, 80)),
            live("w-3", 30, "C:\\Program Files\\KeePass\\KeePass.exe", "KP", "db", (90, 0, 100, 10)),
        ],
        list_fails,
    })
}

fn deadline() -> Deadline {
    Deadline::after(5_000).unwrap()
}

#[test]
fn listings_leave_fenced_windows_and_apps_out() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    let wins = f.list_windows(&WindowFilter { focused_only: false, app: None }, deadline()).unwrap();
    assert_eq!(wins.iter().map(|w| w.id.as_str()).collect::<Vec<_>>(), vec!["w-1"]);
    let apps = f.list_apps(deadline()).unwrap();
    assert_eq!(apps.len(), 1);
    assert_eq!(apps[0].pid.get(), 10);
}

#[test]
fn reads_of_a_fenced_window_are_refused_with_fenced() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    let err = f.get_tree(&win_info("w-2", 20), &TreeOptions::default(), deadline()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Fenced);
    let ok = f.get_tree(&win_info("w-1", 10), &TreeOptions::default(), deadline()).unwrap_err();
    assert_ne!(ok.code, ErrorCode::Fenced);
}

#[test]
fn a_fenced_window_message_never_names_its_title() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    let err = f.get_tree(&win_info("w-2", 20), &TreeOptions::default(), deadline()).unwrap_err();
    let text = format!("{err:?}");
    assert!(!text.contains("balance"));
}

#[test]
fn screenshots_black_out_fenced_windows() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    let img = f.screenshot(ScreenshotTarget::FullScreen, deadline()).unwrap();
    let mut r = png::Decoder::new(std::io::Cursor::new(&img.data)).read_info().unwrap();
    let mut buf = vec![0u8; r.output_buffer_size().unwrap()];
    r.next_frame(&mut buf).unwrap();
    let px = |x: usize, y: usize| buf[(y * 100 + x) * 4];
    assert_eq!(px(65, 65), 0, "bank window is black");
    assert_eq!(px(95, 5), 0, "password manager is black");
    assert_eq!(px(10, 10), 255, "notepad is untouched");
}

#[test]
fn screenshot_is_refused_when_the_window_list_fails() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(true));
    let err = f.screenshot(ScreenshotTarget::FullScreen, deadline()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Fenced);
}

#[test]
fn exact_window_screenshot_of_a_fenced_window_is_refused() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    let err = f.screenshot(ScreenshotTarget::ExactWindow(win_info("w-3", 30)), deadline()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Fenced);
}

#[test]
fn launch_is_refused_for_fenced_programs_paths_and_shortcuts() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    let opts = |args: Vec<&str>| LaunchOptions {
        args: args.into_iter().map(String::from).collect(),
        env: Default::default(),
        cwd: None,
        timeout_ms: 1000,
        attach_if_running: false,
        activate: false,
        cdp_port: None,
    };
    assert!(f.check_launch("keepass", &opts(vec![])).is_err());
    assert!(f.check_launch("C:\\Apps\\MyBank.exe", &opts(vec![])).is_err());
    assert!(f.check_launch("notepad", &opts(vec!["C:\\Users\\alex\\Private\\taxes.txt"])).is_err());
    assert!(f.check_launch("notepad", &opts(vec!["C:\\Link\\taxes.txt"])).is_err(), "resolved through a link");
    assert!(f.check_launch("C:\\Users\\alex\\Desktop\\Bank.lnk", &opts(vec![])).is_err());
    assert!(f.check_launch("notepad", &opts(vec!["C:\\Users\\alex\\Documents\\a.txt"])).is_ok());
}

#[test]
fn typed_text_naming_a_fenced_path_is_refused() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    assert!(f.check_text("t", "C:\\Users\\alex\\Private\\x.pdf").is_err());
    assert!(f.check_text("t", "C:\\Link\\x.pdf").is_err());
    assert!(f.check_text("t", "hello").is_ok());
}

#[test]
fn physical_input_over_a_fenced_window_is_refused() {
    let inner = Inner;
    let f = FencedAdapter::new(&inner, fence(), probe(false));
    assert!(f.check_point("mouse_event", &Point { x: 60.0, y: 60.0 }).is_err());
    assert!(f.check_point("mouse_event", &Point { x: 10.0, y: 10.0 }).is_ok());
    assert!(f.check_point("mouse_event", &Point { x: 85.0, y: 85.0 }).is_err(), "unknown window fails closed");
}

fn glow_png(w: u32, h: u32) -> ImageBuffer {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().unwrap();
    wr.write_image_data(&vec![255u8; (w * h * 4) as usize]).unwrap();
    drop(wr);
    ImageBuffer { data: out, format: ImageFormat::Png, width: w, height: h, scale_factor: 1.0 }
}

fn rgba_at(img: &ImageBuffer, x: u32, y: u32) -> [u8; 4] {
    let mut r = png::Decoder::new(std::io::Cursor::new(&img.data)).read_info().unwrap();
    let mut buf = vec![0u8; r.output_buffer_size().unwrap()];
    r.next_frame(&mut buf).unwrap();
    let i = ((y * img.width + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

#[test]
fn a_glow_strip_still_showing_is_blacked_out_of_the_capture() {
    let area = ScreenRect { left: 0, top: 0, right: 100, bottom: 50 };
    let top = crate::glow::Rect { x: 0, y: 0, w: 100, h: 4 };
    let out = super::mask_glow(glow_png(100, 50), Some(area), &[top]).unwrap();
    assert_eq!(rgba_at(&out, 50, 1)[..3], [0, 0, 0]);
    assert_eq!(rgba_at(&out, 50, 30), [255, 255, 255, 255]);
}

#[test]
fn with_no_strips_the_capture_is_untouched_and_with_no_area_it_is_refused() {
    let img = glow_png(10, 10);
    let before = img.data.clone();
    let same = super::mask_glow(img, None, &[]).unwrap();
    assert_eq!(same.data, before);
    let img = glow_png(10, 10);
    let strip = crate::glow::Rect { x: 0, y: 0, w: 10, h: 2 };
    assert!(super::mask_glow(img, None, &[strip]).is_err());
}
