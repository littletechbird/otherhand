//! A fake editor that misbehaves the ways real ones were seen to: it can
//! apply keys late, stall after a number of characters, move focus to
//! another field part way through, rewrite line breaks and spaces, or let a
//! late key turn up after typing finished.

use std::time::Duration;

use super::{ChunkPolicy, ChunkTarget, TYPE_NOT_VERIFIED_KIND, type_in_verified_chunks};
use crate::wait::deadline::VirtualClock;
use crate::{AdapterError, Deadline, DeliverySemantics, ErrorCode};

#[derive(Default)]
struct FakeEditor {
    text: String,
    other_field: String,
    queued: Vec<String>,
    reads_until_applied: usize,
    reads_since_send: usize,
    stall_after_chars: Option<usize>,
    focus_moves_after_sends: Option<usize>,
    late_key_on_read: Option<(usize, char)>,
    rich_rewrite: bool,
    readable: bool,
    sends: usize,
    reads: usize,
    stops: usize,
    applied_chars: usize,
}

impl FakeEditor {
    fn new() -> Self {
        Self {
            readable: true,
            ..Self::default()
        }
    }

    fn has_focus(&self) -> bool {
        self.focus_moves_after_sends
            .is_none_or(|after| self.sends < after)
    }

    fn apply(&mut self, chunk: &str) {
        let mut chunk = chunk.to_string();
        if self.rich_rewrite {
            chunk = chunk
                .replace("\r\n", "\n")
                .replace('\r', "\n")
                .replace(' ', "\u{a0}");
        }
        if let Some(limit) = self.stall_after_chars {
            let room = limit.saturating_sub(self.applied_chars);
            let kept: String = chunk.chars().take(room).collect();
            let dropped: String = chunk.chars().skip(room).collect();
            self.applied_chars += kept.chars().count();
            self.text.push_str(&kept);
            if !dropped.is_empty() {
                self.queued.push(dropped);
            }
            return;
        }
        self.applied_chars += chunk.chars().count();
        self.text.push_str(&chunk);
    }
}

impl ChunkTarget for FakeEditor {
    fn focus_on_target(&mut self) -> Result<bool, AdapterError> {
        Ok(self.has_focus())
    }

    fn send(&mut self, chunk: &str, _deadline: Deadline) -> Result<(), AdapterError> {
        let focused = self.has_focus();
        self.sends += 1;
        if !focused {
            self.other_field.push_str(chunk);
            return Ok(());
        }
        self.reads_since_send = 0;
        if self.reads_until_applied == 0 {
            self.apply(chunk);
        } else {
            self.queued.push(chunk.to_string());
        }
        Ok(())
    }

    fn read_back(&mut self, _deadline: Deadline) -> Result<Option<String>, AdapterError> {
        self.reads += 1;
        if let Some((at, key)) = self.late_key_on_read
            && self.reads == at
        {
            self.text.push(key);
        }
        if !self.readable {
            return Ok(None);
        }
        if self.reads_until_applied > 0 && self.stall_after_chars.is_none() {
            self.reads_since_send += 1;
            if self.reads_since_send >= self.reads_until_applied {
                for chunk in std::mem::take(&mut self.queued) {
                    self.apply(&chunk);
                }
            }
        }
        Ok(Some(self.text.clone()))
    }

    fn stop_pending_input(&mut self) {
        self.stops += 1;
        self.queued.clear();
    }
}

fn policy() -> ChunkPolicy {
    ChunkPolicy {
        max_chunk_chars: 40,
        land_budget: Duration::from_millis(200),
        land_budget_per_char: Duration::ZERO,
        poll: Duration::from_millis(10),
        quiet_window: Duration::from_millis(50),
    }
}

