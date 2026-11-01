//! Strict-headless `type` for Chromium and Electron windows.
//!
//! `SendInput` needs the target to own the desktop foreground, so the
//! physical leg needs `--headed`. A Chromium top-level window
//! (`Chrome_WidgetWin_1`) also accepts keyboard input as window messages
//! once it has been told it is active, which leaves the foreground window
//! and the physical keyboard alone. The leg activates the window by message,
//! focuses the target, types, and deactivates it again, unless the window
//! became the foreground window in the meantime. Any other window is left to
//! the physical leg, which refuses it under a headless policy.
//!
//! A key message that returns has only reached Chromium's browser thread.
//! The page applies it later, to whichever element has focus by then, and a
//! busy or background page can apply it much later. So the text goes in
//! through core's `text_delivery` engine: small chunks, focus on the target
//! checked before each one, and each one read back from the target before
//! the next leaves. The step reports success only once every chunk has been
//! read back and the field has held still.

use std::time::{Duration, Instant};

use agent_desktop_core::text_delivery::{ChunkPolicy, type_in_verified_chunks};
use agent_desktop_core::{
    ActionStep, AdapterError, Deadline, DeliverySemantics, ErrorCode, StepMechanism,
};

use crate::actions::background_target as target;
use crate::actions::physical_target::{ensure_elevation_allows, read_click_bounds};
use crate::actions::verified_type::MessageTarget;
use crate::input::keyboard::keyboard_message::{activation_messages, click_messages};
use crate::input::keyboard::keyboard_message_send::send_window_messages;
use crate::input::keyboard::preflight_text;
use crate::tree::element::UIAElement;
use crate::tree::live_read::corroborate_verified_process;
use crate::tree::text_readback::read_target_text;

const TYPE_TEXT_LABEL: &str = "WindowMessage.type_text";
const FOCUS_BUDGET: Duration = Duration::from_millis(1_500);
const CARET_BUDGET: Duration = Duration::from_millis(1_000);
const DEACTIVATION_BUDGET_MS: u64 = 1_000;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Types `text` by window messages when `element` lives in a Chromium
/// window, and returns `None` for any other window.
pub(crate) fn background_type_steps(
    element: &UIAElement,
    text: &str,
    deadline: Deadline,
) -> Result<Option<Vec<ActionStep>>, AdapterError> {
    preflight_text(text, deadline)?;
    corroborate_verified_process(element)?;
    let Some(root) = target::chromium_root(element, deadline)? else {
        return Ok(None);
    };
    ensure_elevation_allows(element)?;
    if target::is_minimized(root) {
        return Err(minimized_window());
    }
    let plan = BackgroundTypePlan {
        root,
        was_foreground: target::is_foreground(root),
        text,
    };
    type_text_by_messages(
        plan,
        deadline,
        |deadline| focus_target(element, root, deadline),
        |deadline| {
            let mut message_target = MessageTarget { element, root };
            type_in_verified_chunks(&mut message_target, text, ChunkPolicy::default(), deadline)
                .map(|_| ())
        },
        || !target::is_foreground(root),
    )
    .map(|step| Some(vec![step]))
}

/// What `type_text_by_messages` delivers, bundled so it stays within the
/// parameter cap.
pub(crate) struct BackgroundTypePlan<'a> {
    pub(crate) root: isize,
    pub(crate) was_foreground: bool,
    pub(crate) text: &'a str,
}

/// The message sequence past the window checks, with target focusing, the
/// verified delivery and the foreground re-read injected so a test drives
/// the same code the adapter runs. `deliver` returns `Ok` only once the text
/// has been read back from the target, so the step it produces is verified.
/// Deactivation is sent whenever activation was, even when a later step
/// fails or exhausts `deadline`, so the window is never left believing it is
/// active.
pub(crate) fn type_text_by_messages(
    plan: BackgroundTypePlan<'_>,
    deadline: Deadline,
    focus_target: impl FnOnce(Deadline) -> Result<(), AdapterError>,
    deliver: impl FnOnce(Deadline) -> Result<(), AdapterError>,
    still_background: impl FnOnce() -> bool,
) -> Result<ActionStep, AdapterError> {
    let _ = plan.text;
    let activate = !plan.was_foreground;
    if activate {
        let activation = activation_messages(true);
        let delivered = send_window_messages(plan.root, &activation, deadline);
        if delivered < activation.len() {
            if delivered > 0 {
                deactivate(plan.root);
            }
            return Err(stopped_before_text(deadline));
        }
    }
    let result = focus_target(deadline).and_then(|()| deliver(deadline));
    if activate && still_background() {
        deactivate(plan.root);
    }
    result.map(|()| {
        ActionStep::succeeded(TYPE_TEXT_LABEL)
            .with_mechanism(StepMechanism::PhysicalSynthetic)
            .with_verified(true)
    })
}

