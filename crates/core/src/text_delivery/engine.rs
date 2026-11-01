//! The verified chunked typing loop. See the module docs in `mod.rs`.

use std::time::Duration;

use serde_json::json;

use super::chunks::{DEFAULT_MAX_CHUNK_CHARS, plan_chunks};
use super::compare::{landed, normalize_for_compare};
use crate::wait::deadline::sleep;
use crate::{AdapterError, Deadline, DeliverySemantics, ErrorCode};

/// The `details.kind` every failure from this module carries.
pub const TYPE_NOT_VERIFIED_KIND: &str = "type_not_verified";

/// What a platform has to provide for verified typing.
pub trait ChunkTarget {
    /// Whether keyboard focus is on the target element right now.
    fn focus_on_target(&mut self) -> Result<bool, AdapterError>;
    /// Sends one chunk. An `Err` means some of it may have been sent. An
    /// `Err` with [`ErrorCode::FocusStolen`] means the sender saw focus leave
    /// the target part way through the chunk and stopped there.
    fn send(&mut self, chunk: &str, deadline: Deadline) -> Result<(), AdapterError>;
    /// The target's whole current text, or `None` when it cannot be read.
    fn read_back(&mut self, deadline: Deadline) -> Result<Option<String>, AdapterError>;
    /// Makes sure nothing sent so far is applied later. Called before every
    /// failure that follows a send.
    fn stop_pending_input(&mut self);
}

/// Timing for the loop. The defaults suit a real desktop; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct ChunkPolicy {
    pub max_chunk_chars: usize,
    /// How long one chunk may take to show up in the readback.
    pub land_budget: Duration,
    /// Added to `land_budget` for every character in the chunk.
    pub land_budget_per_char: Duration,
    pub poll: Duration,
    /// How long the text must stay unchanged after the last chunk lands
    /// before the command reports success. Keys that turn up late, or text
    /// that keeps arriving from somewhere else, show up here.
    pub quiet_window: Duration,
}

impl Default for ChunkPolicy {
    fn default() -> Self {
        Self {
            max_chunk_chars: DEFAULT_MAX_CHUNK_CHARS,
            land_budget: Duration::from_millis(1_500),
            land_budget_per_char: Duration::from_millis(10),
            poll: Duration::from_millis(25),
            quiet_window: Duration::from_millis(300),
        }
    }
}

/// What a verified run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkedTypeReport {
    pub chunks: usize,
    pub chars: usize,
    pub final_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    ReadbackUnavailable,
    FocusNotOnTarget,
    FocusMoved,
    NotLanded,
    SendFailed,
    ChangedAfterTyping,
    Timeout,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReadbackUnavailable => "readback_unavailable",
            Self::FocusNotOnTarget => "focus_not_on_target",
            Self::FocusMoved => "focus_moved",
            Self::NotLanded => "not_landed",
            Self::SendFailed => "send_failed",
            Self::ChangedAfterTyping => "changed_after_typing",
            Self::Timeout => "timeout",
        }
    }
}

struct Progress {
    planned: usize,
    verified_chunks: usize,
    verified_chars: usize,
    sent_unverified: bool,
}

/// Types `text` into `target` one chunk at a time and returns only once the
/// whole text has been read back from the target.
///
/// Before each chunk, focus must be on the target. After each chunk, the
/// readback must show everything typed so far as one unbroken run; focus is
/// checked again on every poll while it settles. After the last chunk the
/// text must then hold still for `quiet_window`. Any failure stops sending
/// at once, asks the target to drop anything still queued, and returns an
/// `Err` whose details say how far typing got and why it stopped.
pub fn type_in_verified_chunks(
    target: &mut dyn ChunkTarget,
    text: &str,
    policy: ChunkPolicy,
    deadline: Deadline,
) -> Result<ChunkedTypeReport, AdapterError> {
    let chunks = plan_chunks(text, policy.max_chunk_chars);
    let mut progress = Progress {
        planned: chunks.len(),
        verified_chunks: 0,
        verified_chars: 0,
        sent_unverified: false,
    };
    let Some(baseline) = target.read_back(deadline)? else {
        return Err(failure(Reason::ReadbackUnavailable, &progress, None));
    };
    let mut typed = String::new();
    let mut last_read = baseline.clone();
    for chunk in &chunks {
        if deadline.is_expired() {
            return Err(failure(Reason::Timeout, &progress, None));
        }
        if !target.focus_on_target()? {
            let reason = if progress.verified_chunks == 0 {
                Reason::FocusNotOnTarget
            } else {
                Reason::FocusMoved
            };
            return Err(failure(reason, &progress, None));
        }
        progress.sent_unverified = true;
        if let Err(error) = target.send(chunk, deadline) {
            target.stop_pending_input();
            if error.code == ErrorCode::FocusStolen {
                return Err(failure(Reason::FocusMoved, &progress, Some(error)));
            }
            return Err(failure(Reason::SendFailed, &progress, Some(error)));
        }
        typed.push_str(chunk);
        let budget =
            policy.land_budget + policy.land_budget_per_char * chunk.chars().count() as u32;
        match wait_for_landing(target, &baseline, &typed, budget, policy.poll, deadline)? {
            Landing::Landed(observed) => last_read = observed,
            Landing::Failed(reason) => {
                target.stop_pending_input();
                return Err(failure(reason, &progress, None));
            }
        }
        progress.sent_unverified = false;
        progress.verified_chunks += 1;
        progress.verified_chars += chunk.chars().count();
    }
    if !chunks.is_empty() {
        sleep(policy.quiet_window.min(deadline.remaining()));
        let settled = target.read_back(deadline)?;
        let unchanged = settled.as_deref().is_some_and(|settled| {
            normalize_for_compare(settled) == normalize_for_compare(&last_read)
        });
        if !unchanged {
            target.stop_pending_input();
            return Err(failure(Reason::ChangedAfterTyping, &progress, None));
        }
    }
    Ok(ChunkedTypeReport {
        chunks: chunks.len(),
        chars: text.chars().count(),
        final_text: last_read,
    })
}

