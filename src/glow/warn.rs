//! Whether a command may take focus or send input, and the countdown it
//! has to show first.
//!
//! The decision is pure. The Windows helper is the only thing that can turn
//! a `Shown` outcome into pixels; this module only says whether that outcome
//! is enough to proceed.

use serde_json::{Value, json};

pub(crate) const DEFAULT_COUNTDOWN_MS: u64 = 1_500;
pub(crate) const MIN_COUNTDOWN_MS: u64 = 1_000;
pub(crate) const MAX_COUNTDOWN_MS: u64 = 10_000;

/// Commands that take the foreground or inject input. Each one has to show
/// the glow and wait out the countdown before it runs.
const WARNING: &[&str] = &[
    "click",
    "double-click",
    "triple-click",
    "right-click",
    "type",
    "set-value",
    "fill-secret",
    "clear",
    "focus",
    "select",
    "toggle",
    "check",
    "uncheck",
    "expand",
    "collapse",
    "scroll",
    "scroll-to",
    "press",
    "key-down",
    "key-up",
    "hover",
    "drag",
    "mouse-move",
    "mouse-click",
    "mouse-down",
    "mouse-up",
    "mouse-wheel",
    "launch",
    "close-app",
    "focus-window",
    "open-system-surface",
    "resize-window",
    "move-window",
    "minimize",
    "maximize",
    "restore",
    "dismiss-notification",
    "dismiss-all-notifications",
    "notification-action",
    "batch",
];

/// Mutations that do not take focus or inject input. They report
/// `not_required` instead of drawing the frame.
const EXEMPT: &[&str] = &[
    "clipboard-set",
    "clipboard-clear",
    "cursor-overlay",
    "session",
    "trace",
    "permissions",
];

/// Reads and other commands that neither glow nor count as an exempt mutation.
const QUIET: &[&str] = &[
    "snapshot",
    "find",
    "screenshot",
    "get",
    "is",
    "list-windows",
    "list-displays",
    "list-apps",
    "list-surfaces",
    "list-notifications",
    "wait",
    "status",
    "version",
    "skills",
    "clipboard-get",
];

pub(crate) fn warning_required(command: &str) -> bool {
    WARNING.contains(&command)
}

pub(crate) fn exempt_mutation(command: &str) -> bool {
    EXEMPT.contains(&command)
}

pub(crate) fn known_command(command: &str) -> bool {
    warning_required(command) || exempt_mutation(command) || QUIET.contains(&command)
}