/// Runs on its own budget, because a focus or typing step that exhausted
/// the operation deadline would otherwise leave nothing to send it with.
fn deactivate(root: isize) {
    let Ok(deadline) = Deadline::detached_after(DEACTIVATION_BUDGET_MS) else {
        return;
    };
    send_window_messages(root, &activation_messages(false), deadline);
}

/// Focuses the target with a click message on its center, then requires
/// UIA to report keyboard focus on the target itself before anything is
/// typed. A field that already holds text then gets the caret at its end
/// through its UIA text range, and that too has to be confirmed. Either one
/// failing refuses the command with nothing typed: typing at an unconfirmed
/// point is how text ended up spliced into the middle of an earlier
/// paragraph, or in a different field altogether.
fn focus_target(element: &UIAElement, root: isize, deadline: Deadline) -> Result<(), AdapterError> {
    let bounds = read_click_bounds(element)?;
    let center_x = (bounds.x + bounds.width / 2.0).round() as i32;
    let center_y = (bounds.y + bounds.height / 2.0).round() as i32;
    let (x, y) =
        target::screen_to_client(root, center_x, center_y).ok_or_else(window_unresponsive)?;
    let click = click_messages(x, y);
    if send_window_messages(root, &click, deadline) < click.len() {
        return Err(stopped_before_text(deadline));
    }
    let focused = wait_until(deadline, FOCUS_BUDGET, || {
        target::has_keyboard_focus(element)
    });
    if !focused {
        return Err(focus_not_confirmed());
    }
    let holds_text = read_target_text(element, deadline)?
        .is_some_and(|text| !text.is_empty() && Some(text) != target::element_name(element));
    if holds_text {
        let at_end = target::select_end_of_text(element)
            && wait_until(deadline, CARET_BUDGET, || target::caret_at_end(element));
        if !at_end {
            return Err(caret_not_confirmed());
        }
    }
    Ok(())
}

fn wait_until(deadline: Deadline, budget: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    loop {
        if ready() {
            return true;
        }
        if deadline.is_expired() || started.elapsed() >= budget {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.remaining()));
    }
}

fn focus_not_confirmed() -> AdapterError {
    AdapterError::new(
        ErrorCode::FocusStolen,
        "Keyboard focus could not be confirmed on the target, so nothing was typed",
    )
    .with_suggestion("Run 'focus' on the ref and check it reports focused, then retry.")
    .with_disposition(DeliverySemantics::not_delivered())
}

fn caret_not_confirmed() -> AdapterError {
    AdapterError::new(
        ErrorCode::ActionFailed,
        "The caret could not be moved to the end of the field's text, so nothing was typed",
    )
    .with_suggestion("Click at the end of the text with a headed click, then retry.")
    .with_disposition(DeliverySemantics::not_delivered())
}

fn minimized_window() -> AdapterError {
    AdapterError::new(
        ErrorCode::ActionFailed,
        "Chromium does not expose or accept input for a minimized window",
    )
    .with_suggestion("Restore the window, then retry. It can stay behind other windows.")
    .with_disposition(DeliverySemantics::not_delivered())
}

fn window_unresponsive() -> AdapterError {
    AdapterError::new(
        ErrorCode::AppUnresponsive,
        "The target window did not accept input messages",
    )
    .with_suggestion("Run 'snapshot' to check the window, then retry.")
    .with_disposition(DeliverySemantics::not_delivered())
}

fn stopped_before_text(deadline: Deadline) -> AdapterError {
    if deadline.is_expired() {
        return deadline
            .timeout_error()
            .with_disposition(DeliverySemantics::not_delivered());
    }
    window_unresponsive()
}

#[cfg(all(test, target_os = "windows"))]
#[path = "background_keyboard_tests.rs"]
mod tests;
