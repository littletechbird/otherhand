//! The two verified ways `type` puts text into a field without the
//! physical keyboard.
//!
//! A native control with a writable `ValuePattern` gets the whole text in
//! one `SetValue` of its current value plus the typed text, read back
//! before it counts. Chromium fields are left to key messages even when
//! they advertise `ValuePattern`: a page's own script tracks what the user
//! typed through input events, and a value written from outside can show on
//! screen while the page still thinks the field is empty.
//!
//! A Chromium field gets key messages through [`MessageTarget`], driven by
//! core's `text_delivery` engine, which sends one small chunk at a time and
//! reads the field back after each one.

use std::time::{Duration, Instant};

use agent_desktop_core::text_delivery::{ChunkTarget, normalize_for_compare};
use agent_desktop_core::{
    ActionStep, AdapterError, Deadline, DeliverySemantics, ErrorCode, StepMechanism,
};

use crate::actions::background_target as target;
use crate::input::keyboard::keyboard_message::{activation_messages, null_message, text_messages};
use crate::input::keyboard::keyboard_message_send::send_window_messages;
use crate::tree::element::UIAElement;
use crate::tree::text_readback::read_target_text;

/// Characters sent between focus checks inside one chunk.
const FOCUS_CHECK_CHARS: usize = 4;

const SET_VALUE_LABEL: &str = "ValuePattern.SetValue.type_text";
const SET_VALUE_SETTLE: Duration = Duration::from_millis(500);
const STOP_BUDGET_MS: u64 = 1_000;
const POLL: Duration = Duration::from_millis(20);

/// A Chromium text target reached by window messages.
pub(crate) struct MessageTarget<'a> {
    pub(crate) element: &'a UIAElement,
    pub(crate) root: isize,
}

impl ChunkTarget for MessageTarget<'_> {
    fn focus_on_target(&mut self) -> Result<bool, AdapterError> {
        Ok(target::has_keyboard_focus(self.element))
    }

    /// Sends the chunk a few characters at a time and checks focus between
    /// slices. The page applies keys to whatever holds focus when they reach
    /// it, so a focus change found only after the whole chunk would let the
    /// rest of the chunk land somewhere else.
    fn send(&mut self, chunk: &str, deadline: Deadline) -> Result<(), AdapterError> {
        let mut sent_chars = 0_usize;
        for slice in focus_check_slices(chunk) {
            if sent_chars > 0 && !target::has_keyboard_focus(self.element) {
                return Err(AdapterError::new(
                    ErrorCode::FocusStolen,
                    "Keyboard focus left the target while a chunk was being sent",
                )
                .with_details(serde_json::json!({ "chars_sent_in_chunk": sent_chars })));
            }
            let messages = text_messages(slice);
            let delivered = send_window_messages(self.root, &messages, deadline);
            if delivered < messages.len() {
                return Err(AdapterError::new(
                    ErrorCode::AppUnresponsive,
                    "The window stopped accepting key messages",
                )
                .with_details(serde_json::json!({
                    "delivered_messages": delivered,
                    "planned_messages": messages.len(),
                    "chars_sent_in_chunk": sent_chars,
                })));
            }
            sent_chars += slice.chars().count();
        }
        Ok(())
    }

    fn read_back(&mut self, deadline: Deadline) -> Result<Option<String>, AdapterError> {
        read_target_text(self.element, deadline)
    }

    /// Tells the window it is no longer active, so Chromium drops any key
    /// message that still reaches it, then waits until the window's thread
    /// has handled everything sent so far. A `SendMessageTimeout` that timed
    /// out leaves its message queued, and the window would otherwise apply it
    /// later, to whatever element holds focus by then.
    fn stop_pending_input(&mut self) {
        let Ok(deadline) = Deadline::detached_after(STOP_BUDGET_MS) else {
            return;
        };
        if !target::is_foreground(self.root) {
            send_window_messages(self.root, &activation_messages(false), deadline);
        }
        send_window_messages(self.root, &[null_message()], deadline);
    }
}

/// Types into a native control by `SetValue`, when it has a writable,
/// readable `ValuePattern` and the typed text belongs at the end, which is
/// where `type` puts it in a field that already holds text. Returns `None`
/// when the control does not qualify or refused the write without changing,
/// so the caller can fall back to keys.
pub(crate) fn type_by_set_value(
    element: &UIAElement,
    text: &str,
    deadline: Deadline,
) -> Result<Option<ActionStep>, AdapterError> {
    if target::chromium_root(element, deadline)?.is_some() || !target::value_writable(element) {
        return Ok(None);
    }
    let Some(before) = target::pattern_value(element) else {
        return Ok(None);
    };
    if !before.is_empty() && !target::caret_at_end(element) {
        return Ok(None);
    }
    let wanted = format!("{before}{text}");
    if !target::set_pattern_value(element, &wanted) {
        return Ok(None);
    }
    let wanted_normalized = normalize_for_compare(&wanted);
    let started = Instant::now();
    loop {
        let now = target::pattern_value(element);
        if now.as_deref().map(normalize_for_compare).as_deref() == Some(&wanted_normalized) {
            return Ok(Some(
                ActionStep::succeeded(SET_VALUE_LABEL)
                    .with_mechanism(StepMechanism::SemanticApi)
                    .with_verified(true),
            ));
        }
        if started.elapsed() >= SET_VALUE_SETTLE || deadline.is_expired() {
            if now.as_deref() == Some(before.as_str()) {
                return Ok(None);
            }
            return Err(AdapterError::new(
                ErrorCode::ActionFailed,
                "The field took the write but now holds something other than what was typed",
            )
            .with_suggestion("Read the field with 'get' before going on.")
            .with_disposition(DeliverySemantics::delivered_unverified()));
        }
        std::thread::sleep(POLL.min(deadline.remaining()));
    }
}

/// Splits a chunk into slices of about [`FOCUS_CHECK_CHARS`] characters,
/// never between a CR and the LF after it, which type as one line break.
fn focus_check_slices(chunk: &str) -> Vec<&str> {
    let mut slices = Vec::new();
    let mut start = 0;
    let mut count = 0;
    let mut previous = '\0';
    for (index, character) in chunk.char_indices() {
        if count >= FOCUS_CHECK_CHARS && !(previous == '\r' && character == '\n') {
            slices.push(&chunk[start..index]);
            start = index;
            count = 0;
        }
        count += 1;
        previous = character;
    }
    if start < chunk.len() {
        slices.push(&chunk[start..]);
    }
    slices
}

#[cfg(test)]
#[path = "verified_type_tests.rs"]
mod tests;
