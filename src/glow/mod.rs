//! The screen-border glow: a colored frame around every monitor while
//! Otherhand is acting, so the person can see at a glance that the agent
//! has control.
//!
//! The frame is drawn by a small helper process (the same executable, started
//! with a hidden argument). The first acting command opens a work session.
//! The frame stays up across later commands, including headless ones that
//! never move the foreground, until that session releases: every client has
//! gone and a clear idle passes with no new command, the agent ends its
//! session, or a client is explicitly released. `linger_ms` is only the short
//! fade after that release. Switching windows does not end the session.
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
//! { "glow": { "enabled": true, "color": "#00D4FF", "thickness": 24, "linger_ms": 1500,
//!             "require_capture_exclusion": false } }
//! ```
//!
//! A command that takes focus or sends input waits until the helper has
//! confirmed the frame is visible, then waits out `countdown_ms`, and refuses
//! with `GLOW_UNAVAILABLE` if the frame cannot be shown. The warning cannot
//! be switched off from config or from a flag. The person turns it off from
//! an interactive console with `glow set --off`; focus-taking commands then
//! refuse instead of running dark. Everything here except the window code is
//! plain logic and is tested on every platform.

// The drawing half only exists on Windows; elsewhere the logic is still
// built and tested.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

mod work;

mod person;
pub(crate) mod settings;
pub(crate) mod warn;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "windows")]
mod windows_warn;

#[cfg(all(test, target_os = "windows"))]
#[path = "live_tests.rs"]
mod live_tests;

use agent_desktop_core::AppError;
use serde_json::Value;
use std::collections::BTreeMap;
#[cfg(not(test))]
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub(crate) const DEFAULT_COLOR: u32 = 0x00D4FF;
/// Four times the original 6px edge, so the frame is easy to see at a desk.
pub(crate) const DEFAULT_THICKNESS: u32 = 24;
pub(crate) const MIN_THICKNESS: u32 = 2;
/// Above five times the original 6px edge (30), so that thickness is not the ceiling.
pub(crate) const MAX_THICKNESS: u32 = 48;
pub(crate) const DEFAULT_LINGER_MS: u64 = 1_500;
/// After the last acting client has gone, this long with no new command
/// releases the work session. It is not `linger_ms`.
pub(crate) const SESSION_IDLE_MS: u64 = 30_000;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GlowConfig {
    pub enabled: bool,
    /// 0xRRGGBB.
    pub color: u32,
    /// Frame width in pixels at 100% scaling.
    pub thickness: u32,
    pub linger_ms: u64,
    /// How long the frame stays up, brightening, before focus or input.
    pub countdown_ms: u64,
    /// Show nothing when the frame can't be excluded from capture, instead
    /// of hiding it for Otherhand's own captures.
    pub strict: bool,
}

impl Default for GlowConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            color: DEFAULT_COLOR,
            thickness: DEFAULT_THICKNESS,
            linger_ms: DEFAULT_LINGER_MS,
            countdown_ms: warn::DEFAULT_COUNTDOWN_MS,
            strict: false,
        }
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

pub(crate) fn clamp_thickness(thickness: u32) -> u32 {
    thickness.clamp(MIN_THICKNESS, MAX_THICKNESS)
}

