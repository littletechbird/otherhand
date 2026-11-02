//! The screen-border glow: a thin colored frame around every monitor while
//! Otherhand is acting, so the person can see at a glance that the agent
//! has control.
//!
//! The frame is drawn by a small helper process (the same executable, started
//! with a hidden argument) so it can linger for a moment after a command's
//! process has exited, which keeps a run of quick commands from flickering.
//! Each command tells the helper "begin" with its process id and "end" when
//! it is done; the helper also notices a command process that died without
//! saying so. The helper's windows are layered, click-through, topmost tool
//! windows that never activate, never show in the taskbar or Alt+Tab, and are
//! excluded from screen capture, so they never show up in Otherhand's own
//! screenshots or redaction checks and never move the foreground.
//!
//! When Windows refuses capture exclusion (seen in Windows Sandbox, where
//! `SetWindowDisplayAffinity` fails on these windows), the frame is still
//! shown, and Otherhand hides it for the moment of each of its own captures
//! instead: the screenshot path asks the helper to hide the strips, waits for
//! the screen to be redrawn without them, takes the picture and shows them
//! again. Any strip that is still visible and not excluded at that point is
//! blacked out of the picture, so the frame can never end up in a screenshot.
//! The cost is a short blink of the frame during each capture, and other
//! capture tools (a screen share, say) do see it. Set
//! `"require_capture_exclusion": true` to show no frame at all instead.
//!
//! The helper writes what it did to `<profile>\.otherhand\glow status.json`
//! (its pid, how many strips it made, whether they are excluded, and the
//! Windows error code of anything that failed), and a command that starts the
//! helper writes `glow spawn.json` next to it.
//!
//! Settings live in `<profile>\.otherhand\config.json`:
//!
//! ```json
//! { "glow": { "enabled": true, "color": "#00D4FF", "colors": ["#00D4FF"], "thickness": 12,
//!             "opacity": 100, "linger_ms": 1500, "require_capture_exclusion": false } }
//! ```
//!
//! `otherhand glow show`, `glow set` and `glow reset` edit that block.
//! `countdown_ms` is stored for a future focus-warning countdown and is not
//! acted on. A saved change applies on the next command that draws the glow;
//! the helper does not need a restart. `--no-glow` turns it off for one
//! command. Everything here except the window code is plain logic and is
//! tested on every platform.

// The drawing half only exists on Windows; elsewhere the logic is still
// built and tested.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

#[cfg(target_os = "windows")]
mod windows;

pub(crate) mod settings;

use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub(crate) const DEFAULT_COLOR: u32 = 0x00D4FF;
pub(crate) const DEFAULT_THICKNESS: u32 = 12;
pub(crate) const DEFAULT_LINGER_MS: u64 = 1_500;
pub(crate) const DEFAULT_OPACITY: u8 = 100;
/// Colors past the first. The drawn palette is the primary color plus these.
pub(crate) const MAX_EXTRA_COLORS: usize = 3;
/// Upper bound for the reserved focus-warning countdown, in milliseconds.
pub(crate) const MAX_COUNTDOWN_MS: u64 = 3_600_000;
/// One full pulse, dim to bright and back.
pub(crate) const PULSE_PERIOD_MS: u64 = 1_600;
/// The helper leaves when it has shown nothing for this long.
pub(crate) const IDLE_EXIT_MS: u64 = 60_000;
/// Hidden first argument that starts the helper.
pub(crate) const HELPER_ARG: &str = "__otherhand-glow";
/// Window class of the frame strips (what a test looks for).
pub(crate) const FRAME_CLASS: &str = "OtherhandGlowFrame";
/// Window class of the helper's message-only control window.
pub(crate) const CONTROL_CLASS: &str = "OtherhandGlowControl";
/// Longest a capture can keep the frame hidden, in case the command taking
/// it dies before saying it is done.
pub(crate) const HIDE_MAX_MS: u64 = 5_000;
/// After the frame could not be built, wait this long before trying again,
/// rather than making and destroying windows on every tick.
pub(crate) const REBUILD_RETRY_MS: u64 = 2_000;

