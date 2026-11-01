//! Shared append-only audit log in Dark Room's format (`~/.darkroom/audit.log`,
//! one JSON object per line with `ts` and `event`). Fence refusals and secret
//! fills land in the same file. Never pass titles, values or secrets here.

use std::io::Write;
use std::path::PathBuf;

/// The vault folder `fill-secret` reads: `DARKROOM_HOME` when set (tests
/// and portable vaults), else the user's `~/.darkroom`.
pub(crate) fn darkroom_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DARKROOM_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|h| h.join(".darkroom"))
}

/// Every Dark Room folder to protect and log into: always the user's own
/// `~/.darkroom`, plus `DARKROOM_HOME` when it points somewhere else. An
/// agent that sets `DARKROOM_HOME` can't unprotect the real vault or move
/// the audit trail away from the user.
pub(crate) fn darkroom_dirs_from(env: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = home.map(|h| vec![h.join(".darkroom")]).unwrap_or_default();
    if let Some(d) = env.filter(|v| !v.is_empty()).map(PathBuf::from) {
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

pub(crate) fn darkroom_dirs() -> Vec<PathBuf> {
    darkroom_dirs_from(std::env::var_os("DARKROOM_HOME"), home_dir())
}

/// The user's profile folder. On Windows it comes from the shell's known
/// folder, not from `USERPROFILE`, which the caller's environment controls.
pub(crate) fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        if let Some(p) = known_profile_dir() {
            return Some(p);
        }
    }
    home_from_env(std::env::var_os("USERPROFILE"), std::env::var_os("HOME"))
}

/// An empty `USERPROFILE` must not hide a real `HOME`.
pub(crate) fn home_from_env(profile: Option<std::ffi::OsString>, home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    profile.filter(|v| !v.is_empty()).or_else(|| home.filter(|v| !v.is_empty())).map(PathBuf::from)
}

#[cfg(target_os = "windows")]
fn known_profile_dir() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Profile, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Profile, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const core::ffi::c_void));
        s.filter(|s| !s.is_empty()).map(PathBuf::from)
    }
}

const FORBIDDEN_KEYS: &[&str] = &["secret", "plaintext", "password", "token_value", "value", "title", "text"];

/// Builds the line; refuses forbidden keys the same way Dark Room's vault does.
pub(crate) fn record(event: &str, fields: &[(&str, &str)]) -> Option<String> {
    let mut map = serde_json::Map::new();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    map.insert("ts".into(), serde_json::json!(ts));
    map.insert("event".into(), serde_json::json!(event));
    for (k, v) in fields {
        if FORBIDDEN_KEYS.contains(k) {
            return None;
        }
        let v = if v.len() > 200 && !v.starts_with("dr_sec_") {
            format!("{}…", &v[..v.char_indices().nth(32).map_or(v.len(), |(i, _)| i)])
        } else {
            (*v).to_string()
        };
        map.insert((*k).into(), serde_json::json!(v));
    }
    serde_json::to_string(&serde_json::Value::Object(map)).ok()
}

pub(crate) fn append(event: &str, fields: &[(&str, &str)]) {
    if cfg!(test) {
        return;
    }
    let Some(line) = record(event, fields) else {
        return;
    };
    for dir in darkroom_dirs() {
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("audit.log"))
        {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_refuses_content_fields() {
        assert!(record("fence_refused", &[("value", "x")]).is_none());
        assert!(record("fence_refused", &[("title", "x")]).is_none());
        let line = record("fence_refused", &[("action", "get_tree"), ("target", "keepass.exe")]).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["event"], "fence_refused");
        assert_eq!(v["target"], "keepass.exe");
        assert!(v["ts"].as_f64().unwrap() > 0.0);
    }

    #[test]
    fn long_fields_are_truncated() {
        let long = "a".repeat(300);
        let line = record("e", &[("origin", &long)]).unwrap();
        assert!(line.len() < 120);
    }
}
