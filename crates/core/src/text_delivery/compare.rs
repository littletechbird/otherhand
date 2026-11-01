//! How typed text is compared with what a field reads back.
//!
//! Editors rewrite what they are given. A rich editor turns a line break
//! into a paragraph and reports it as `\r`, `\n` or `\r\n`; it may show a
//! run of spaces as non-breaking spaces; it may add zero-width marks around
//! a block. None of that means the text failed to land. So both sides are
//! normalized the same way before they are compared: every run of
//! whitespace, including non-breaking spaces and line breaks, becomes one
//! space, and zero-width characters are dropped.

/// Normalizes `text` for comparison. See the module docs.
pub fn normalize_for_compare(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut in_space = false;
    for character in text.chars() {
        if is_zero_width(character) {
            continue;
        }
        if character.is_whitespace() || character == '\u{a0}' {
            if !in_space {
                normalized.push(' ');
            }
            in_space = true;
            continue;
        }
        in_space = false;
        normalized.push(character);
    }
    normalized
}

fn is_zero_width(character: char) -> bool {
    matches!(
        character,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
    )
}

/// Whether `typed` has landed in a field that read `baseline` before typing
/// started and reads `observed` now.
///
/// The typed text must appear in the field as one unbroken run, and it must
/// appear more times than it did before typing started, so text that was
/// already there cannot pass for new text. Trailing whitespace is left out
/// of the comparison, because an editor may not show a trailing space until
/// the next character arrives. Text that is only whitespace, such as a lone
/// line break, cannot be found this way, so it counts as landed when the
/// field changed at all.
pub fn landed(baseline: &str, observed: &str, typed: &str) -> bool {
    let needle = normalize_for_compare(typed);
    let needle = needle.trim();
    if needle.is_empty() {
        return observed != baseline;
    }
    let observed = normalize_for_compare(observed);
    let baseline = normalize_for_compare(baseline);
    occurrences(&observed, needle) > occurrences(&baseline, needle)
}

fn occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut from = 0;
    while let Some(found) = haystack[from..].find(needle) {
        count += 1;
        from += found + needle.chars().next().map_or(1, char::len_utf8);
    }
    count
}