/// A frame the helper has already put on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameAck {
    pub strips: u32,
    pub min_alpha: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShowOutcome {
    Shown(FrameAck),
    /// The platform has no glow helper. The command may proceed, and the
    /// result says so.
    Unsupported,
    /// The helper did not confirm a visible frame.
    Unavailable {
        error: String,
    },
    /// The person turned the warning off. Focus-taking commands refuse.
    PersonOff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GlowReport {
    pub shown: bool,
    pub mode: &'static str,
    pub error: Option<String>,
    pub countdown_ms: u64,
}

impl GlowReport {
    pub(crate) fn not_required() -> Self {
        Self {
            shown: false,
            mode: "not_required",
            error: None,
            countdown_ms: 0,
        }
    }

    pub(crate) fn json(&self) -> Value {
        let mut doc = json!({
            "shown": self.shown,
            "mode": self.mode,
            "countdown_ms": self.countdown_ms,
        });
        if let Some(error) = &self.error {
            doc["error"] = json!(error);
        }
        doc
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Block {
    PersonOff,
    NotShown { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    pub report: GlowReport,
    pub wait_ms: u64,
    pub block: Option<Block>,
}

pub(crate) fn clamp_countdown(ms: u64) -> u64 {
    ms.clamp(MIN_COUNTDOWN_MS, MAX_COUNTDOWN_MS)
}

/// Brightness while the countdown is running: three visible steps, then the
/// ordinary pulse. `elapsed` and `total` are milliseconds.
pub(crate) fn countdown_brightness(pulse: u8, elapsed: u64, total: u64) -> u8 {
    if total == 0 || elapsed >= total {
        return pulse;
    }
    let third = total / 3;
    let scale = if elapsed < third {
        40
    } else if elapsed < third.saturating_mul(2) {
        70
    } else {
        100
    };
    let scaled = u32::from(pulse) * scale / 100;
    scaled.max(1) as u8
}

pub(crate) fn pack_ack(strips: u32, alpha: u8) -> usize {
    if strips == 0 || alpha == 0 {
        return 0;
    }
    0x4756_0000 | ((strips.min(255) as usize) << 8) | usize::from(alpha)
}

pub(crate) fn parse_ack(value: usize) -> Option<FrameAck> {
    if value >> 16 != 0x4756 {
        return None;
    }
    let strips = ((value >> 8) & 0xFF) as u32;
    let min_alpha = (value & 0xFF) as u8;
    if strips == 0 || min_alpha == 0 {
        return None;
    }
    Some(FrameAck { strips, min_alpha })
}

pub(crate) fn mutex_name(sid: Option<&str>) -> String {
    match sid {
        Some(sid)
            if !sid.is_empty()
                && sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && sid.len() <= 184 =>
        {
            format!("Local\\OtherhandGlowHelper-{sid}")
        }
        _ => "Local\\OtherhandGlowHelper".to_string(),
    }
}

pub(crate) fn same_image(left: &std::path::Path, right: &std::path::Path) -> bool {
    normalize_image(left) == normalize_image(right)
}

fn normalize_image(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    let text = text.trim_start_matches(r"\\?\");
    text.replace('/', "\\").to_ascii_lowercase()
}

pub(crate) fn countdown_from_packed(packed: u64) -> u64 {
    let raw = (packed >> 48) & 0x3FFF;
    if raw == 0 {
        DEFAULT_COUNTDOWN_MS
    } else {
        clamp_countdown(raw)
    }
}

pub(crate) fn decide(outcome: ShowOutcome, configured_ms: u64) -> Decision {
    let countdown_ms = clamp_countdown(configured_ms);
    match outcome {
        ShowOutcome::PersonOff => Decision {
            report: GlowReport {
                shown: false,
                mode: "off",
                error: Some("person_off".into()),
                countdown_ms: 0,
            },
            wait_ms: 0,
            block: Some(Block::PersonOff),
        },
        ShowOutcome::Unsupported => Decision {
            report: GlowReport {
                shown: false,
                mode: "unsupported",
                error: None,
                countdown_ms: 0,
            },
            wait_ms: 0,
            block: None,
        },
        ShowOutcome::Unavailable { error } => Decision {
            report: GlowReport {
                shown: false,
                mode: "unavailable",
                error: Some(error.clone()),
                countdown_ms: 0,
            },
            wait_ms: 0,
            block: Some(Block::NotShown { detail: error }),
        },
        ShowOutcome::Shown(ack) if ack.strips == 0 || ack.min_alpha == 0 => Decision {
            report: GlowReport {
                shown: false,
                mode: "unavailable",
                error: Some("frame had no visible pixels".into()),
                countdown_ms: 0,
            },
            wait_ms: 0,
            block: Some(Block::NotShown {
                detail: "frame had no visible pixels".into(),
            }),
        },
        ShowOutcome::Shown(_) => Decision {
            report: GlowReport {
                shown: true,
                mode: "warning",
                error: None,
                countdown_ms,
            },
            wait_ms: countdown_ms,
            block: None,
        },
    }
}

/// Input is allowed only after a confirmed visible frame and a finished
/// non-zero countdown. `elapsed_ms` is how long the caller has waited since
/// the acknowledgement.
pub(crate) fn input_allowed(ack: Option<FrameAck>, elapsed_ms: u64, countdown_ms: u64) -> bool {
    match ack {
        Some(frame) if frame.strips > 0 && frame.min_alpha > 0 => {
            countdown_ms > 0 && elapsed_ms >= countdown_ms
        }
        _ => false,
    }
}

#[cfg(test)]
#[derive(Clone)]
enum Script {
    Shown,
    Fail(String),
    Live,
}

#[cfg(test)]
thread_local! {
    static SCRIPT: std::cell::RefCell<Script> = std::cell::RefCell::new(Script::Shown);
    static WAITED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn scripted_outcome() -> Option<ShowOutcome> {
    SCRIPT.with(|slot| match &*slot.borrow() {
        Script::Shown => Some(ShowOutcome::Shown(FrameAck {
            strips: 4,
            min_alpha: 200,
        })),
        Script::Fail(error) => Some(ShowOutcome::Unavailable {
            error: error.clone(),
        }),
        Script::Live => None,
    })
}

#[cfg(test)]
pub(crate) fn live_wait() -> bool {
    SCRIPT.with(|slot| matches!(*slot.borrow(), Script::Live))
}

#[cfg(test)]
pub(crate) fn note_wait(ms: u64) {
    WAITED.with(|cell| cell.set(cell.get().saturating_add(ms)));
}

#[cfg(test)]
pub(crate) fn waited_ms() -> u64 {
    WAITED.with(|cell| cell.get())
}

#[cfg(test)]
pub(crate) fn reset_wait() {
    WAITED.with(|cell| cell.set(0));
}

#[cfg(test)]
pub(crate) fn script_fail(error: impl Into<String>) {
    let error = error.into();
    SCRIPT.with(|slot| *slot.borrow_mut() = Script::Fail(error));
}

#[cfg(test)]
pub(crate) fn script_live() {
    SCRIPT.with(|slot| *slot.borrow_mut() = Script::Live);
}

#[cfg(test)]
pub(crate) fn script_shown() {
    SCRIPT.with(|slot| *slot.borrow_mut() = Script::Shown);
}

#[cfg(test)]
#[path = "warn_tests.rs"]
mod tests;