/// Reads the `glow` block of the config document. Anything missing or
/// unreadable falls back to its default, so a broken file never turns the
/// glow off by accident.
pub(crate) fn config_from(doc: Option<&Value>) -> GlowConfig {
    let mut cfg = GlowConfig::default();
    let Some(glow) = doc.and_then(|d| d.get("glow")) else {
        return cfg;
    };
    if glow.as_bool().is_some() {
        return cfg;
    }
    if let Some(c) = glow
        .get("color")
        .and_then(Value::as_str)
        .and_then(parse_color)
    {
        cfg.color = c;
    }
    if let Some(t) = glow.get("thickness").and_then(Value::as_u64) {
        cfg.thickness = clamp_thickness(u32::try_from(t).unwrap_or(MAX_THICKNESS));
    }
    if let Some(l) = glow.get("linger_ms").and_then(Value::as_u64) {
        cfg.linger_ms = l.min(10_000);
    }
    if let Some(ms) = glow.get("countdown_ms").and_then(Value::as_u64) {
        cfg.countdown_ms = warn::clamp_countdown(ms);
    }
    if let Some(b) = glow
        .get("require_capture_exclusion")
        .and_then(Value::as_bool)
    {
        cfg.strict = b;
    }
    cfg
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
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
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

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Commands that take focus or send input. Kept as the name the rest of the
/// crate already calls.
pub(crate) fn glows_for(command: &str) -> bool {
    warn::warning_required(command)
}

/// Begins and ends of the glow inside this process, so a batch and the
/// commands inside it send the helper one begin and one end.
#[cfg(not(test))]
static DEPTH: AtomicU32 = AtomicU32::new(0);
#[cfg(not(test))]
static FRAME_SHOWN: AtomicBool = AtomicBool::new(false);
static PREPARE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
thread_local! {
    static TEST_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    static TEST_SHOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn depth_load() -> u32 {
    #[cfg(test)]
    {
        return TEST_DEPTH.with(|cell| cell.get());
    }
    #[cfg(not(test))]
    {
        DEPTH.load(Ordering::SeqCst)
    }
}

fn depth_store(value: u32) {
    #[cfg(test)]
    {
        TEST_DEPTH.with(|cell| cell.set(value));
    }
    #[cfg(not(test))]
    {
        DEPTH.store(value, Ordering::SeqCst);
    }
}

fn depth_add(n: u32) -> u32 {
    #[cfg(test)]
    {
        return TEST_DEPTH.with(|cell| {
            let prev = cell.get();
            cell.set(prev.saturating_add(n));
            prev
        });
    }
    #[cfg(not(test))]
    {
        DEPTH.fetch_add(n, Ordering::SeqCst)
    }
}

fn depth_sub(n: u32) -> u32 {
    #[cfg(test)]
    {
        return TEST_DEPTH.with(|cell| {
            let prev = cell.get();
            cell.set(prev.saturating_sub(n));
            prev
        });
    }
    #[cfg(not(test))]
    {
        DEPTH.fetch_sub(n, Ordering::SeqCst)
    }
}

fn frame_shown_load() -> bool {
    #[cfg(test)]
    {
        return TEST_SHOWN.with(|cell| cell.get());
    }
    #[cfg(not(test))]
    {
        FRAME_SHOWN.load(Ordering::SeqCst)
    }
}

fn frame_shown_store(value: bool) {
    #[cfg(test)]
    {
        TEST_SHOWN.with(|cell| cell.set(value));
    }
    #[cfg(not(test))]
    {
        FRAME_SHOWN.store(value, Ordering::SeqCst);
    }
}

/// Held while a command runs; dropping it ends this command's share of the
/// glow. The work session stays open after that until it releases.
#[derive(Debug)]
pub(crate) struct GlowHold {
    report: warn::GlowReport,
}

impl Drop for GlowHold {
    fn drop(&mut self) {
        if depth_sub(1) == 1 {
            platform_end();
        }
    }
}

/// What an acting command attaches to its result, plus the hold that ends
/// the command's share of the glow when dropped.
#[derive(Debug)]
pub(crate) struct Prepared {
    pub report: warn::GlowReport,
    hold: Option<GlowHold>,
}

/// Shows the warning and waits out the countdown for a focus-taking command.
/// A mutation that does not take focus gets a `not_required` report and no
/// hold. A focus-taking command that cannot show the frame returns
/// `GLOW_UNAVAILABLE` and does not run.
pub(crate) fn prepare(command: &str) -> Result<Prepared, AppError> {
    let _prepare = PREPARE.lock().unwrap_or_else(|error| error.into_inner());
    if !glows_for(command) {
        return Ok(Prepared {
            report: warn::GlowReport::not_required(),
            hold: None,
        });
    }
    if depth_load() > 0 {
        depth_add(1);
        let shown = frame_shown_load();
        let report = warn::GlowReport {
            shown,
            mode: if shown { "warning" } else { "unsupported" },
            error: None,
            countdown_ms: 0,
        };
        return Ok(Prepared {
            report: report.clone(),
            hold: Some(GlowHold { report }),
        });
    }
    let cfg = load_config();
    let outcome = if person::is_off() {
        warn::ShowOutcome::PersonOff
    } else {
        platform_show(&cfg)
    };
    let decision = warn::decide(outcome, cfg.countdown_ms);
    if let Some(block) = decision.block {
        return Err(refusal(block));
    }
    if decision.wait_ms > 0 {
        wait_countdown(decision.wait_ms);
    }
    frame_shown_store(decision.report.shown);
    depth_store(1);
    Ok(Prepared {
        report: decision.report.clone(),
        hold: Some(GlowHold {
            report: decision.report,
        }),
    })
}

fn refusal(block: warn::Block) -> AppError {
    use agent_desktop_core::{AdapterError, DeliverySemantics, ErrorCode};
    let (message, suggestion, mode, detail) = match block {
        warn::Block::PersonOff => (
            "The screen glow is off, so this command will not take focus or send input".to_string(),
            "From an interactive console, run `otherhand glow set --on` to turn the warning back on",
            "off",
            "person_off".to_string(),
        ),
        warn::Block::NotShown { detail } => (
            format!("The screen glow could not be shown ({detail}), so this command was not run"),
            "The glow helper has to draw a visible frame before focus or input. Retry once the desktop can show it",
            "unavailable",
            detail,
        ),
    };
    AdapterError::new(ErrorCode::GlowUnavailable, message)
        .with_suggestion(suggestion.to_string())
        .with_details(serde_json::json!({
            "glow": { "shown": false, "mode": mode, "error": detail }
        }))
        .with_disposition(DeliverySemantics::not_delivered())
        .into()
}

/// Puts `glow` on a mutating command's result. The hold ends when this
/// returns, after the command body has finished.
pub(crate) fn attach(
    result: Result<Value, AppError>,
    prepared: Prepared,
) -> Result<Value, AppError> {
    let note = prepared.report.json();
    let attached = match result {
        Ok(Value::Object(mut map)) => {
            map.insert("glow".into(), note);
            Ok(Value::Object(map))
        }
        Ok(other) => Ok(other),
        Err(error) => Err(attach_error(error, note)),
    };
    drop(prepared);
    attached
}

fn attach_error(error: AppError, note: Value) -> AppError {
    use agent_desktop_core::{AdapterError, ErrorCode};
    let mut adapter = match error {
        AppError::Adapter(adapter) => adapter,
        other => {
            let code = if other.code() == ErrorCode::PermDenied.as_str() {
                ErrorCode::PermDenied
            } else {
                ErrorCode::Internal
            };
            AdapterError::new(code, other.to_string())
        }
    };
    let details = match adapter.details.take() {
        Some(Value::Object(mut map)) => {
            map.insert("glow".into(), note);
            Value::Object(map)
        }
        Some(other) => serde_json::json!({ "detail": other, "glow": note }),
        None => serde_json::json!({ "glow": note }),
    };
    adapter.details = Some(details);
    AppError::Adapter(adapter)
}

fn platform_show(cfg: &GlowConfig) -> warn::ShowOutcome {
    #[cfg(test)]
    {
        if let Some(scripted) = scripted_show() {
            return scripted;
        }
    }
    #[cfg(target_os = "windows")]
    {
        return match windows_warn::client_warn(cfg) {
            Ok(ack) => warn::ShowOutcome::Shown(ack),
            Err(error) => warn::ShowOutcome::Unavailable { error },
        };
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = cfg;
        warn::ShowOutcome::Unsupported
    }
}

#[cfg(test)]
fn scripted_show() -> Option<warn::ShowOutcome> {
    warn::scripted_outcome()
}

fn wait_countdown(ms: u64) {
    #[cfg(test)]
    {
        warn::note_wait(ms);
        if !warn::live_wait() {
            return;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(ms));
}

#[cfg(target_os = "windows")]
fn platform_end() {
    windows::client_end();
}
#[cfg(not(target_os = "windows"))]
fn platform_end() {}

/// Ends the work session now. The frame then fades for `linger_ms` only if
/// no acting command is still running.
pub(crate) fn release_work() {
    platform_release();
}

#[cfg(target_os = "windows")]
fn platform_release() {
    windows::client_release();
}
#[cfg(not(target_os = "windows"))]
fn platform_release() {}

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
/// helper: color in bits 0-23, a "set" mark in bit 24, thickness low 5 bits
/// in bits 25-29, "require capture exclusion" in bit 30, thickness bit 5
/// (32 and above) in bit 31, the linger in bits 32-47, and the countdown in
/// bits 48-61.
pub(crate) fn pack_config(cfg: &GlowConfig) -> u64 {
    let thickness = clamp_thickness(cfg.thickness);
    u64::from(cfg.color & 0xFF_FFFF)
        | 1 << 24
        | u64::from(thickness & 0x1F) << 25
        | u64::from(cfg.strict) << 30
        | u64::from(thickness >> 5) << 31
        | cfg.linger_ms.min(10_000) << 32
        | warn::clamp_countdown(cfg.countdown_ms) << 48
}

pub(crate) fn unpack_config(v: u64) -> Option<GlowConfig> {
    if v & (1 << 24) == 0 {
        return None;
    }
    Some(GlowConfig {
        enabled: true,
        color: (v & 0xFF_FFFF) as u32,
        thickness: clamp_thickness((((v >> 25) & 0x1F) as u32) | ((((v >> 31) & 0x1) as u32) << 5)),
        linger_ms: ((v >> 32) & 0xFFFF).min(10_000),
        countdown_ms: warn::countdown_from_packed(v),
        strict: v & (1 << 30) != 0,
    })
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
            format_color(self.config.color)
                .trim_start_matches('#')
                .to_string(),
            "--thickness".into(),
            self.config.thickness.to_string(),
            "--linger".into(),
            self.config.linger_ms.to_string(),
        ]);
        if self.config.strict {
            out.extend(["--strict".into(), "1".into()]);
        }
        out
    }

    pub(crate) fn parse(args: &[String]) -> Self {
        let mut out = Self {
            first_pid: None,
            config: GlowConfig::default(),
        };
        let mut it = args.iter();
        while let Some(flag) = it.next() {
            let Some(value) = it.next() else { break };
            match flag.as_str() {
                "--pid" => out.first_pid = value.parse().ok(),
                "--color" => out.config.color = parse_color(value).unwrap_or(DEFAULT_COLOR),
                "--thickness" => {
                    out.config.thickness = value
                        .parse::<u32>()
                        .map_or(DEFAULT_THICKNESS, clamp_thickness)
                }
                "--linger" => {
                    out.config.linger_ms = value
                        .parse::<u64>()
                        .map_or(DEFAULT_LINGER_MS, |l| l.min(10_000))
                }
                "--strict" => out.config.strict = value == "1",
                _ => {}
            }
        }
        out
    }
}