/// Extra palette stops after the primary color. Empty means one solid color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlowStops {
    pub extra: [u32; MAX_EXTRA_COLORS],
    pub extra_count: u8,
}

impl Default for GlowStops {
    fn default() -> Self {
        Self { extra: [0; MAX_EXTRA_COLORS], extra_count: 0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlowConfig {
    pub enabled: bool,
    /// Primary color, 0xRRGGBB. Also the first palette stop.
    pub color: u32,
    pub stops: GlowStops,
    /// Frame width in pixels at 100% scaling.
    pub thickness: u32,
    pub linger_ms: u64,
    /// Show nothing when the frame can't be excluded from capture, instead
    /// of hiding it for Otherhand's own captures.
    pub strict: bool,
    /// Brightness as a percent. 100 is the unscaled pulse.
    pub opacity: u8,
}

impl Default for GlowConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            color: DEFAULT_COLOR,
            stops: GlowStops::default(),
            thickness: DEFAULT_THICKNESS,
            linger_ms: DEFAULT_LINGER_MS,
            strict: false,
            opacity: DEFAULT_OPACITY,
        }
    }
}

impl GlowConfig {
    /// Primary color first, then any extra stops, and how many of those are real.
    pub(crate) fn palette(&self) -> ([u32; 4], usize) {
        let extra = self.stops.extra_count.min(MAX_EXTRA_COLORS as u8) as usize;
        let mut colors = [self.color, 0, 0, 0];
        colors[1..1 + extra].copy_from_slice(&self.stops.extra[..extra]);
        (colors, 1 + extra)
    }

    pub(crate) fn set_palette(&mut self, colors: &[u32]) {
        let Some((first, rest)) = colors.split_first() else { return };
        self.color = *first;
        self.stops.extra = [0; MAX_EXTRA_COLORS];
        let n = rest.len().min(MAX_EXTRA_COLORS);
        self.stops.extra[..n].copy_from_slice(&rest[..n]);
        self.stops.extra_count = n as u8;
    }
}