fn article(words: usize) -> String {
    (0..words)
        .map(|index| format!("word{index}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn run(editor: &mut FakeEditor, text: &str) -> Result<super::ChunkedTypeReport, AdapterError> {
    type_in_verified_chunks(editor, text, policy(), Deadline::after(600_000).unwrap())
}

fn reason(error: &AdapterError) -> String {
    let details = error.details.as_ref().unwrap();
    assert_eq!(details["kind"], TYPE_NOT_VERIFIED_KIND);
    details["reason"].as_str().unwrap().to_string()
}

#[test]
fn an_800_word_article_into_a_well_behaved_editor_is_verified() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    let text = article(800);
    let report = run(&mut editor, &text).unwrap();
    assert_eq!(editor.text, text);
    assert_eq!(report.final_text, text);
    assert!(report.chunks > 100);
    assert_eq!(editor.stops, 0);
}

#[test]
fn a_slow_editor_is_waited_for_chunk_by_chunk() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.reads_until_applied = 5;
    let text = article(120);
    run(&mut editor, &text).unwrap();
    assert_eq!(editor.text, text);
}

#[test]
fn an_editor_that_stalls_after_about_100_words_fails_instead_of_passing() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.stall_after_chars = Some(600);
    let text = article(400);
    let error = run(&mut editor, &text).unwrap_err();
    assert_eq!(reason(&error), "not_landed");
    assert_eq!(error.code, ErrorCode::ActionFailed);
    assert_eq!(error.disposition, DeliverySemantics::delivered_unverified());
    let verified = error.details.as_ref().unwrap()["chars_verified"]
        .as_u64()
        .unwrap();
    assert!(verified <= 600, "{verified}");
    assert!(
        editor.queued.is_empty(),
        "queued keys must be dropped, not left to land late"
    );
    assert_eq!(editor.stops, 1);
    let sent_after_stall = editor.sends;
    assert!(
        sent_after_stall <= 600 / 30 + 2,
        "kept sending after the stall: {sent_after_stall}"
    );
}

#[test]
fn focus_moving_to_another_field_stops_typing_before_the_next_chunk() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.focus_moves_after_sends = Some(3);
    let text = article(100);
    let error = run(&mut editor, &text).unwrap_err();
    assert_eq!(reason(&error), "focus_moved");
    assert_eq!(error.code, ErrorCode::FocusStolen);
    assert!(
        editor.other_field.is_empty(),
        "text went into the wrong field"
    );
    assert_eq!(editor.sends, 3);
}

#[test]
fn focus_that_is_not_on_the_target_at_the_start_types_nothing() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.focus_moves_after_sends = Some(0);
    let error = run(&mut editor, "Hire me").unwrap_err();
    assert_eq!(reason(&error), "focus_not_on_target");
    assert_eq!(error.disposition, DeliverySemantics::not_delivered());
    assert_eq!(editor.sends, 0);
}

#[test]
fn a_field_that_cannot_be_read_back_is_refused_before_any_key_is_sent() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.readable = false;
    let error = run(&mut editor, "hello").unwrap_err();
    assert_eq!(reason(&error), "readback_unavailable");
    assert_eq!(error.disposition, DeliverySemantics::not_delivered());
    assert_eq!(editor.sends, 0);
}

#[test]
fn a_late_key_after_typing_finished_fails_the_command() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    let text = "short and sweet";
    editor.late_key_on_read = Some((3, 'X'));
    let error = run(&mut editor, text).unwrap_err();
    assert_eq!(reason(&error), "changed_after_typing");
    assert_eq!(error.disposition, DeliverySemantics::delivered_unverified());
}

#[test]
fn a_rich_editor_that_rewrites_breaks_and_spaces_still_verifies() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.rich_rewrite = true;
    let text = "First paragraph here.\r\nSecond paragraph, a bit longer than the first one.\r";
    run(&mut editor, text).unwrap();
}

#[test]
fn existing_text_in_the_field_does_not_count_as_typed() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.text = "alpha beta".into();
    editor.stall_after_chars = Some(0);
    let error = run(&mut editor, "alpha").unwrap_err();
    assert_eq!(reason(&error), "not_landed");
}

#[test]
fn running_out_of_time_is_a_timeout_not_a_success() {
    let _clock = VirtualClock::freeze();
    let mut editor = FakeEditor::new();
    editor.reads_until_applied = 1_000;
    let error = type_in_verified_chunks(
        &mut editor,
        &article(50),
        ChunkPolicy {
            land_budget: Duration::from_secs(60),
            ..policy()
        },
        Deadline::after(300).unwrap(),
    )
    .unwrap_err();
    assert_eq!(reason(&error), "timeout");
    assert_eq!(editor.stops, 1);
}
