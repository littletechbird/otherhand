//! Splits typed text into chunks that end on a word boundary.

/// Largest chunk the engine sends before it stops to read the target back.
/// Small enough that a stalled editor is caught within a sentence or two,
/// large enough that an 800-word post needs only a few dozen readbacks.
pub const DEFAULT_MAX_CHUNK_CHARS: usize = 160;

/// Splits `text` into chunks of at most `max_chars` characters.
///
/// A chunk ends after a run of whitespace when it can, so no word is split
/// across two chunks. A word longer than the cap is split at the cap. A CR LF
/// pair is never split, because the keyboard layer types it as one line
/// break. Concatenating the chunks always gives back `text` exactly.
pub fn plan_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let max_chars = max_chars.max(2);
    let chars: Vec<char> = text.chars().collect();
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + max_chars).min(chars.len());
        let mut end = if hard_end == chars.len() {
            hard_end
        } else {
            last_word_boundary(&chars, start, hard_end).unwrap_or(hard_end)
        };
        if end < chars.len() && end > start && chars[end - 1] == '\r' && chars[end] == '\n' {
            end = if end - 1 > start { end - 1 } else { end + 1 };
        }
        chunks.push(chars[start..end].iter().collect());
        start = end;
    }
    chunks
}

/// The last index in `(start, end]` that follows whitespace and precedes a
/// non-whitespace character, or `end` itself when it lands on whitespace.
fn last_word_boundary(chars: &[char], start: usize, end: usize) -> Option<usize> {
    (start + 1..=end)
        .rev()
        .find(|&index| chars[index - 1].is_whitespace() && !chars[index].is_whitespace())
}
