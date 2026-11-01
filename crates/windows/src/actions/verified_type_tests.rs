use super::focus_check_slices;

#[test]
fn slices_rebuild_the_chunk_and_keep_crlf_together() {
    let chunk = "abc\r\ndefgh\u{1F600}ij";
    let slices = focus_check_slices(chunk);
    assert_eq!(slices.concat(), chunk);
    assert!(slices.iter().all(|slice| !slice.starts_with('\n')));
    assert!(slices.iter().all(|slice| slice.chars().count() <= 5));
    assert!(focus_check_slices("").is_empty());
}
