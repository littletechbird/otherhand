use super::{Page, read_paged};
use crate::{Deadline, ErrorCode};

fn pages_of(
    text: &str,
    page: usize,
) -> impl FnMut(usize) -> Result<Option<Page>, crate::AdapterError> {
    let chars: Vec<char> = text.chars().collect();
    let mut at = 0;
    let mut done = false;
    move |requested| {
        assert_eq!(requested, page);
        if done {
            return Ok(None);
        }
        let end = (at + requested).min(chars.len());
        let out: String = chars[at..end].iter().collect();
        at = end;
        let last = end == chars.len();
        done = last;
        Ok(Some(Page { text: out, last }))
    }
}

#[test]
fn a_long_document_is_read_back_whole() {
    let text = "word ".repeat(2_000);
    let read = read_paged(
        64,
        100_000,
        Deadline::after(1_000).unwrap(),
        pages_of(&text, 64),
    )
    .unwrap();
    assert_eq!(read, text);
}

#[test]
fn a_document_that_ends_on_a_page_boundary_is_read_whole() {
    let text = "x".repeat(128);
    let read = read_paged(
        64,
        1_000,
        Deadline::after(1_000).unwrap(),
        pages_of(&text, 64),
    )
    .unwrap();
    assert_eq!(read, text);
}

#[test]
fn a_document_past_the_cap_fails_instead_of_returning_a_prefix() {
    let text = "y".repeat(500);
    let error = read_paged(
        64,
        200,
        Deadline::after(1_000).unwrap(),
        pages_of(&text, 64),
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ActionFailed);
}

#[test]
fn an_empty_page_that_is_not_last_does_not_end_the_read() {
    let mut pages = vec![
        Page {
            text: "b".into(),
            last: true,
        },
        Page {
            text: String::new(),
            last: false,
        },
        Page {
            text: "a".into(),
            last: false,
        },
    ];
    let read = read_paged(1, 100, Deadline::after(1_000).unwrap(), |_| Ok(pages.pop())).unwrap();
    assert_eq!(read, "ab");
}

#[test]
fn a_page_read_failure_is_returned() {
    let error = read_paged(10, 100, Deadline::after(1_000).unwrap(), |_| {
        Err(crate::AdapterError::new(ErrorCode::AppUnresponsive, "gone"))
    })
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::AppUnresponsive);
}