/// Parses `#RGB`, `#RRGGBB` or `RRGGBB` into 0xRRGGBB.
pub(crate) fn parse_color(text: &str) -> Option<u32> {
    let hex = text.trim().trim_start_matches('#');
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match hex.len() {
        6 => u32::from_str_radix(hex, 16).ok(),
        3 => {
            let v = u32::from_str_radix(hex, 16).ok()?;
            let (r, g, b) = ((v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF);
            Some((r * 17) << 16 | (g * 17) << 8 | (b * 17))
        }
        _ => None,
    }
}

pub(crate) fn format_color(color: u32) -> String {
    format!("#{:06X}", color & 0xFF_FFFF)
}

/// Reads the `glow` block of the config document. Anything missing or
/// unreadable falls back to its default, so a broken file never turns the
/// glow off by accident.
pub(crate) fn config_from(doc: Option<&Value>) -> GlowConfig {
    let mut cfg = GlowConfig::default();
    let Some(glow) = doc.and_then(|d| d.get("glow")) else {
        return cfg;
    };
    if let Some(b) = glow.as_bool() {
        cfg.enabled = b;
        return cfg;
    }
    if let Some(b) = glow.get("enabled").and_then(Value::as_bool) {
        cfg.enabled = b;
    }
    if let Some(c) = glow.get("color").and_then(Value::as_str).and_then(parse_color) {
        cfg.color = c;
    }
    if let Some(list) = glow.get("colors").and_then(Value::as_array) {
        let parsed = palette_colors(list);
        if !parsed.is_empty() {
            cfg.set_palette(&parsed);
        }
    }
    if let Some(t) = glow.get("thickness").and_then(Value::as_u64) {
        cfg.thickness = t.clamp(2, 24) as u32;
    }
    if let Some(l) = glow.get("linger_ms").and_then(Value::as_u64) {
        cfg.linger_ms = l.min(10_000);
    }
    if let Some(b) = glow.get("require_capture_exclusion").and_then(Value::as_bool) {
        cfg.strict = b;
    }
    if let Some(o) = percent_value(glow.get("opacity")).or_else(|| percent_value(glow.get("brightness"))) {
        cfg.opacity = o;
    }
    cfg
}

fn palette_colors(list: &[Value]) -> Vec<u32> {
    list.iter().filter_map(|v| v.as_str().and_then(parse_color)).take(1 + MAX_EXTRA_COLORS).collect()
}

fn percent_value(value: Option<&Value>) -> Option<u8> {
    let n = value?.as_u64()?;
    Some(n.min(100) as u8)
}

/// `<profile>\.otherhand\<name>`, where the helper and the client leave
/// their notes about the glow.
pub(crate) fn note_path(name: &str) -> Option<std::path::PathBuf> {
    crate::fence::audit::home_dir().map(|h| h.join(".otherhand").join(name))
}

/// Writes a note whole (temp file, then rename), so a reader never sees half
/// of one. Notes are for people and tests; failing to write one changes
/// nothing else.
pub(crate) fn write_note(name: &str, doc: &Value) {
    let Some(path) = note_path(name) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, serde_json::to_vec_pretty(doc).unwrap_or_default()).is_ok() {
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

pub(crate) fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// `<profile>\.otherhand\config.json`.
pub(crate) fn config_path() -> Option<std::path::PathBuf> {
    crate::fence::audit::home_dir().map(|h| h.join(".otherhand").join("config.json"))
}

pub(crate) fn load_config() -> GlowConfig {
    let doc = config_path()
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<Value>(strip_bom(&b)).ok());
    config_from(doc.as_ref())
}

pub(crate) fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

static FLAG_OFF: AtomicBool = AtomicBool::new(false);

/// `--no-glow` on the command line.
pub(crate) fn set_disabled_by_flag(off: bool) {
    FLAG_OFF.store(off, Ordering::SeqCst);
}

/// Commands that act on the desktop. Reads, sessions, traces and the
/// clipboard leave the screen alone and get no frame.
pub(crate) fn glows_for(command: &str) -> bool {
    matches!(
        command,
        "click" | "double-click" | "triple-click" | "right-click" | "type" | "set-value" | "fill-secret"
            | "clear" | "focus" | "select" | "toggle" | "check" | "uncheck" | "expand" | "collapse"
            | "scroll" | "scroll-to" | "press" | "key-down" | "key-up" | "hover" | "drag" | "mouse-move"
            | "mouse-click" | "mouse-down" | "mouse-up" | "mouse-wheel" | "launch" | "close-app"
            | "focus-window" | "open-system-surface" | "resize-window" | "move-window" | "minimize"
            | "maximize" | "restore" | "dismiss-notification" | "dismiss-all-notifications"
            | "notification-action" | "batch"
    )
}

/// Begins and ends of the glow inside this process, so a batch and the
/// commands inside it send the helper one begin and one end.
static DEPTH: AtomicU32 = AtomicU32::new(0);

/// Held while a command runs; dropping it ends this command's share of the
/// glow. The helper keeps the frame up for the linger time after the last
/// one ends.
pub(crate) struct GlowHold {
    _private: (),
}

impl Drop for GlowHold {
    fn drop(&mut self) {
        if DEPTH.fetch_sub(1, Ordering::SeqCst) == 1 {
            platform_end();
        }
    }
}

/// Starts the glow for `command` when it acts on the desktop and the glow is
/// on. Never fails a command: if the frame can't be shown, the command runs
/// without it.
pub(crate) fn begin(command: &str) -> Option<GlowHold> {
    if FLAG_OFF.load(Ordering::SeqCst) || !glows_for(command) {
        return None;
    }
    if DEPTH.fetch_add(1, Ordering::SeqCst) == 0 {
        let cfg = load_config();
        if !cfg.enabled {
            DEPTH.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        platform_begin(&cfg);
    }
    Some(GlowHold { _private: () })
}

#[cfg(target_os = "windows")]
fn platform_begin(cfg: &GlowConfig) {
    windows::client_begin(cfg);
}
#[cfg(not(target_os = "windows"))]
fn platform_begin(_cfg: &GlowConfig) {}

#[cfg(target_os = "windows")]
fn platform_end() {
    windows::client_end();
}
#[cfg(not(target_os = "windows"))]
fn platform_end() {}

/// Held around one of Otherhand's own screen captures. While it lives, a
/// frame that is not excluded from capture is hidden; `mask` lists any strip
/// that could not be hidden in time and must be blacked out of the picture.
pub(crate) struct CaptureHide {
    pub mask: Vec<Rect>,
    hid: bool,
}

impl CaptureHide {
    /// Strips to black out of a picture just taken: those listed before it
    /// and any that are showing, not excluded, now.
    pub(crate) fn strips_to_mask(&self) -> Vec<Rect> {
        let mut out = self.mask.clone();
        for r in platform_leftover() {
            if !out.contains(&r) {
                out.push(r);
            }
        }
        out
    }
}

#[cfg(target_os = "windows")]
fn platform_leftover() -> Vec<Rect> {
    windows::leftover_strips()
}
#[cfg(not(target_os = "windows"))]
fn platform_leftover() -> Vec<Rect> {
    Vec::new()
}

impl Drop for CaptureHide {
    fn drop(&mut self) {
        if self.hid {
            platform_capture_end();
        }
    }
}

/// Call right before taking pixels from the screen. Always safe: with no
/// helper running it does nothing.
pub(crate) fn hide_for_capture() -> CaptureHide {
    let (hid, mask) = platform_capture_begin();
    CaptureHide { mask, hid }
}

#[cfg(target_os = "windows")]
fn platform_capture_begin() -> (bool, Vec<Rect>) {
    windows::client_capture_begin()
}
#[cfg(not(target_os = "windows"))]
fn platform_capture_begin() -> (bool, Vec<Rect>) {
    (false, Vec::new())
}

#[cfg(target_os = "windows")]
fn platform_capture_end() {
    windows::client_capture_end();
}
#[cfg(not(target_os = "windows"))]
fn platform_capture_end() {}

/// How a built frame stays out of Otherhand's screenshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaptureMode {
    /// Windows leaves every strip out of captures.
    Excluded,
    /// At least one strip can't be excluded; it is hidden for each capture.
    HideForCapture,
}

impl CaptureMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Excluded => "excluded_from_capture",
            Self::HideForCapture => "hidden_for_each_capture",
        }
    }
}

