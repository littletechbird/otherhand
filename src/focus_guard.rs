//! Foreground guard and shield.
//!
//! Before every headless command the guard records the foreground window and
//! the real cursor. While the command runs a shield keeps the foreground
//! where it was: a foreground WinEvent hook (out of context) and a 1 ms
//! sampler both put the person's window straight back with the
//! AttachThreadInput + SetForegroundWindow technique when anything else takes
//! it. The person's own window switches are never fought: a real (not
//! injected) mouse button press or Alt / Win / Tab key press shortly before
//! the change turns the shield off for that command.
//!
//! The guard still reports every change. A change the shield undid within
//! [`RESTORE_BUDGET_MS`] comes back as `focus_guard.status = "RESTORED"` on
//! the result; anything else fails the command with FOCUS_STOLEN so a batch
//! stops at the step that caused it. A moved cursor is put back first.
//! `--headed` turns all of this off, because headed input takes the
//! foreground on purpose.

use agent_desktop_core::{AdapterError, AppError, DeliverySemantics, ErrorCode};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Longest single stretch the person's window may be out of the foreground
/// for a command to count as RESTORED rather than STOLEN.
pub(crate) const RESTORE_BUDGET_MS: u64 = 250;

/// A real click or window-switch key this recent before a foreground change
/// marks the change as the person's own.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) const PERSON_SWITCH_WINDOW_MS: u32 = 600;

/// Counts input this process sent itself (mouse, keys, drags). Fresh
/// "last input" time is only taken as the person's own when this did not
/// move, because injected input updates that time too.
static TOOL_INPUT: AtomicU64 = AtomicU64::new(0);

pub(crate) fn note_tool_input() {
    TOOL_INPUT.fetch_add(1, Ordering::SeqCst);
}

fn tool_input_count() -> u64 {
    TOOL_INPUT.load(Ordering::SeqCst)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Observation {
    pub foreground: isize,
    pub cursor: (i32, i32),
    pub last_input_tick: u32,
}

/// An observation and when the sampler took it, in microseconds since the
/// guard started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timed {
    pub at_us: u64,
    pub obs: Observation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Change {
    pub foreground_changed: bool,
    pub cursor_moved: bool,
    pub user_input_seen: bool,
    pub tool_input: bool,
}

impl Change {
    pub(crate) fn stolen(&self) -> bool {
        self.foreground_changed || self.cursor_moved
    }
}

#[cfg(test)]
pub(crate) fn compare(before: &Observation, after: &Observation) -> Change {
    fold(before, &[], after, false)
}

/// Folds every sample taken while the command ran. A foreground that
/// changed and was put back before the end still counts as changed.
pub(crate) fn fold(
    before: &Observation,
    samples: &[Observation],
    after: &Observation,
    tool_input: bool,
) -> Change {
    let all = || samples.iter().chain(std::iter::once(after));
    Change {
        foreground_changed: all().any(|o| o.foreground != before.foreground),
        cursor_moved: all().any(|o| o.cursor != before.cursor),
        user_input_seen: all().any(|o| o.last_input_tick != before.last_input_tick),
        tool_input,
    }
}

/// A headless command sends no real input, so a cursor move alongside fresh
/// input with an unchanged foreground is the person, not the tool, unless
/// the tool itself sent input during the command.
pub(crate) fn user_only(change: Change) -> bool {
    change.cursor_moved
        && change.user_input_seen
        && !change.foreground_changed
        && !change.tool_input
}

/// How long the person's window was out of the foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Away {
    /// Number of separate stretches away.
    pub stretches: u32,
    pub longest_ms: u64,
    pub total_ms: u64,
    /// The person's window is in front again at the end.
    pub back: bool,
}

