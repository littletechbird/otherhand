use super::*;

#[test]
fn only_the_exact_phrase_on_a_console_authorizes_off() {
    assert!(phrase_authorizes_off(true, "GLOW OFF\n"));
    assert!(phrase_authorizes_off(true, "  GLOW OFF  "));
    assert!(!phrase_authorizes_off(false, "GLOW OFF"));
    assert!(!phrase_authorizes_off(true, "glow off"));
    assert!(!phrase_authorizes_off(true, "GLOW OFF please"));
}

#[test]
fn the_off_file_is_the_only_switch_and_clearing_it_turns_the_warning_back_on() {
    let dir = std::env::temp_dir().join(format!("glow-person-{}-{}", std::process::id(), line()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("glow-person-off");
    assert!(!file_is_off(&path));
    write_off(&path).unwrap();
    assert!(file_is_off(&path));
    std::fs::write(&path, "enabled=false\n").unwrap();
    assert!(
        !file_is_off(&path),
        "a config-shaped file is not the person switch"
    );
    write_off(&path).unwrap();
    clear_off(&path).unwrap();
    assert!(!file_is_off(&path));
    clear_off(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn line() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(1)
}