/// The mode of a frame with `excluded` of its `total` strips excluded, or
/// `None` for an empty frame.
pub(crate) fn capture_mode(excluded: usize, total: usize) -> Option<CaptureMode> {
    match total {
        0 => None,
        t if excluded >= t => Some(CaptureMode::Excluded),
        _ => Some(CaptureMode::HideForCapture),
    }
}

/// Whether a strip whose exclusion failed may still be shown.
pub(crate) fn keep_unexcluded_strip(cfg: &GlowConfig) -> bool {
    !cfg.strict
}

/// Whether to (re)build the frame now: it should be up, has no strips, and
/// the last attempt (if any) is old enough.
pub(crate) fn rebuild_due(frame_empty: bool, last_attempt: Option<u64>, now: u64) -> bool {
    frame_empty && last_attempt.is_none_or(|t| now.saturating_sub(t) >= REBUILD_RETRY_MS)
}

/// Commands that are taking a capture right now, each until a deadline.
#[derive(Debug, Default)]
pub(crate) struct CaptureHides {
    until: BTreeMap<u32, u64>,
}

impl CaptureHides {
    pub(crate) fn hide(&mut self, pid: u32, now: u64) {
        self.until.insert(pid, now + HIDE_MAX_MS);
    }

    pub(crate) fn show(&mut self, pid: u32) {
        self.until.remove(&pid);
    }

