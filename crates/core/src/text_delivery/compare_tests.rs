use super::{landed, normalize_for_compare};

#[test]
fn line_breaks_and_space_runs_compare_as_one_space() {
    assert_eq!(
        normalize_for_compare("a\r\nb\n\nc  d\u{a0}e\tf"),
        "a b c d e f"
    );
}

#[test]
fn zero_width_marks_are_ignored() {
    assert_eq!(normalize_for_compare("a\u{200b}b\u{feff}"), "ab");
}

#[test]
fn text_that_arrived_whole_has_landed() {
    assert!(landed(
        "Title",
        "Title First paragraph.",
        "First paragraph."
    ));
}

#[test]
fn a_paragraph_break_rewritten_by_the_editor_still_lands() {
    assert!(landed("", "One.\nTwo.", "One.\r\nTwo.\r"));
}

#[test]
fn text_that_only_partly_arrived_has_not_landed() {
    assert!(!landed("", "First para", "First paragraph."));
}

#[test]
fn text_split_by_something_else_has_not_landed() {
    assert!(!landed(
        "",
        "the mou@someone tag lineuse moved",
        "the mouse moved"
    ));
}

#[test]
fn text_that_was_already_there_does_not_count_as_new() {
    assert!(!landed("hello world", "hello world", "hello"));
    assert!(landed("hello world", "hello world hello", "hello"));
}

#[test]
fn a_missing_trailing_space_is_not_a_failure() {
    assert!(landed("", "alpha beta", "alpha beta "));
}

#[test]
fn whitespace_only_text_lands_when_the_field_changed() {
    assert!(landed("abc", "abc\r", "\r"));
    assert!(!landed("abc", "abc", "\r"));
}
