//! Reads a text target's whole current text for verified typing and for
//! live value reads on long editors.
//!
//! `TextPattern` is read a page at a time through fresh document ranges, so
//! no single cross-process call has to return a long document, which is the
//! call a rich web editor was seen to refuse. A field without `TextPattern`
//! falls back to its `ValuePattern` value. A password field is never read.
//!
//! Chromium answers an empty field's document range with the field's
//! accessible name, so a document text equal to the name, on a field whose
//! `ValuePattern` value is empty or missing, reads as empty.

#[cfg(target_os = "windows")]
mod imp {
    use agent_desktop_core::text_delivery::{
        DEFAULT_PAGE_CHARS, MAX_READBACK_CHARS, Page, read_paged,
    };
    use agent_desktop_core::{AdapterError, Deadline};
    use uiautomation::patterns::{UITextPattern, UIValuePattern};
    use uiautomation::types::{TextPatternRangeEndpoint, TextUnit};

    use crate::tree::automation::uia_error;
    use crate::tree::element::UIAElement;

    pub(crate) fn read_target_text(
        element: &UIAElement,
        deadline: Deadline,
    ) -> Result<Option<String>, AdapterError> {
        if element.0.is_password().unwrap_or(true) {
            return Ok(None);
        }
        let value = element
            .0
            .get_pattern::<UIValuePattern>()
            .and_then(|pattern| pattern.get_value())
            .ok();
        let Ok(pattern) = element.0.get_pattern::<UITextPattern>() else {
            return Ok(value);
        };
        let mut offset = 0_usize;
        let text = read_paged(DEFAULT_PAGE_CHARS, MAX_READBACK_CHARS, deadline, |page| {
            next_page(&pattern, &mut offset, page)
        })?;
        let name = element.0.get_name().unwrap_or_default();
        if !text.is_empty() && text == name && value.as_deref().is_none_or(str::is_empty) {
            return Ok(Some(String::new()));
        }
        Ok(Some(text))
    }

    fn next_page(
        pattern: &UITextPattern,
        offset: &mut usize,
        page: usize,
    ) -> Result<Option<Page>, AdapterError> {
        let failed = |error: &uiautomation::Error| uia_error(error, "read the field's text");
        let range = pattern.get_document_range().map_err(|e| failed(&e))?;
        let skip = i32::try_from(*offset).unwrap_or(i32::MAX);
        if skip > 0 {
            let moved = range
                .move_endpoint_by_unit(TextPatternRangeEndpoint::Start, TextUnit::Character, skip)
                .map_err(|e| failed(&e))?;
            if moved < skip {
                return Ok(None);
            }
        }
        range
            .move_endpoint_by_range(
                TextPatternRangeEndpoint::End,
                &range,
                TextPatternRangeEndpoint::Start,
            )
            .map_err(|e| failed(&e))?;
        let want = i32::try_from(page).unwrap_or(i32::MAX);
        let moved = range
            .move_endpoint_by_unit(TextPatternRangeEndpoint::End, TextUnit::Character, want)
            .map_err(|e| failed(&e))?;
        if moved <= 0 {
            return Ok(None);
        }
        let text = range.get_text(-1).map_err(|e| failed(&e))?;
        *offset += usize::try_from(moved).unwrap_or(0);
        Ok(Some(Page {
            text,
            last: moved < want,
        }))
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use agent_desktop_core::{AdapterError, Deadline};

    use crate::tree::element::UIAElement;

    pub(crate) fn read_target_text(
        _element: &UIAElement,
        _deadline: Deadline,
    ) -> Result<Option<String>, AdapterError> {
        Ok(None)
    }
}

pub(crate) use imp::read_target_text;