/// Measures the away stretches from the timed samples. A stretch still open
/// at the end, or a change seen only after the last sample, runs to `end_us`.
pub(crate) fn away(
    before: &Observation,
    samples: &[Timed],
    after: &Observation,
    end_us: u64,
) -> Away {
    let mut out = Away {
        back: after.foreground == before.foreground,
        ..Default::default()
    };
    let mut open: Option<u64> = None;
    let close = |start: u64, stop: u64, out: &mut Away| {
        let ms = stop.saturating_sub(start).div_ceil(1000);
        out.stretches += 1;
        out.total_ms += ms;
        out.longest_ms = out.longest_ms.max(ms);
    };
    for s in samples {
        let gone = s.obs.foreground != before.foreground;
        match (gone, open) {
            (true, None) => open = Some(s.at_us),
            (false, Some(start)) => {
                close(start, s.at_us, &mut out);
                open = None;
            }
            _ => {}
        }
    }
    if let Some(start) = open {
        close(start, end_us, &mut out);
    } else if !out.back {
        close(end_us, end_us, &mut out);
    }
    out
}

/// What the shield did during one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ShieldReport {
    pub restore_attempts: u32,
    pub person_switch: bool,
    pub lock_held: bool,
    pub hook_installed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    Clean,
    /// The person moved the mouse; nothing to report.
    PersonOnly,
    /// The foreground moved and the shield put it back within budget.
    Restored,
    Stolen,
}

pub(crate) fn decide(change: Change, away: Away, shield: ShieldReport) -> Verdict {
    if !change.stolen() {
        return Verdict::Clean;
    }
    if user_only(change) {
        return Verdict::PersonOnly;
    }
    if change.foreground_changed
        && !change.cursor_moved
        && !shield.person_switch
        && away.back
        && away.longest_ms <= RESTORE_BUDGET_MS
    {
        return Verdict::Restored;
    }
    Verdict::Stolen
}

/// A real input event this recent is the person switching windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_person_switch(now_tick: u32, last_switch_tick: Option<u32>) -> bool {
    last_switch_tick.is_some_and(|t| now_tick.wrapping_sub(t) <= PERSON_SWITCH_WINDOW_MS)
}

pub(crate) fn stolen_error(cmd: &str, change: Change, cursor_restored: bool) -> AppError {
    stolen_error_with(
        cmd,
        change,
        cursor_restored,
        Away::default(),
        ShieldReport::default(),
    )
}

fn stolen_error_with(
    cmd: &str,
    change: Change,
    cursor_restored: bool,
    away: Away,
    shield: ShieldReport,
) -> AppError {
    crate::fence::audit::append(
        "focus_stolen",
        &[
            ("action", cmd),
            (
                "foreground_changed",
                if change.foreground_changed {
                    "true"
                } else {
                    "false"
                },
            ),
            (
                "cursor_moved",
                if change.cursor_moved { "true" } else { "false" },
            ),
            (
                "foreground_restored",
                if away.back { "true" } else { "false" },
            ),
        ],
    );
    AdapterError::new(
        ErrorCode::FocusStolen,
        format!("'{cmd}' changed the foreground window or moved the cursor without --headed"),
    )
    .with_details(json!({
        "guard": "FOCUS_STOLEN",
        "status": "STOLEN",
        "command": cmd,
        "foreground_changed": change.foreground_changed,
        "foreground_restored": change.foreground_changed && away.back,
        "away_ms": away.longest_ms,
        "away_total_ms": away.total_ms,
        "restore_attempts": shield.restore_attempts,
        "restore_budget_ms": RESTORE_BUDGET_MS,
        "person_switch": shield.person_switch,
        "cursor_moved": change.cursor_moved,
        "cursor_restored": cursor_restored,
        "user_input_seen": change.user_input_seen,
    }))
    .with_suggestion("Stop here. This control or app is not safe to drive headless; use another route or ask the user for --headed.")
    .with_disposition(DeliverySemantics::uncertain())
    .into()
}

/// The note a RESTORED command carries on its result.
pub(crate) fn restored_note(away: Away, shield: ShieldReport) -> Value {
    json!({
        "status": "RESTORED",
        "foreground_changed": true,
        "foreground_restored": true,
        "away_ms": away.longest_ms,
        "away_total_ms": away.total_ms,
        "stretches": away.stretches,
        "restore_attempts": shield.restore_attempts,
        "restore_budget_ms": RESTORE_BUDGET_MS,
    })
}

