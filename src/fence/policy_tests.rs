use super::*;

fn fence() -> Fence {
    let file: FenceFile = serde_json::from_str(
        r#"{
            "version": 1,
            "programs": {
                "process_names": ["MyBank.exe"],
                "exe_paths": ["C:\\Program Files\\Quicken\\"],
                "window_classes": ["BankFrame"],
                "title_patterns": ["*Chase*"]
            },
            "paths": ["C:\\Users\\alex\\Private", "\\\\nas\\finance"]
        }"#,
    )
    .unwrap();
    Fence::new(&file, &["C:\\Users\\alex\\.agent-desktop\\fence.json".into()])
}

fn proc(path: &str) -> ProcessFacts {
    ProcessFacts { image_path: Some(path.into()), image_name: None }
}

#[test]
fn floor_cannot_be_removed_by_an_empty_file() {
    let f = Fence::new(&FenceFile::default(), &[]);
    assert_eq!(f.match_process(&proc("C:\\Program Files\\KeePass\\KeePass.exe")), Some(Rule::ProcessName));
    assert_eq!(f.match_process(&proc("C:\\Windows\\System32\\consent.exe")), Some(Rule::ProcessName));
    assert_eq!(
        f.match_process(&proc("C:\\Users\\alex\\AppData\\Local\\Programs\\Grok Bot\\Grok Bot.exe")),
        Some(Rule::ProcessName)
    );
    let w = WindowFacts { class: "#32770".into(), title: "Windows Security".into(), process: proc("C:\\x\\notepad.exe"), folder: None };
    assert_eq!(f.match_window(&w), Some(Rule::TitlePattern));
    let w = WindowFacts { class: "Credential Dialog Xaml Host".into(), title: String::new(), process: proc("C:\\x\\app.exe"), folder: None };
    assert_eq!(f.match_window(&w), Some(Rule::WindowClass));
}

#[test]
fn unknown_process_identity_is_fenced() {
    let f = fence();
    assert_eq!(f.match_process(&ProcessFacts::default()), Some(Rule::UnknownIdentity));
}

#[test]
fn user_rules_match_case_insensitively() {
    let f = fence();
    assert_eq!(f.match_process(&proc("D:\\apps\\MYBANK.EXE")), Some(Rule::ProcessName));
    assert_eq!(f.match_process(&proc("c:/program files/quicken/qw.exe")), Some(Rule::ExePath));
    assert_eq!(f.match_process(&proc("C:\\Windows\\notepad.exe")), None);
    let w = WindowFacts { class: "Chrome_WidgetWin_1".into(), title: "Chase Online - Google Chrome".into(), process: proc("C:\\c\\chrome.exe"), folder: None };
    assert_eq!(f.match_window(&w), Some(Rule::TitlePattern));
    let w = WindowFacts { class: "bankframe".into(), title: "x".into(), process: proc("C:\\c\\x.exe"), folder: None };
    assert_eq!(f.match_window(&w), Some(Rule::WindowClass));
}

#[test]
fn explorer_window_showing_a_fenced_folder_is_fenced() {
    let f = fence();
    let w = WindowFacts {
        class: "CabinetWClass".into(),
        title: "Taxes".into(),
        process: proc("C:\\Windows\\explorer.exe"),
        folder: Some("C:\\Users\\alex\\Private\\Taxes".into()),
    };
    assert_eq!(f.match_window(&w), Some(Rule::Path));
    let w = WindowFacts { folder: Some("C:\\Users\\alex\\Privateer".into()), ..w };
    assert_eq!(f.match_window(&w), None);
}

#[test]
fn path_normalization_folds_prefixes_dots_and_case() {
    assert_eq!(normalize_path("\\\\?\\C:\\Users\\alex\\Private\\"), "c:\\users\\alex\\private");
    assert_eq!(normalize_path("\\\\?\\UNC\\nas\\Finance\\a"), "\\\\nas\\finance\\a");
    assert_eq!(normalize_path("C:/Users/alex/Docs/../Private/./x.pdf"), "c:\\users\\alex\\private\\x.pdf");
    let f = fence();
    assert!(f.path_is_fenced("\\\\?\\c:\\USERS\\alex\\private\\bank.pdf"));
    assert!(f.path_is_fenced("C:\\Users\\alex\\Docs\\..\\Private"));
    assert!(f.path_is_fenced("\\\\NAS\\finance\\2026\\q3.xlsx"));
    assert!(!f.path_is_fenced("C:\\Users\\alex\\PrivateNot\\x"));
    assert!(f.path_is_fenced("C:\\Users\\alex\\.agent-desktop\\fence.json"));
}

#[test]
fn free_text_naming_a_fenced_root_is_caught() {
    let f = fence();
    assert!(f.text_mentions_fenced_root("C:\\Users\\alex\\Private\\taxes.pdf"));
    assert!(f.text_mentions_fenced_root("open \"c:/users/alex/private\" now"));
    assert!(!f.text_mentions_fenced_root("C:\\Users\\alex\\Privateer\\x"));
    assert!(!f.text_mentions_fenced_root("hello world"));
}

#[test]
fn path_tokens_find_drive_and_unc_paths() {
    let t = path_tokens("see C:\\Users\\alex\\My Docs\\a.txt\nand \\\\nas\\share\\b \"D:/x/y\"");
    assert_eq!(t, vec!["C:\\Users\\alex\\My Docs\\a.txt", "\\\\nas\\share\\b ", "D:/x/y"].into_iter().map(|s| s.trim_end().to_string()).collect::<Vec<_>>());
    assert!(path_tokens("ratio 3:2 and http://x").is_empty());
}

#[test]
fn glob_handles_stars_and_question_marks() {
    assert!(glob_match("*chase*", "my chase account"));
    assert!(glob_match("dark room*", "dark room"));
    assert!(glob_match("a?c", "abc"));
    assert!(!glob_match("a?c", "ac"));
    assert!(!glob_match("windows security", "windows security center"));
}

#[test]
fn unknown_fence_file_fields_are_rejected() {
    assert!(serde_json::from_str::<FenceFile>(r#"{"programz": {}}"#).is_err());
}
