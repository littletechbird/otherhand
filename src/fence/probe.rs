//! Live facts the fence needs from the OS, behind a trait so the decision
//! logic is tested with a fake on every host.

use super::policy::{ProcessFacts, WindowFacts};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub(crate) fn intersect(&self, other: &ScreenRect) -> Option<ScreenRect> {
        let r = ScreenRect {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (r.right > r.left && r.bottom > r.top).then_some(r)
    }

    /// The parts of `self` that `cover` does not overlap: up to four bands.
    pub(crate) fn minus(&self, cover: &ScreenRect) -> Vec<ScreenRect> {
        let Some(hole) = self.intersect(cover) else {
            return vec![*self];
        };
        let mut out = Vec::with_capacity(4);
        let mut push = |r: ScreenRect| {
            if r.right > r.left && r.bottom > r.top {
                out.push(r);
            }
        };
        push(ScreenRect { left: self.left, top: self.top, right: self.right, bottom: hole.top });
        push(ScreenRect { left: self.left, top: hole.bottom, right: self.right, bottom: self.bottom });
        push(ScreenRect { left: self.left, top: hole.top, right: hole.left, bottom: hole.bottom });
        push(ScreenRect { left: hole.right, top: hole.top, right: self.right, bottom: hole.bottom });
        out
    }
}

/// What of each fenced window is actually on screen, given windows listed
/// top of the z-order first. A fenced window keeps its whole rectangle
/// except where an opaque unfenced window sits above it; windows that can
/// show what is under them (layered, transparent, cloaked, shaped, shell
/// chrome) never take anything away. `is_fenced` decides per window.
pub(crate) fn exposed_fenced_regions(windows: &[LiveWindow], is_fenced: impl Fn(&LiveWindow) -> bool) -> Vec<ScreenRect> {
    let mut above: Vec<ScreenRect> = Vec::new();
    let mut out = Vec::new();
    for w in windows.iter().filter(|w| w.visible && !w.minimized) {
        let Some(rect) = w.rect else {
            continue;
        };
        if is_fenced(w) {
            let mut pieces = vec![rect];
            for cover in &above {
                pieces = pieces.iter().flat_map(|p| p.minus(cover)).collect();
                if pieces.is_empty() {
                    break;
                }
            }
            out.extend(pieces);
        } else if w.occluder {
            above.push(rect);
        }
    }
    out
}

#[derive(Debug, Clone)]
pub(crate) struct LiveWindow {
    #[allow(dead_code)] // read by tests and kept for diagnostics
    pub id: String,
    pub pid: u32,
    pub facts: WindowFacts,
    pub rect: Option<ScreenRect>,
    pub visible: bool,
    pub minimized: bool,
    /// Opaque and plainly rectangular, so nothing under it shows through.
    /// False for layered, transparent, cloaked, shaped and shell windows.
    pub occluder: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ClipboardFacts {
    pub owner_pid: Option<u32>,
    pub marked_sensitive: bool,
}

pub(crate) trait Probe: Send + Sync {
    fn process(&self, pid: u32) -> ProcessFacts;
    fn window(&self, id: &str) -> Option<LiveWindow>;
    /// Every visible top-level window. `None` when enumeration failed.
    fn top_level_windows(&self) -> Option<Vec<LiveWindow>>;
    /// Every visible top-level window, top of the z-order first. `None`
    /// when enumeration failed.
    fn z_ordered_windows(&self) -> Option<Vec<LiveWindow>> {
        self.top_level_windows()
    }
    fn foreground(&self) -> Option<LiveWindow>;
    fn window_at(&self, x: i32, y: i32) -> Option<LiveWindow>;
    /// Fully resolved form of `path` (links, junctions, 8.3 names, mapped
    /// drives). Falls back to the nearest existing ancestor plus the rest.
    fn resolve_path(&self, path: &str) -> String;
    /// `.lnk` / `.url` targets cannot be followed safely here, so callers refuse them.
    /// Matched on the normalized name, so `x.lnk.` and `x.lnk::$DATA` count.
    fn is_shortcut(&self, path: &str) -> bool {
        let p = super::policy::normalize_path(path);
        [".lnk", ".url", ".appref-ms", ".pif", ".website", ".library-ms", ".search-ms", ".searchconnector-ms", ".scf"]
            .iter()
            .any(|ext| p.ends_with(ext))
    }
    /// Who owns the clipboard and whether the owner marked the data as not
    /// for clipboard monitors (password managers do). `None` when unknown.
    fn clipboard_facts(&self) -> Option<ClipboardFacts> {
        Some(ClipboardFacts::default())
    }
    fn display_rect(&self, index: usize) -> Option<ScreenRect>;
    fn virtual_screen(&self) -> Option<ScreenRect>;
}

#[cfg(not(target_os = "windows"))]
pub(crate) struct NullProbe;

#[cfg(not(target_os = "windows"))]
impl Probe for NullProbe {
    fn process(&self, _pid: u32) -> ProcessFacts {
        ProcessFacts { image_path: None, image_name: Some(String::from("unknown")) }
    }
    fn window(&self, _id: &str) -> Option<LiveWindow> {
        None
    }
    fn top_level_windows(&self) -> Option<Vec<LiveWindow>> {
        Some(Vec::new())
    }
    fn foreground(&self) -> Option<LiveWindow> {
        None
    }
    fn window_at(&self, _x: i32, _y: i32) -> Option<LiveWindow> {
        None
    }
    fn resolve_path(&self, path: &str) -> String {
        path.to_string()
    }
    fn display_rect(&self, _index: usize) -> Option<ScreenRect> {
        None
    }
    fn virtual_screen(&self) -> Option<ScreenRect> {
        None
    }
}
