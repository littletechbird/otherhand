use super::plan_chunks;

#[test]
fn chunks_join_back_to_the_original_text() {
    let text = "One two three, four five six.\r\nSeven eight\nnine ten eleven twelve.";
    for cap in [2, 3, 5, 8, 13, 40, 400] {
        let chunks = plan_chunks(text, cap);
        assert_eq!(chunks.concat(), text, "cap {cap}");
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= cap + 1));
    }
}

#[test]
fn chunks_end_on_word_boundaries_when_they_can() {
    let chunks = plan_chunks("alpha beta gamma delta epsilon", 12);
    assert_eq!(chunks, ["alpha beta ", "gamma delta ", "epsilon"]);
}

#[test]
fn a_word_longer_than_the_cap_is_split_at_the_cap() {
    let chunks = plan_chunks("abcdefghij", 4);
    assert_eq!(chunks, ["abcd", "efgh", "ij"]);
}

#[test]
fn a_crlf_pair_is_never_split() {
    let text = "abc\r\ndef";
    for cap in 2..8 {
        for chunk in plan_chunks(text, cap) {
            assert!(!chunk.ends_with('\r'), "cap {cap}: {chunk:?}");
        }
    }
}

#[test]
fn empty_text_has_no_chunks() {
    assert!(plan_chunks("", 10).is_empty());
}

#[test]
fn multibyte_text_is_split_on_characters_not_bytes() {
    let text = "héllo wörld ünïcode ✓✓✓";
    let chunks = plan_chunks(text, 6);
    assert_eq!(chunks.concat(), text);
}