/// Puts the guard's note on the result, success or failure. A command that
/// failed (a refusal, a timeout) can still have moved the foreground before
/// it failed, and sandbox run 3 lost exactly that note: the error went out
/// untagged. Errors carry it under `error.details.focus_guard`.
fn annotate(result: Result<Value, AppError>, note: Value) -> Result<Value, AppError> {
    match result {
        Ok(Value::Object(mut map)) => {
            map.insert("focus_guard".into(), note);
            Ok(Value::Object(map))
        }
        Ok(other) => Ok(other),
        Err(error) => Err(annotate_error(error, note)),
    }
}

fn annotate_error(error: AppError, note: Value) -> AppError {
    let mut adapter = match error {
        AppError::Adapter(a) => a,
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
            map.insert("focus_guard".into(), note);
            Value::Object(map)
        }
        Some(other) => json!({ "detail": other, "focus_guard": note }),
        None => json!({ "focus_guard": note }),
    };
    adapter.details = Some(details);
    AppError::Adapter(adapter)
}

fn read_foreground() -> ForegroundRead {
    #[cfg(not(target_os = "windows"))]
    {
        ForegroundRead::Unsupported
    }
    #[cfg(target_os = "windows")]
    {
        match platform::observe() {
            Some(obs) if obs.foreground != 0 => ForegroundRead::Seen,
            _ => ForegroundRead::Blind,
        }
    }
}

fn blind_error(cmd: &str) -> AppError {
    AdapterError::new(
        ErrorCode::FocusStolen,
        format!("'{cmd}' was not run because the foreground could not be watched"),
    )
    .with_details(json!({
        "guard": "FOCUS_STOLEN",
        "status": "BLIND",
        "command": cmd,
    }))
    .with_suggestion("The focus guard refuses a headless command when it cannot read the foreground or start its sampler.")
    .with_disposition(DeliverySemantics::not_delivered())
    .into()
}

/// Whether the target window is the foreground. `use-type` for a secret
/// requires this, so keystrokes cannot land in another window.
pub(crate) fn window_is_foreground(window_id: Option<&str>) -> bool {
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window_id;
        false
    }
    #[cfg(target_os = "windows")]
    {
        let Some(id) = window_id else { return false };
        let Some(raw) = id.strip_prefix("w-") else {
            return false;
        };
        let Ok(hwnd) = raw.parse::<usize>() else {
            return false;
        };
        platform::observe()
            .is_some_and(|obs| obs.foreground != 0 && obs.foreground as usize == hwnd)
    }
}

fn settle_ms(cmd: &str) -> u64 {
    match cmd {
        "launch" => 400,
        "click" | "set-value" | "type" | "select" | "toggle" | "check" | "uncheck" | "expand"
        | "collapse" | "close-app" | "fill-secret" | "scroll" => 120,
        _ => 0,
    }
}

/// What a foreground read means for the guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ForegroundRead {
    Seen,
    /// This platform has a guard, and it could not read the foreground.
    Blind,
    /// This platform does not implement the guard.
    Unsupported,
}

/// A headless focus-taking command refuses when the guard cannot see the
/// foreground or cannot start its sampler. Reads and platforms without a
/// guard are unchanged.
pub(crate) fn blocks_when_blind(
    command: &str,
    read: ForegroundRead,
    sampler_started: bool,
) -> bool {
    if !crate::glow::warn::warning_required(command) {
        return false;
    }
    match read {
        ForegroundRead::Unsupported => false,
        ForegroundRead::Blind => true,
        ForegroundRead::Seen => !sampler_started,
    }
}