/// Which command processes are acting, and the work session that stays open
/// between them. `linger_until` is only the fade after the session releases.
#[derive(Debug, Default)]
pub(crate) struct Holders {
    active: BTreeMap<u32, u32>,
    linger_until: Option<u64>,
    last_visible_at: u64,
    linger_ms: u64,
    work: work::WorkSession,
    countdown_until: Option<u64>,
    countdown_ms: u64,
}

impl Holders {
    pub(crate) fn new(linger_ms: u64, now: u64) -> Self {
        Self {
            linger_ms,
            last_visible_at: now,
            ..Self::default()
        }
    }

    pub(crate) fn set_linger(&mut self, linger_ms: u64) {
        self.linger_ms = linger_ms;
    }

    pub(crate) fn arm_countdown(&mut self, now: u64, countdown_ms: u64) {
        let countdown_ms = warn::clamp_countdown(countdown_ms);
        self.countdown_ms = countdown_ms;
        self.countdown_until = Some(now.saturating_add(countdown_ms));
    }

    pub(crate) fn frame_alpha(&self, now: u64) -> u8 {
        let pulse = pulse_alpha(now);
        let Some(until) = self.countdown_until else {
            return pulse;
        };
        let total = self.countdown_ms;
        let elapsed = total.saturating_sub(until.saturating_sub(now));
        warn::countdown_brightness(pulse, elapsed, total)
    }

