//! Reads long text one page at a time.
//!
//! A single UI Automation call that returns a whole long document is the
//! call most likely to time out or be refused, and a value property is
//! often capped by the provider. Paging keeps every call small, checks the
//! deadline between pages, and lets the caller stop at a sane maximum.

use crate::{AdapterError, Deadline, ErrorCode};

/// Characters requested per page.
pub const DEFAULT_PAGE_CHARS: usize = 4_096;

/// The most text a readback assembles. Typing is capped at 10,000
/// characters, so this leaves room for a field that already held a long
/// document before typing started.
pub const MAX_READBACK_CHARS: usize = 200_000;

/// One page of a paged read: its text, and whether the document ends with
/// it. The provider decides where the document ends, because its units are
/// not always characters: an embedded object is one unit and no text.
pub struct Page {
    pub text: String,
    pub last: bool,
}

/// Assembles a document from `next_page`, which returns the next page of at
/// most `page_chars` units, or `None` once the document has ended.
///
/// Fails when the deadline runs out between pages, or when the document
/// passes `max_chars`, rather than returning a prefix that looks complete.
pub fn read_paged(
    page_chars: usize,
    max_chars: usize,
    deadline: Deadline,
    mut next_page: impl FnMut(usize) -> Result<Option<Page>, AdapterError>,
) -> Result<String, AdapterError> {
    let page_chars = page_chars.max(1);
    let mut text = String::new();
    let mut total = 0;
    loop {
        deadline.ensure_remaining()?;
        let Some(page) = next_page(page_chars)? else {
            return Ok(text);
        };
        total += page.text.chars().count();
        if total > max_chars {
            return Err(AdapterError::new(
                ErrorCode::ActionFailed,
                "The field holds more text than a readback can check",
            )
            .with_details(serde_json::json!({
                "kind": "paged_readback",
                "max_chars": max_chars,
            })));
        }
        text.push_str(&page.text);
        if page.last {
            return Ok(text);
        }
    }
}
