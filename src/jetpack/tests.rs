use super::guard::{Guard, Refusal, looks_secret};
use super::store::{Store, civil, slug};
use super::run;
use crate::fence::policy::{Fence, FenceFile};
use agent_desktop_core::AppError;
use serde_json::Value;
use std::path::PathBuf;

fn fence() -> Fence {
    let file: FenceFile = serde_json::from_str(
        r#"{ "version": 1, "programs": { "process_names": ["MyBank.exe"] }, "paths": ["C:\\Users\\alex\\Private"] }"#,
    )
    .unwrap();
    Fence::new(&file, &["C:\\Users\\alex\\.darkroom".to_string()])
}

fn expand(t: &str) -> String {
    t.replace("%USERPROFILE%", "C:\\Users\\alex")
}

fn same(p: &str) -> String {
    p.to_string()
}

fn scratch(label: &str) -> PathBuf {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("jetpack-test-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Runs a jetpack command against a store in a scratch folder, with the test
/// fence and one fenced window on screen titled "Statement - MyBank Online".
fn jet(store: &Store, args: &[&str]) -> Result<Value, AppError> {
    let f = fence();
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run(store, &args, &|body| {
        body(&Guard { fence: &f, fenced_titles: vec!["Statement - MyBank Online".into()], resolve: &same, expand: &expand })
    })
}

fn code(r: &Result<Value, AppError>) -> String {
    match r {
        Err(AppError::Adapter(e)) => format!("{:?}", e.code),
        Err(other) => format!("{other:?}"),
        Ok(_) => "ok".into(),
    }
}

fn all_text(dir: &std::path::Path) -> String {
    let mut out = String::new();
    for e in walk(dir) {
        out.push_str(&std::fs::read_to_string(e).unwrap_or_default());
    }
    out
}

fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() { v.extend(walk(&p)) } else { v.push(p) }
        }
    }
    v
}

#[test]
fn a_note_lands_in_the_session_log_and_reads_back() {
    let dir = scratch("note");
    let store = Store::new(dir.clone());
    jet(&store, &["note", "Notepad", "opens", "untitled", "on", "launch"]).unwrap();
    let r = jet(&store, &["read", "log"]).unwrap();
    assert!(r["text"].as_str().unwrap().contains("Notepad opens untitled on launch"));
    assert!(dir.join("INDEX.md").exists());
}

#[test]
fn a_preference_goes_to_preferences() {
    let store = Store::new(scratch("pref"));
    jet(&store, &["note", "--preference", "Save files to C:\\Users\\alex\\Documents"]).unwrap();
    let r = jet(&store, &["read", "preferences"]).unwrap();
    assert!(r["text"].as_str().unwrap().contains("- Save files to C:\\Users\\alex\\Documents"));
}

#[test]
fn recipes_add_find_and_index() {
    let store = Store::new(scratch("recipe"));
    jet(&store, &["recipe", "add", "--app", "notepad.exe", "--ref", "Edit", "--role", "textfield", "set-value works headless"]).unwrap();
    jet(&store, &["recipe", "add", "--app", "charmap.exe", "--role", "checkbox", "Advanced view toggles quietly"]).unwrap();
    let found = jet(&store, &["recipe", "find", "headless"]).unwrap();
    let m = found["matches"].as_array().unwrap();
    assert_eq!(m.len(), 1);
    assert_eq!(m[0]["app"], "notepad.exe");
    assert!(m[0]["entry"].as_str().unwrap().contains("role: textfield"));
    assert_eq!(jet(&store, &["recipe", "find", "charmap"]).unwrap()["matches"].as_array().unwrap().len(), 1);
    let index = jet(&store, &["read"]).unwrap();
    let t = index["text"].as_str().unwrap();
    assert!(t.contains("- charmap.exe: 1 entry") && t.contains("- notepad.exe: 1 entry"), "{t}");
    let apps = jet(&store, &["read", "recipes"]).unwrap();
    assert_eq!(apps["apps"].as_array().unwrap().len(), 2);
}

#[test]
fn an_app_name_can_not_escape_the_recipes_folder() {
    let dir = scratch("slug");
    let store = Store::new(dir.clone());
    jet(&store, &["recipe", "add", "--app", "..\\..\\Windows\\evil", "text"]).unwrap();
    assert_eq!(slug("..\\..\\Windows\\evil"), "windows-evil");
    for p in walk(&dir) {
        assert!(p.starts_with(&dir), "{p:?}");
    }
    assert!(jet(&store, &["recipe", "add", "--app", "..", "text"]).is_err());
}

