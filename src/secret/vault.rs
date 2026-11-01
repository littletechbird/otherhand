//! Read-only access to a Dark Room vault (`machine.key` + `vault.enc.json`).
//! Plaintext only ever lives in a `Plain` that zeroes itself on drop and is
//! never formatted, logged or serialized.

use std::path::Path;

use super::fernet::FernetKey;
use crate::fence::audit;

pub(crate) const HANDLE_PREFIX: &str = "dr_sec_";

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VaultError {
    NoVault,
    BadFormat,
    Miss,
    Revoked,
    Expired,
    NoOrigins,
    OriginDenied,
}

impl VaultError {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::NoVault => "no_vault",
            Self::BadFormat => "bad_format",
            Self::Miss => "resolve_miss",
            Self::Revoked => "resolve_revoked",
            Self::Expired => "resolve_expired",
            Self::NoOrigins => "resolve_no_origins",
            Self::OriginDenied => "resolve_origin_denied",
        }
    }
}

pub(crate) struct Plain(Vec<u8>);

impl Plain {
    pub(crate) fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).unwrap_or("")
    }
    #[cfg(test)]
    pub(crate) fn char_count(&self) -> usize {
        self.as_str().chars().count()
    }
}

impl Drop for Plain {
    fn drop(&mut self) {
        self.0.iter_mut().for_each(|b| *b = 0);
    }
}

/// Origins on a handle may name a full image path (`app://c:/windows/notepad.exe`)
/// or just the image (`app://notepad.exe`); web origins compare exactly.
pub(crate) fn origin_allowed(allowed: &[String], candidates: &[String]) -> bool {
    allowed.iter().any(|a| {
        let a = a.trim().trim_end_matches('/').to_lowercase().replace('\\', "/");
        candidates.iter().any(|c| c.trim_end_matches('/').to_lowercase().replace('\\', "/") == a)
    })
}

/// Resolves `handle` for `origins` (every accepted spelling of the target's
/// origin). Production rules from the Dark Room spec: an empty allowlist
/// is refused, and an `expires_at` in the past is refused.
/// Whether a handle may go into a field that shows its text. Only the
/// person can set `fill_visible` in Dark Room; by default a secret goes
/// into password fields only, where nothing can read it back.
pub(crate) struct Resolved {
    pub plain: Plain,
    pub fill_visible: bool,
}

#[cfg(test)]
pub(crate) fn resolve(dir: &Path, handle: &str, origins: &[String]) -> Result<Plain, VaultError> {
    resolve_entry(dir, handle, origins).map(|r| r.plain)
}

pub(crate) fn resolve_entry(dir: &Path, handle: &str, origins: &[String]) -> Result<Resolved, VaultError> {
    let primary = origins.first().cloned().unwrap_or_default();
    let fail = |e: VaultError| {
        audit::append(e.as_str(), &[("handle", handle), ("origin", &primary), ("via", "agent-desktop")]);
        e
    };
    if !handle.starts_with(HANDLE_PREFIX) {
        return Err(fail(VaultError::Miss));
    }
    let key_bytes = std::fs::read(dir.join("machine.key")).map_err(|_| fail(VaultError::NoVault))?;
    let key = FernetKey::from_encoded(&key_bytes).map_err(|_| fail(VaultError::BadFormat))?;
    let store = std::fs::read(dir.join("vault.enc.json")).map_err(|_| fail(VaultError::NoVault))?;
    let mut decrypted = key.decrypt(&store).map_err(|_| fail(VaultError::BadFormat))?;
    let parsed: Result<serde_json::Value, _> = serde_json::from_slice(&decrypted);
    decrypted.iter_mut().for_each(|b| *b = 0);
    let entries = parsed.map_err(|_| fail(VaultError::BadFormat))?;
    let Some(entry) = entries.get(handle) else {
        return Err(fail(VaultError::Miss));
    };
    if entry.get("revoked").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Err(fail(VaultError::Revoked));
    }
    if let Some(exp) = entry.get("expires_at").and_then(|v| v.as_f64()) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(f64::MAX);
        if now >= exp {
            return Err(fail(VaultError::Expired));
        }
    }
    let allowed: Vec<String> = entry
        .get("allowlisted_origins")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|s| s.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if allowed.is_empty() {
        return Err(fail(VaultError::NoOrigins));
    }
    if !origin_allowed(&allowed, origins) {
        return Err(fail(VaultError::OriginDenied));
    }
    let ciphertext = entry.get("ciphertext").and_then(|v| v.as_str()).unwrap_or("");
    if ciphertext.is_empty() {
        return Err(fail(VaultError::Revoked));
    }
    let fill_visible = entry.get("fill_visible").and_then(|v| v.as_bool()).unwrap_or(false);
    let plain = key.decrypt(ciphertext.as_bytes()).map_err(|_| fail(VaultError::BadFormat))?;
    let label = entry.get("label").and_then(|v| v.as_str()).unwrap_or("");
    audit::append("resolve_ok", &[("handle", handle), ("origin", &primary), ("label", label), ("via", "agent-desktop")]);
    Ok(Resolved { plain: Plain(plain), fill_visible })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixtures() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("secret").join("fixtures")
    }

    fn handle(name: &str) -> String {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fixtures().join("handles.json")).unwrap()).unwrap();
        v[name].as_str().unwrap().to_string()
    }

    #[test]
    fn decrypts_a_dark_room_vault_written_by_the_python_reference() {
        let p = resolve(&fixtures(), &handle("notepad"), &["app://c:/windows/notepad.exe".into(), "app://notepad.exe".into()]).unwrap();
        assert_eq!(p.as_str(), "CANARY-not-a-secret-7f3a");
        assert_eq!(p.char_count(), 24);
    }

    #[test]
    fn wrong_origin_is_denied() {
        let e = resolve(&fixtures(), &handle("notepad"), &["app://calc.exe".into()]).err().unwrap();
        assert_eq!(e, VaultError::OriginDenied);
        let e = resolve(&fixtures(), &handle("web"), &["https://example.com.evil.test".into()]).err().unwrap();
        assert_eq!(e, VaultError::OriginDenied);
        assert!(resolve(&fixtures(), &handle("web"), &["https://example.com/".into()]).is_ok());
    }

    #[test]
    fn empty_allowlist_revoked_and_unknown_handles_are_refused() {
        assert_eq!(resolve(&fixtures(), &handle("open"), &["app://notepad.exe".into()]).err().unwrap(), VaultError::NoOrigins);
        assert_eq!(resolve(&fixtures(), &handle("revoked"), &["app://notepad.exe".into()]).err().unwrap(), VaultError::Revoked);
        assert_eq!(resolve(&fixtures(), "dr_sec_nope", &["app://notepad.exe".into()]).err().unwrap(), VaultError::Miss);
        assert_eq!(resolve(&fixtures(), "plain-text", &[]).err().unwrap(), VaultError::Miss);
    }

    #[test]
    fn a_missing_vault_is_reported_without_detail() {
        let e = resolve(Path::new("/nonexistent/vault"), "dr_sec_x", &[]).err().unwrap();
        assert_eq!(e, VaultError::NoVault);
    }
}