    /// Whether the frame must stay hidden, dropping hides that ran out or
    /// whose process is gone.
    pub(crate) fn active(&mut self, now: u64, alive: impl Fn(u32) -> bool) -> bool {
        self.until.retain(|pid, t| now < *t && alive(*pid));
        !self.until.is_empty()
    }
}

/// Runs the helper when argv asks for it.
pub(crate) fn helper_entry() -> Option<std::process::ExitCode> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some(HELPER_ARG) {
        return None;
    }
    let opts = HelperArgs::parse(&args[2..]);
    #[cfg(target_os = "windows")]
    {
        Some(windows::run_helper(opts))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = opts;
        Some(std::process::ExitCode::FAILURE)
    }
}

/// Packs the settings into the 64-bit message parameter a begin carries, so
/// a changed setting shows on the next command without restarting the
/// helper: color in bits 0-23, a "set" mark in bit 24, thickness in bits
/// 25-29, "require capture exclusion" in bit 30, an opacity mark in bit 31,
/// the linger in bits 32-47 and opacity (0-100) in bits 48-54. Extra palette
/// stops do not fit; the helper reads those from the config file.
pub(crate) fn pack_config(cfg: &GlowConfig) -> u64 {
    u64::from(cfg.color & 0xFF_FFFF)
        | 1 << 24
        | u64::from(cfg.thickness.clamp(2, 24)) << 25
        | u64::from(cfg.strict) << 30
        | 1 << 31
        | cfg.linger_ms.min(10_000) << 32
        | u64::from(cfg.opacity.min(100)) << 48
}

pub(crate) fn unpack_config(v: u64) -> Option<GlowConfig> {
    if v & (1 << 24) == 0 {
        return None;
    }
    let opacity = if v & (1 << 31) == 0 { DEFAULT_OPACITY } else { ((v >> 48) & 0x7F).min(100) as u8 };
    Some(GlowConfig {
        enabled: true,
        color: (v & 0xFF_FFFF) as u32,
        stops: GlowStops::default(),
        thickness: (((v >> 25) & 0x1F) as u32).clamp(2, 24),
        linger_ms: ((v >> 32) & 0xFFFF).min(10_000),
        strict: v & (1 << 30) != 0,
        opacity,
    })
}

/// What the helper should draw after a begin. The file is the source of
/// truth, including palette stops the packed message cannot carry. The
/// helper only runs while the glow is enabled, so the adopted config is on.
pub(crate) fn adopt_saved(saved: &GlowConfig) -> GlowConfig {
    let mut cfg = *saved;
    cfg.enabled = true;
    cfg
}

/// Whether the strips have to be rebuilt. Opacity is applied when the strip
/// is presented, so a brightness-only change does not rebuild them.
pub(crate) fn frame_rebuild(prev: &GlowConfig, next: &GlowConfig) -> bool {
    prev.color != next.color || prev.stops != next.stops || prev.thickness != next.thickness || prev.strict != next.strict
}

/// What the helper is started with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperArgs {
    pub first_pid: Option<u32>,
    pub config: GlowConfig,
}

impl HelperArgs {
    pub(crate) fn to_args(&self) -> Vec<String> {
        let mut out = vec![HELPER_ARG.to_string()];
        if let Some(pid) = self.first_pid {
            out.extend(["--pid".into(), pid.to_string()]);
        }
        out.extend([
            "--color".into(),
            format_color(self.config.color).trim_start_matches('#').to_string(),
            "--thickness".into(),
            self.config.thickness.to_string(),
            "--linger".into(),
            self.config.linger_ms.to_string(),
            "--opacity".into(),
            self.config.opacity.to_string(),
        ]);
        if self.config.stops.extra_count > 0 {
            out.extend(["--colors".into(), palette_arg(&self.config)]);
        }
        if self.config.strict {
            out.extend(["--strict".into(), "1".into()]);
        }
        out
    }