/// Runs `run` under the guard. Commands that hold other commands (batch)
/// are guarded per entry by the dispatcher and once more as a whole.
pub(crate) fn guard(
    cmd: &str,
    headed: bool,
    run: impl FnOnce() -> Result<Value, AppError>,
) -> Result<Value, AppError> {
    if headed
        || matches!(
            cmd,
            "version" | "skills" | "trace" | "session" | "cursor-overlay"
        )
    {
        return run();
    }
    let before = match read_foreground() {
        ForegroundRead::Unsupported => return run(),
        ForegroundRead::Blind => {
            if blocks_when_blind(cmd, ForegroundRead::Blind, true) {
                return Err(blind_error(cmd));
            }
            return run();
        }
        ForegroundRead::Seen => match platform::observe() {
            Some(obs) => obs,
            None => return Err(blind_error(cmd)),
        },
    };
    let started = Instant::now();
    let input_before = tool_input_count();
    let shield = platform::Shield::raise(before.foreground);
    let sampler = Sampler::start(before, started);
    if !sampler.armed() && blocks_when_blind(cmd, ForegroundRead::Seen, false) {
        return Err(blind_error(cmd));
    }
    let result = run();
    let pause = settle_ms(cmd);
    if pause > 0 {
        std::thread::sleep(std::time::Duration::from_millis(pause));
    }
    let samples = sampler.stop();
    shield.restore_now();
    let report = shield.lower();
    let Some(after) = platform::observe() else {
        return Err(stolen_error(
            cmd,
            Change {
                foreground_changed: true,
                ..Default::default()
            },
            false,
        ));
    };
    let end_us = started.elapsed().as_micros() as u64;
    let plain: Vec<Observation> = samples.iter().map(|t| t.obs).collect();
    let change = fold(&before, &plain, &after, tool_input_count() != input_before);
    let gone = away(&before, &samples, &after, end_us);
    match decide(change, gone, report) {
        Verdict::Clean | Verdict::PersonOnly => result,
        Verdict::Restored => {
            crate::fence::audit::append(
                "focus_restored",
                &[("action", cmd), ("away_ms", &gone.longest_ms.to_string())],
            );
            annotate(result, restored_note(gone, report))
        }
        Verdict::Stolen => {
            let restored = change.cursor_moved
                && (!change.user_input_seen || change.tool_input)
                && platform::set_cursor(before.cursor);
            Err(stolen_error_with(cmd, change, restored, gone, report))
        }
    }
}

/// Polls the foreground and cursor about once a millisecond on its own
/// thread while the command runs, keeping each distinct state it saw, and
/// asks the shield to restore the foreground the moment it sees it move.
struct Sampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<Vec<Timed>>>,
    armed: bool,
}

impl Sampler {
    fn start(first: Observation, started: Instant) -> Self {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("focus-guard".into())
            .spawn(move || {
                let mut seen: Vec<Timed> = Vec::new();
                let mut last = first;
                while !flag.load(Ordering::SeqCst) {
                    if let Some(o) = platform::observe() {
                        if o.foreground != first.foreground {
                            platform::restore_from_sampler(first.foreground);
                        }
                        if o != last {
                            if seen.len() < 4096 {
                                seen.push(Timed {
                                    at_us: started.elapsed().as_micros() as u64,
                                    obs: o,
                                });
                            }
                            last = o;
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                seen
            })
            .ok();
        let armed = thread.is_some();
        Self {
            stop,
            thread,
            armed,
        }
    }

    fn armed(&self) -> bool {
        self.armed
    }

    fn stop(mut self) -> Vec<Timed> {
        self.stop.store(true, Ordering::SeqCst);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

#[cfg(target_os = "windows")]
#[path = "focus_shield_windows.rs"]
mod platform;

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{Observation, ShieldReport};
    pub(super) fn observe() -> Option<Observation> {
        None
    }
    pub(super) fn set_cursor(_at: (i32, i32)) -> bool {
        false
    }
    pub(super) fn restore_from_sampler(_target: isize) {}
    pub(super) struct Shield;
    impl Shield {
        pub(super) fn raise(_target: isize) -> Self {
            Shield
        }
        pub(super) fn restore_now(&self) {}
        pub(super) fn lower(self) -> ShieldReport {
            ShieldReport::default()
        }
    }
}

#[cfg(test)]
#[path = "focus_guard_tests.rs"]
mod tests;
