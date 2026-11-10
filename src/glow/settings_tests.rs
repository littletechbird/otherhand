use super::*;
use serde_json::json;

fn dir(label: &str) -> std::path::PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(1);
    let path = std::env::temp_dir().join(format!("glow-set-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn set_saves_color_thickness_and_countdown_and_show_reads_them_back() {
    let folder = dir("save");
    let path = folder.join("config.json");
    let saved = run(
        &path,
        &[
            "set".into(),
            "--thickness".into(),
            "30".into(),
            "--color".into(),
            "#FF8800".into(),
            "--countdown-ms".into(),
            "2000".into(),
        ],
    )
    .unwrap();
    assert_eq!(saved["thickness"], 30);
    assert_eq!(saved["color"], "#FF8800");
    assert_eq!(saved["countdown_ms"], 2000);
    assert_eq!(saved["warning"], "on");
    let shown = run(&path, &["show".into()]).unwrap();
    assert_eq!(shown["color"], "#FF8800");
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn a_zero_countdown_and_a_bad_color_leave_the_file_unchanged() {
    let folder = dir("reject");
    let path = folder.join("config.json");
    run(&path, &["set".into(), "--color".into(), "#00D4FF".into()]).unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let err = run(&path, &["set".into(), "--countdown-ms".into(), "0".into()]).unwrap_err();
    assert!(err.to_string().contains("countdown-ms") || err.to_string().contains("1000"));
    let err = run(&path, &["set".into(), "--color".into(), "orange".into()]).unwrap_err();
    assert!(err.to_string().contains("color"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn reset_restores_the_defaults_and_drops_an_off_switch() {
    let folder = dir("reset");
    let path = folder.join("config.json");
    run(&path, &["set".into(), "--thickness".into(), "40".into()]).unwrap();
    let off = folder.join("glow-person-off");
    crate::glow::person::write_off(&off).unwrap();
    let saved = run(&path, &["reset".into()]).unwrap();
    assert_eq!(saved["thickness"], DEFAULT_THICKNESS);
    assert_eq!(saved["color"], format_color(DEFAULT_COLOR));
    assert_eq!(saved["warning"], "on");
    assert!(!off.exists());
    let _ = std::fs::read_to_string(&path).unwrap();
    let _ = json!({ "kept": true });
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn config_enabled_false_is_not_how_the_warning_turns_off() {
    let folder = dir("ignore");
    let path = folder.join("config.json");
    std::fs::write(&path, br#"{"glow":{"enabled":false,"thickness":12}}"#).unwrap();
    let shown = run(&path, &["show".into()]).unwrap();
    assert_eq!(shown["warning"], "on");
    assert_eq!(shown["person_off"], false);
    let _ = std::fs::remove_dir_all(&folder);
}