    pub(crate) fn parse(args: &[String]) -> Self {
        let mut out = Self { first_pid: None, config: GlowConfig::default() };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let Some(value) = it.next() else { break };
            match flag.as_str() {
                "--pid" => out.first_pid = value.parse().ok(),
                "--color" => {
                    out.config.color = parse_color(value).unwrap_or(DEFAULT_COLOR);
                    out.config.stops = GlowStops::default();
                }
                "--colors" => out.config.set_palette(&palette_colors(&split_colors(value))),
                "--thickness" => out.config.thickness = value.parse::<u32>().map_or(DEFAULT_THICKNESS, |t| t.clamp(2, 24)),
                "--linger" => out.config.linger_ms = value.parse::<u64>().map_or(DEFAULT_LINGER_MS, |l| l.min(10_000)),
                "--opacity" => out.config.opacity = value.parse::<u8>().map_or(DEFAULT_OPACITY, |o| o.min(100)),
                "--strict" => out.config.strict = value == "1",
                _ => {}
            }
        }
        out
    }
}

/// The helper's bookkeeping: which command processes hold the glow, and when
/// the frame should come down after the last one lets go.
#[derive(Debug, Default)]
pub(crate) struct Holders {
    active: BTreeMap<u32, u32>,
    linger_until: Option<u64>,
    last_visible_at: u64,
    linger_ms: u64,
}

impl Holders {
    pub(crate) fn new(linger_ms: u64, now: u64) -> Self {
        Self { linger_ms, last_visible_at: now, ..Self::default() }
    }

    pub(crate) fn set_linger(&mut self, linger_ms: u64) {
        self.linger_ms = linger_ms;
    }

    pub(crate) fn begin(&mut self, pid: u32) {
        *self.active.entry(pid).or_insert(0) += 1;
        self.linger_until = None;
    }

    pub(crate) fn end(&mut self, pid: u32, now: u64) {
        if let Some(n) = self.active.get_mut(&pid) {
            *n -= 1;
            if *n == 0 {
                self.active.remove(&pid);
            }
        }
        self.start_linger(now);
    }

    /// Drops holders whose process is gone (a command that crashed).
    pub(crate) fn prune(&mut self, now: u64, alive: impl Fn(u32) -> bool) {
        let before = self.active.len();
        self.active.retain(|pid, _| alive(*pid));
        if self.active.len() != before {
            self.start_linger(now);
        }
    }

    fn start_linger(&mut self, now: u64) {
        if self.active.is_empty() && self.linger_until.is_none() {
            self.linger_until = Some(now + self.linger_ms);
        }
    }

    #[cfg(test)]
    pub(crate) fn pids(&self) -> Vec<u32> {
        self.active.keys().copied().collect()
    }

    /// Whether the frame should be on screen now.
    pub(crate) fn visible(&mut self, now: u64) -> bool {
        let on = !self.active.is_empty() || self.linger_until.is_some_and(|t| now < t);
        if on {
            self.last_visible_at = now;
        } else {
            self.linger_until = None;
        }
        on
    }

    /// Whether the helper has been idle long enough to leave.
    pub(crate) fn should_exit(&self, now: u64) -> bool {
        self.active.is_empty() && self.linger_until.is_none() && now.saturating_sub(self.last_visible_at) >= IDLE_EXIT_MS
    }
}

fn palette_arg(cfg: &GlowConfig) -> String {
    let (colors, n) = cfg.palette();
    colors[..n].iter().copied().map(format_color).collect::<Vec<_>>().join(",")
}

fn split_colors(text: &str) -> Vec<Value> {
    text.split(',').map(|part| Value::String(part.trim().to_string())).collect()
}

/// Constant alpha for the pulse at `t_ms`: a slow breath between about 60%
/// and full, so the frame is noticeable without blinking.
pub(crate) fn pulse_alpha(t_ms: u64) -> u8 {
    let phase = (t_ms % PULSE_PERIOD_MS) as f64 / PULSE_PERIOD_MS as f64;
    let wave = 0.5 - 0.5 * (phase * std::f64::consts::TAU).cos();
    (155.0 + 100.0 * wave).round().clamp(0.0, 255.0) as u8
}