#[test]
fn refusals_write_nothing_and_never_echo_the_text() {
    let dir = scratch("refuse");
    let store = Store::new(dir.clone());
    jet(&store, &["note", "baseline"]).unwrap();
    let before = all_text(&dir);
    let cases: &[(&[&str], &str)] = &[
        (&["note", "fill with dr_sec_a1b2c3"], "dark_room_handle"),
        (&["note", "DR_SEC_upper too"], "dark_room_handle"),
        (&["note", "login password=hunter2"], "secret_shaped"),
        (&["note", "key sk-live-0123456789abcdefghij"], "secret_shaped"),
        (&["note", "token Xk9pQ2mZ7rT4vB8nL1sD5fG3hJ6"], "secret_shaped"),
        (&["note", "-----BEGIN OPENSSH PRIVATE KEY-----"], "secret_shaped"),
        (&["note", "statement is in C:\\Users\\alex\\Private\\june.pdf"], "fenced_path"),
        (&["note", "look in %USERPROFILE%\\Private"], "fenced_path"),
        (&["note", "the vault lives at C:\\Users\\alex\\.darkroom"], "fenced_path"),
        (&["note", "MyBank.exe has a login button"], "fenced_program"),
        (&["note", "KeePass keeps the list"], "fenced_program"),
        (&["note", "the window said statement - mybank online"], "fenced_window_text"),
        (&["recipe", "add", "--app", "MyBank.exe", "the login is at the top"], "fenced_program"),
        (&["recipe", "add", "--app", "notepad.exe", "--ref", "dr_sec_x", "text"], "dark_room_handle"),
    ];
    for (args, reason) in cases {
        let r = jet(&store, args);
        assert_eq!(code(&r), "Fenced", "{args:?}");
        let Err(AppError::Adapter(e)) = &r else { unreachable!() };
        assert_eq!(e.details.as_ref().unwrap()["reason"], *reason, "{args:?}");
        let shown = format!("{} {:?}", e.message, e.suggestion);
        assert!(!shown.contains("hunter2") && !shown.contains("june.pdf") && !shown.contains("dr_sec_a1"), "{shown}");
    }
    assert_eq!(all_text(&dir), before, "a refused write must not change any file");
}

#[test]
fn ordinary_recipe_text_is_not_mistaken_for_a_secret() {
    for ok in [
        "ref e14 role textfield name 'File name:' then set-value",
        "C:\\Users\\alex\\Documents\\notes.txt opens in notepad",
        "press ctrl+s, wait 300 ms, then read the title",
        "window class Notepad, child Edit, EM_REPLACESEL is quiet",
        "the password field is the second textfield",
        "a1b2c3 short id",
    ] {
        assert!(!looks_secret(ok), "{ok}");
        let f = fence();
        let g = Guard { fence: &f, fenced_titles: vec![], resolve: &same, expand: &expand };
        assert_eq!(g.screen(ok), Ok(()), "{ok}");
    }
}

#[test]
fn guard_limits_size_and_control_characters() {
    let f = fence();
    let g = Guard { fence: &f, fenced_titles: vec![], resolve: &same, expand: &expand };
    assert_eq!(g.screen(&"a ".repeat(2500)), Err(Refusal::TooLong));
    assert_eq!(g.screen("bell\u{7}"), Err(Refusal::ControlCharacters));
    assert_eq!(g.screen("two\nlines\tok"), Ok(()));
}

#[test]
fn short_fenced_titles_do_not_block_everything() {
    let f = fence();
    let g = Guard { fence: &f, fenced_titles: vec!["Bank".into(), "".into()], resolve: &same, expand: &expand };
    assert_eq!(g.screen("bank holidays are in the calendar"), Err(Refusal::FencedWindowText));
    let g = Guard { fence: &f, fenced_titles: vec!["abc".into()], resolve: &same, expand: &expand };
    assert_eq!(g.screen("abc"), Ok(()), "titles under four characters are ignored");
}

#[test]
fn bad_usage_is_invalid_args_not_a_write() {
    let store = Store::new(scratch("usage"));
    assert_eq!(code(&jet(&store, &["note"])), "InvalidArgs");
    assert_eq!(code(&jet(&store, &["recipe", "add", "no app"])), "InvalidArgs");
    assert_eq!(code(&jet(&store, &["fly"])), "InvalidArgs");
    assert_eq!(code(&jet(&store, &["read", "secrets"])), "InvalidArgs");
}

#[test]
fn only_jetpack_argv_is_taken() {
    let args: Vec<std::ffi::OsString> = ["otherhand", "list-windows"].iter().map(Into::into).collect();
    assert!(super::from_args(&args).is_none());
}

#[test]
fn utc_dates() {
    assert_eq!(civil(0), ("1970-01-01".into(), "00:00".into()));
    assert_eq!(civil(1_793_547_720), ("2026-11-01".into(), "15:42".into()));
    assert_eq!(civil(951_782_400), ("2000-02-29".into(), "00:00".into()));
}