    pub(crate) fn session_open(&self, now: u64) -> bool {
        !self.active.is_empty() || self.work.is_open(now)
    }

    pub(crate) fn begin(&mut self, pid: u32) {
        *self.active.entry(pid).or_insert(0) += 1;
        self.linger_until = None;
        self.work.begin();
    }

    pub(crate) fn end(&mut self, pid: u32, now: u64) {
        if let Some(n) = self.active.get_mut(&pid) {
            *n -= 1;
            if *n == 0 {
                self.active.remove(&pid);
            }
        }
        self.settle(now);
    }

    /// Drops holders whose process is gone (a command that exited without end).
    pub(crate) fn prune(&mut self, now: u64, alive: impl Fn(u32) -> bool) {
        let before = self.active.len();
        self.active.retain(|pid, _| alive(*pid));
        if self.active.len() != before {
            self.settle(now);
        }
    }

    /// The agent ended the session. Fade now if nothing is still acting.
    pub(crate) fn release(&mut self, now: u64) {
        self.work.release();
        if self.active.is_empty() {
            self.start_fade(now);
        }
    }

    fn settle(&mut self, now: u64) {
        if !self.active.is_empty() {
            return;
        }
        if self.work.is_released() {
            self.start_fade(now);
            return;
        }
        self.work.clients_idle(now, SESSION_IDLE_MS);
    }