/// Pulse alpha scaled by an opacity percent. 100 leaves the pulse unchanged.
pub(crate) fn frame_alpha(t_ms: u64, opacity: u8) -> u8 {
    (u32::from(pulse_alpha(t_ms)) * u32::from(opacity.min(100)) / 100) as u8
}

/// Thickness in pixels for a monitor at `dpi` (96 = 100%).
pub(crate) fn scaled_thickness(thickness: u32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { 96 } else { dpi };
    ((thickness * dpi + 48) / 96).max(2) as i32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// The four strips that make the frame of one monitor: top and bottom run the
/// full width, left and right fill the height between them. Thin strips,
/// rather than one window the size of the screen, keep the system from
/// mistaking the frame for a full-screen app.
pub(crate) fn strips(monitor: Rect, t: i32) -> Vec<Rect> {
    let t = t.min(monitor.w / 2).min(monitor.h / 2).max(1);
    let inner = monitor.h - 2 * t;
    let mut out = vec![
        Rect { x: monitor.x, y: monitor.y, w: monitor.w, h: t },
        Rect { x: monitor.x, y: monitor.y + monitor.h - t, w: monitor.w, h: t },
    ];
    if inner > 0 {
        out.push(Rect { x: monitor.x, y: monitor.y + t, w: t, h: inner });
        out.push(Rect { x: monitor.x + monitor.w - t, y: monitor.y + t, w: t, h: inner });
    }
    out
}

/// Opacity of the frame `d` pixels in from the monitor's edge: solid at the
/// edge, fading to nothing at the inner side, which reads as a glow.
pub(crate) fn edge_alpha(d: i32, t: i32) -> u8 {
    if d < 0 || d >= t || t <= 0 {
        return 0;
    }
    let f = 1.0 - (d as f64 + 0.5) / t as f64;
    (255.0 * f.powf(1.6)).round().clamp(0.0, 255.0) as u8
}

/// Premultiplied BGRA pixels (as `u32` 0xAARRGGBB) for one strip, top row
/// first. Distance is measured to the nearest monitor edge, so the corners
/// meet cleanly.
pub(crate) fn render_strip(strip: Rect, monitor: Rect, t: i32, colors: &[u32]) -> Vec<u32> {
    let fallback = [DEFAULT_COLOR];
    let colors = if colors.is_empty() { &fallback[..] } else { colors };
    let mut out = Vec::with_capacity((strip.w.max(0) * strip.h.max(0)) as usize);
    for row in 0..strip.h {
        let y = strip.y + row - monitor.y;
        for col in 0..strip.w {
            let x = strip.x + col - monitor.x;
            let d = x.min(y).min(monitor.w - 1 - x).min(monitor.h - 1 - y);
            let (r, g, b) = color_channels(colors, d, t);
            let a = u32::from(edge_alpha(d, t));
            let pm = |c: u32| (c * a + 127) / 255;
            out.push(a << 24 | pm(r) << 16 | pm(g) << 8 | pm(b));
        }
    }
    out
}

fn color_channels(colors: &[u32], d: i32, t: i32) -> (u32, u32, u32) {
    if colors.len() < 2 || t <= 1 {
        return channels(colors[0]);
    }
    let span = (colors.len() - 1) as f64;
    let along = (d.max(0) as f64) / ((t - 1).max(1) as f64);
    let scaled = (along * span).clamp(0.0, span);
    let index = (scaled.floor() as usize).min(colors.len() - 2);
    let frac = scaled - index as f64;
    let (r0, g0, b0) = channels(colors[index]);
    let (r1, g1, b1) = channels(colors[index + 1]);
    let mix = |from: u32, to: u32| (from as f64 + (to as f64 - from as f64) * frac).round() as u32;
    (mix(r0, r1), mix(g0, g1), mix(b0, b1))
}

fn channels(color: u32) -> (u32, u32, u32) {
    ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF)
}

#[cfg(test)]
mod tests;