enum Landing {
    Landed(String),
    Failed(Reason),
}

fn wait_for_landing(
    target: &mut dyn ChunkTarget,
    baseline: &str,
    typed: &str,
    budget: Duration,
    poll: Duration,
    deadline: Deadline,
) -> Result<Landing, AdapterError> {
    let chunk_deadline = deadline.capped(budget);
    loop {
        if let Some(observed) = target.read_back(deadline)?
            && landed(baseline, &observed, typed)
        {
            return Ok(Landing::Landed(observed));
        }
        if !target.focus_on_target()? {
            return Ok(Landing::Failed(Reason::FocusMoved));
        }
        if chunk_deadline.remaining() <= poll {
            let reason = if deadline.remaining() <= poll {
                Reason::Timeout
            } else {
                Reason::NotLanded
            };
            return Ok(Landing::Failed(reason));
        }
        sleep(poll);
    }
}

fn failure(reason: Reason, progress: &Progress, cause: Option<AdapterError>) -> AdapterError {
    let (code, message, suggestion) = match reason {
        Reason::ReadbackUnavailable => (
            ErrorCode::ActionNotSupported,
            "This field's text cannot be read back, so typing into it cannot be checked. Nothing was typed",
            "Use 'set-value' if the field supports it, or a headed paste, then read the field with 'get'.",
        ),
        Reason::FocusNotOnTarget => (
            ErrorCode::FocusStolen,
            "Keyboard focus is not on the target, so nothing was typed",
            "Run 'focus' on the ref and check it reports focused, then retry.",
        ),
        Reason::FocusMoved => (
            ErrorCode::FocusStolen,
            "Keyboard focus moved off the target part way through typing, so typing stopped",
            "Read the field with 'get' to see what landed, then type only the rest.",
        ),
        Reason::NotLanded => (
            ErrorCode::ActionFailed,
            "Typed text did not show up in the target, so typing stopped",
            "Read the field with 'get' to see what landed. The editor may be ignoring background input; try 'set-value' or a headed paste.",
        ),
        Reason::SendFailed => (
            ErrorCode::ActionFailed,
            "Sending keys to the target failed part way through",
            "Read the field with 'get' to see what landed, then type only the rest.",
        ),
        Reason::ChangedAfterTyping => (
            ErrorCode::ActionFailed,
            "The field kept changing after typing finished, so the result cannot be trusted",
            "Read the field with 'get' and check it before going on.",
        ),
        Reason::Timeout => (
            ErrorCode::Timeout,
            "Typing ran out of time before the text could be checked",
            "Retry with a longer --timeout, or type a shorter piece.",
        ),
    };
    let disposition = if progress.verified_chunks == 0 && !progress.sent_unverified {
        DeliverySemantics::not_delivered()
    } else {
        DeliverySemantics::delivered_unverified()
    };
    let mut details = json!({
        "kind": TYPE_NOT_VERIFIED_KIND,
        "reason": reason.as_str(),
        "chunks_planned": progress.planned,
        "chunks_verified": progress.verified_chunks,
        "chars_verified": progress.verified_chars,
    });
    if let Some(cause) = cause
        && let Some(object) = details.as_object_mut()
    {
        object.insert("cause".into(), json!(cause.message));
        if let Some(cause_details) = cause.details {
            object.insert("cause_details".into(), cause_details);
        }
    }
    AdapterError::new(code, message)
        .with_suggestion(suggestion)
        .with_details(details)
        .with_disposition(disposition)
}