    fn start_fade(&mut self, now: u64) {
        if self.linger_until.is_none() {
            self.linger_until = Some(now.saturating_add(self.linger_ms));
        }
    }

    #[cfg(test)]
    pub(crate) fn pids(&self) -> Vec<u32> {
        self.active.keys().copied().collect()
    }

    /// Whether the frame should be on screen now.
    pub(crate) fn visible(&mut self, now: u64) -> bool {
        if self.active.is_empty() {
            if let Some(released_at) = self.work.take_expired(now) {
                self.start_fade(released_at);
            }
        }
        let on = self.session_open(now) || self.linger_until.is_some_and(|t| now < t);
        if on {
            self.last_visible_at = now;
        } else {
            self.linger_until = None;
        }
        on
    }

    /// Whether the helper has been idle long enough to leave.
    pub(crate) fn should_exit(&self, now: u64) -> bool {
        !self.session_open(now)
            && self.linger_until.is_none()
            && now.saturating_sub(self.last_visible_at) >= IDLE_EXIT_MS
    }
}

/// Constant alpha for the pulse at `t_ms`: a slow breath between about 60%
/// and full, so the frame is noticeable without blinking.
pub(crate) fn pulse_alpha(t_ms: u64) -> u8 {
    let phase = (t_ms % PULSE_PERIOD_MS) as f64 / PULSE_PERIOD_MS as f64;
    let wave = 0.5 - 0.5 * (phase * std::f64::consts::TAU).cos();
    (155.0 + 100.0 * wave).round().clamp(0.0, 255.0) as u8
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
        Rect {
            x: monitor.x,
            y: monitor.y,
            w: monitor.w,
            h: t,
        },
        Rect {
            x: monitor.x,
            y: monitor.y + monitor.h - t,
            w: monitor.w,
            h: t,
        },
    ];
    if inner > 0 {
        out.push(Rect {
            x: monitor.x,
            y: monitor.y + t,
            w: t,
            h: inner,
        });
        out.push(Rect {
            x: monitor.x + monitor.w - t,
            y: monitor.y + t,
            w: t,
            h: inner,
        });
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
pub(crate) fn render_strip(strip: Rect, monitor: Rect, t: i32, color: u32) -> Vec<u32> {
    let (r, g, b) = ((color >> 16) & 0xFF, (color >> 8) & 0xFF, color & 0xFF);
    let mut out = Vec::with_capacity((strip.w.max(0) * strip.h.max(0)) as usize);
    for row in 0..strip.h {
        let y = strip.y + row - monitor.y;
        for col in 0..strip.w {
            let x = strip.x + col - monitor.x;
            let d = x.min(y).min(monitor.w - 1 - x).min(monitor.h - 1 - y);
            let a = u32::from(edge_alpha(d, t));
            let pm = |c: u32| (c * a + 127) / 255;
            out.push(a << 24 | pm(r) << 16 | pm(g) << 8 | pm(b));
        }
    }
    out
}

#[cfg(test)]
mod tests;
