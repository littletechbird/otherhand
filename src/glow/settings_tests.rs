use super::{from_args, run};
use crate::glow::{config_from, format_color};
use agent_desktop_core::AppError;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

fn scratch(label: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("glow-settings-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("config.json")
}

fn glow(path: &Path, args: &[&str]) -> Result<Value, AppError> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run(path, &args)
}

fn message(error: &AppError) -> String {
    error.to_string()
}

#[test]
fn show_reports_defaults_without_creating_a_file() {
    let path = scratch("show");
    let shown = glow(&path, &["show"]).unwrap();
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["color"], "#00D4FF");
    assert_eq!(shown["colors"], json!(["#00D4FF"]));
    assert_eq!(shown["thickness"], 12);
    assert_eq!(shown["opacity"], 100);
    assert!(shown["countdown_ms"].is_null());
    assert_eq!(shown["applies"], "next_glowing_command");
    assert!(!path.exists());
}

#[test]
fn set_round_trips_and_reset_restores_defaults() {
    let path = scratch("round");
    std::fs::write(
        &path,
        r#"{"kept":true,"glow":{"linger_ms":2222,"require_capture_exclusion":true}}"#,
    )
    .unwrap();
    let saved = glow(
        &path,
        &[
            "set",
            "--off",
            "--thickness",
            "8",
            "--colors",
            "#00D4FF,#7C5CFF",
            "--brightness",
            "40",
            "--countdown-ms",
            "5000",
        ],
    )
    .unwrap();
    assert_eq!(saved["enabled"], false);
    assert_eq!(saved["thickness"], 8);
    assert_eq!(saved["color"], "#00D4FF");
    assert_eq!(saved["colors"], json!(["#00D4FF", "#7C5CFF"]));
    assert_eq!(saved["opacity"], 40);
    assert_eq!(saved["countdown_ms"], 5000);
    assert_eq!(saved["linger_ms"], 2222);
    assert_eq!(saved["require_capture_exclusion"], true);

    let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(doc["kept"], true);
    let cfg = config_from(Some(&doc));
    assert!(!cfg.enabled);
    assert_eq!(cfg.thickness, 8);
    assert_eq!(cfg.opacity, 40);
    assert_eq!(format_color(cfg.color), "#00D4FF");
    let (colors, n) = cfg.palette();
    assert_eq!(
        colors[..n]
            .iter()
            .copied()
            .map(format_color)
            .collect::<Vec<_>>(),
        vec!["#00D4FF", "#7C5CFF"]
    );
    assert_eq!(cfg.linger_ms, 2222);
    assert!(cfg.strict);

    let again = glow(&path, &["show"]).unwrap();
    assert_eq!(again["countdown_ms"], 5000);
    assert_eq!(again["opacity"], 40);

    let reset = glow(&path, &["reset"]).unwrap();
    assert_eq!(reset["enabled"], true);
    assert_eq!(reset["thickness"], 12);
    assert_eq!(reset["opacity"], 100);
    assert_eq!(reset["color"], "#00D4FF");
    assert_eq!(reset["colors"], json!(["#00D4FF"]));
    assert!(reset["countdown_ms"].is_null());
    assert_eq!(reset["linger_ms"], 1500);
    assert_eq!(reset["require_capture_exclusion"], false);
    let doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(doc["kept"], true);
    assert!(doc["glow"].get("countdown_ms").is_none());
    assert_eq!(config_from(Some(&doc)), crate::glow::GlowConfig::default());
}

#[test]
fn a_boolean_glow_expands_when_one_field_is_set() {
    let path = scratch("bool");
    std::fs::write(&path, r#"{"glow":false}"#).unwrap();
    let shown = glow(&path, &["show"]).unwrap();
    assert_eq!(shown["enabled"], false);
    assert_eq!(shown["thickness"], 12);
    let saved = glow(
        &path,
        &["set", "--on", "--thickness", "16", "--color", "#abc"],
    )
    .unwrap();
    assert_eq!(saved["enabled"], true);
    assert_eq!(saved["thickness"], 16);
    assert_eq!(saved["color"], "#AABBCC");
    assert_eq!(saved["colors"], json!(["#AABBCC"]));
}

#[test]
fn invalid_values_are_errors_and_leave_the_file_alone() {
    let path = scratch("bad");
    std::fs::write(&path, r#"{"glow":{"thickness":9}}"#).unwrap();
    let before = std::fs::read(&path).unwrap();
    for args in [
        &["set", "--thickness", "1"][..],
        &["set", "--thickness", "25"],
        &["set", "--thickness", "99"],
        &["set", "--thickness", "nope"],
        &["set", "--color", "blue"],
        &["set", "--color", "#GG0000"],
        &["set", "--colors", "#00D4FF,blue"],
        &["set", "--colors", "#111111,#222222,#333333,#444444,#555555"],
        &["set", "--opacity", "101"],
        &["set", "--brightness", "-1"],
        &["set", "--countdown-ms", "3600001"],
        &["set", "--on", "--off"],
        &["set", "--color", "#00D4FF", "--colors", "#00D4FF,#FFFFFF"],
        &["set"],
    ] {
        let error = glow(&path, args).unwrap_err();
        assert_eq!(error.code(), "INVALID_ARGS", "{args:?} {}", message(&error));
        assert!(!message(&error).is_empty());
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(glow(&path, &["set", "--thickness", "2"]).is_ok());
    assert!(
        glow(
            &path,
            &[
                "set",
                "--thickness",
                "24",
                "--opacity",
                "0",
                "--countdown-ms",
                "0"
            ]
        )
        .is_ok()
    );
    let shown = glow(&path, &["show"]).unwrap();
    assert_eq!(shown["thickness"], 24);
    assert_eq!(shown["opacity"], 0);
    assert_eq!(shown["countdown_ms"], 0);
}

#[test]
fn other_commands_are_not_glow_commands() {
    let args: Vec<OsString> = ["otherhand", "snapshot"]
        .into_iter()
        .map(OsString::from)
        .collect();
    assert!(from_args(&args).is_none());
}
