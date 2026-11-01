//! Exclusion fence rules: which programs, windows and paths the agent must
//! never read, touch or capture. The built-in floor is always present and a
//! fence file can only add to it.

use serde::Deserialize;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FenceFile {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub programs: ProgramRules,
    #[serde(default)]
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProgramRules {
    #[serde(default)]
    pub process_names: Vec<String>,
    #[serde(default)]
    pub exe_paths: Vec<String>,
    #[serde(default)]
    pub window_classes: Vec<String>,
    #[serde(default)]
    pub title_patterns: Vec<String>,
}

/// Password managers, credential and elevation prompts, Windows Security,
/// the agent host that shows approval cards, and Dark Room's own entry window.
pub(crate) const FLOOR_PROCESS_NAMES: &[&str] = &[
    "keepass.exe",
    "keepassxc.exe",
    "1password.exe",
    "bitwarden.exe",
    "dashlane.exe",
    "lastpass.exe",
    "nordpass.exe",
    "roboform.exe",
    "enpass.exe",
    "proton pass.exe",
    "keeper.exe",
    "consent.exe",
    "credentialuibroker.exe",
    "logonui.exe",
    "lockapp.exe",
    "sechealthui.exe",
    "securityhealthsystray.exe",
    "securityhealthhost.exe",
    "smartscreen.exe",
    "grok bot.exe",
];

pub(crate) const FLOOR_WINDOW_CLASSES: &[&str] = &[
    "Credential Dialog Xaml Host",
    "CredentialUIBroker",
    "$$$Secure UAP Dummy Window Class For Interim Dialog",
];

pub(crate) const FLOOR_TITLE_PATTERNS: &[&str] = &[
    "Windows Security",
    "User Account Control",
    "Dark Room*",
    "*fence.json*",
    "*KeePass*",
    "*Bitwarden*",
    "*1Password*",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rule {
    ProcessName,
    ExePath,
    WindowClass,
    TitlePattern,
    Path,
    UnknownIdentity,
}

impl Rule {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProcessName => "process_name",
            Self::ExePath => "exe_path",
            Self::WindowClass => "window_class",
            Self::TitlePattern => "title_pattern",
            Self::Path => "path",
            Self::UnknownIdentity => "unknown_identity",
        }
    }
}

/// What the fence knows about one process.
#[derive(Debug, Clone, Default)]
pub(crate) struct ProcessFacts {
    pub image_path: Option<String>,
    pub image_name: Option<String>,
}

/// What the fence knows about one top-level window.
#[derive(Debug, Clone, Default)]
pub(crate) struct WindowFacts {
    pub class: String,
    pub title: String,
    pub process: ProcessFacts,
    /// Folder an Explorer window shows, already fully resolved.
    pub folder: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct Fence {
    process_names: Vec<String>,
    exe_paths: Vec<String>,
    window_classes: Vec<String>,
    title_patterns: Vec<String>,
    roots: Vec<String>,
}

impl Fence {
    /// Floor plus the user's file. `protected_files` are always-fenced paths
    /// such as the fence file and the Dark Room vault directory.
    pub(crate) fn new(file: &FenceFile, protected_files: &[String]) -> Self {
        let lower = |items: &[String]| items.iter().map(|s| s.trim().to_lowercase()).collect::<Vec<_>>();
        let mut process_names: Vec<String> = FLOOR_PROCESS_NAMES.iter().map(|s| s.to_string()).collect();
        process_names.extend(lower(&file.programs.process_names));
        let mut window_classes: Vec<String> = FLOOR_WINDOW_CLASSES.iter().map(|s| s.to_lowercase()).collect();
        window_classes.extend(lower(&file.programs.window_classes));
        let mut title_patterns: Vec<String> = FLOOR_TITLE_PATTERNS.iter().map(|s| s.to_lowercase()).collect();
        title_patterns.extend(lower(&file.programs.title_patterns));
        let exe_paths = file.programs.exe_paths.iter().map(|p| normalize_path(p)).collect();
        let mut roots: Vec<String> = file.paths.iter().map(|p| normalize_path(p)).collect();
        roots.extend(protected_files.iter().map(|p| normalize_path(p)));
        roots.retain(|r| !r.is_empty());
        Self {
            process_names: dedup(process_names),
            exe_paths: dedup(exe_paths),
            window_classes: dedup(window_classes),
            title_patterns: dedup(title_patterns),
            roots: dedup(roots),
        }
    }

    pub(crate) fn roots(&self) -> &[String] {
        &self.roots
    }

    pub(crate) fn add_resolved_root(&mut self, root: &str) {
        let n = normalize_path(root);
        if !n.is_empty() && !self.roots.contains(&n) {
            self.roots.push(n);
        }
    }

    pub(crate) fn add_resolved_exe_path(&mut self, path: &str) {
        let n = normalize_path(path);
        if !n.is_empty() && !self.exe_paths.contains(&n) {
            self.exe_paths.push(n);
        }
    }

    pub(crate) fn exe_paths(&self) -> &[String] {
        &self.exe_paths
    }

    pub(crate) fn match_process(&self, facts: &ProcessFacts) -> Option<Rule> {
        let name = facts
            .image_name
            .clone()
            .or_else(|| facts.image_path.as_deref().map(file_name))
            .map(|n| n.to_lowercase());
        let Some(name) = name else {
            return Some(Rule::UnknownIdentity);
        };
        if self.process_names.iter().any(|p| glob_match(p, &name)) {
            return Some(Rule::ProcessName);
        }
        if let Some(path) = facts.image_path.as_deref() {
            let path = normalize_path(path);
            if self.exe_paths.iter().any(|root| path_under(&path, root)) {
                return Some(Rule::ExePath);
            }
            if self.roots.iter().any(|root| path_under(&path, root)) {
                return Some(Rule::Path);
            }
        }
        None
    }

    pub(crate) fn match_window(&self, facts: &WindowFacts) -> Option<Rule> {
        if let Some(rule) = self.match_process(&facts.process) {
            return Some(rule);
        }
        let class = facts.class.to_lowercase();
        if self.window_classes.contains(&class) {
            return Some(Rule::WindowClass);
        }
        let title = facts.title.to_lowercase();
        if !title.is_empty() && self.title_patterns.iter().any(|p| glob_match(p, &title)) {
            return Some(Rule::TitlePattern);
        }
        match facts.folder.as_deref() {
            Some(folder) if self.path_is_fenced(folder) => return Some(Rule::Path),
            None if is_explorer_class(&facts.class) && !self.roots.is_empty() => {
                // Explorer whose folder could not be read: it may be showing a fenced one.
                return Some(Rule::UnknownIdentity);
            }
            _ => {}
        }
        None
    }

    /// Last component of every fenced root, for relative paths such as
    /// `..\Private\x` typed where the current folder is unknown.
    pub(crate) fn relative_mentions_root(&self, text: &str) -> bool {
        let leaves: Vec<&str> = self.roots.iter().filter_map(|r| r.rsplit('\\').next()).filter(|l| !l.is_empty() && !l.ends_with(':')).collect();
        text.split(['"', '\'', '\n', '\r', '\t', '<', '>', '|'])
            .map(|chunk| chunk.trim())
            .filter(|chunk| chunk.contains(['\\', '/']))
            .any(|chunk| {
                let n = normalize_path(chunk);
                n.split('\\').any(|part| leaves.contains(&part))
            })
    }

    /// `path` must already be fully resolved by the caller when possible.
    pub(crate) fn path_is_fenced(&self, path: &str) -> bool {
        let p = normalize_path(path);
        !p.is_empty() && self.roots.iter().any(|root| path_under(&p, root))
    }

    /// Whether free text (a typed value, a launch argument) names a fenced
    /// root. Catches the root written plainly anywhere in the text; callers
    /// also resolve path-shaped tokens and check them with `path_is_fenced`.
    /// Whether free text names a fenced program or matches a fenced window
    /// title pattern. Used to keep notes about fenced apps out of the jetpack.
    /// Process names count only when written whole (`mybank.exe`), so a common
    /// word that happens to be a program name does not trip it.
    pub(crate) fn text_mentions_fenced_program(&self, text: &str) -> bool {
        let t = text.to_lowercase();
        let named = self.process_names.iter().any(|p| {
            let core = p.trim_matches('*');
            !core.contains('*') && core.len() >= 5 && t.contains(core)
        });
        named || self.title_patterns.iter().any(|p| p.contains('*') && glob_match(p, &t))
    }

    pub(crate) fn text_mentions_fenced_root(&self, text: &str) -> bool {
        let t = percent_decode(text).to_lowercase().replace('/', "\\");
        self.roots.iter().any(|root| {
            let r = root.trim_end_matches('\\');
            !r.is_empty() && contains_path_prefix(&t, r)
        })
    }
}

fn contains_path_prefix(text: &str, root: &str) -> bool {
    let mut start = 0;
    while let Some(i) = text[start..].find(root) {
        let end = start + i + root.len();
        match text[end..].chars().next() {
            None | Some('\\') | Some('"') | Some('\'') | Some(' ') | Some('\n') | Some('\t') | Some(';') | Some(',') | Some(':') | Some(')') | Some(']') | Some('>') | Some('|') => return true,
            Some('.') if text[end..].trim_start_matches(['.', ' ']).chars().next().is_none_or(|c| c == '\\' || c == ':') => return true,
            _ => {}
        }
        start = start + i + 1;
        if start >= text.len() {
            break;
        }
    }
    false
}

fn dedup(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v.retain(|s| !s.is_empty());
    v
}

pub(crate) fn file_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_string()
}

/// Lowercase, backslashes, no `\\?\` prefix, `\\?\UNC\` back to `\\`,
/// no trailing separator, `.` and `..` folded. Each component also loses what
/// Windows ignores when it opens a file: an alternate data stream suffix
/// (`name:stream`, `name::$DATA`) and trailing dots and spaces. A local
/// administrative share (`\\localhost\c$\x`, `\\127.0.0.1\c$\x`, this
/// computer's own name) becomes the drive path it points at.
pub(crate) fn normalize_path(path: &str) -> String {
    let mut p = path.trim().trim_matches('"').trim().replace('/', "\\").to_lowercase();
    if let Some(rest) = p.strip_prefix("\\\\?\\unc\\") {
        p = format!("\\\\{rest}");
    } else if let Some(rest) = p.strip_prefix("\\\\?\\") {
        p = rest.to_string();
    } else if let Some(rest) = p.strip_prefix("\\\\.\\") {
        p = rest.to_string();
    }
    let unc = p.starts_with("\\\\");
    let body = if unc { &p[2..] } else { &p[..] };
    let mut parts: Vec<String> = Vec::new();
    for (index, raw) in body.split('\\').enumerate() {
        let is_drive = index == 0 && !unc && raw.len() == 2 && raw.as_bytes()[0].is_ascii_alphabetic() && raw.ends_with(':');
        let is_host = index == 0 && unc;
        let part: &str = if is_drive || is_host {
            raw
        } else {
            let no_stream = match raw.find(':') {
                Some(i) => &raw[..i],
                None => raw,
            };
            if no_stream == "." || no_stream == ".." { no_stream } else { no_stream.trim_end_matches(['.', ' ']) }
        };
        match part {
            "" | "." => {}
            ".." => {
                if parts.len() > 1 {
                    parts.pop();
                }
            }
            other => parts.push(other.to_string()),
        }
    }
    if unc && parts.len() >= 2 && is_local_host(&parts[0]) {
        let share = parts[1].as_bytes();
        if share.len() == 2 && share[0].is_ascii_alphabetic() && share[1] == b'$' {
            let drive = format!("{}:", share[0] as char);
            let rest = parts[2..].to_vec();
            parts = std::iter::once(drive).chain(rest).collect();
            return parts.join("\\");
        }
    }
    let joined = parts.join("\\");
    if unc { format!("\\\\{joined}") } else { joined }
}

/// Names that reach this computer over SMB.
fn is_local_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']);
    if matches!(h, "localhost" | "::1" | "0--1.ipv6-literal.net" | ".") || h.starts_with("127.") {
        return true;
    }
    local_computer_name().is_some_and(|n| n == h || h.strip_prefix(&n).is_some_and(|r| r.starts_with('.')))
}

fn local_computer_name() -> Option<String> {
    static NAME: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        #[cfg(target_os = "windows")]
        {
            use windows_sys::Win32::System::SystemInformation::{ComputerNameNetBIOS, GetComputerNameExW};
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            if unsafe { GetComputerNameExW(ComputerNameNetBIOS, buf.as_mut_ptr(), &mut len) } != 0 {
                return Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase());
            }
            None
        }
        #[cfg(not(target_os = "windows"))]
        {
            std::env::var("COMPUTERNAME").ok().map(|n| n.to_lowercase()).filter(|n| !n.is_empty())
        }
    })
    .clone()
}

/// Expands `%NAME%` references the way a file dialog or shell would.
/// Unknown names are left as they are.
pub(crate) fn expand_env(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 && !after[..end].contains(char::is_whitespace) => {
                let name = &after[..end];
                match lookup(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `%41` style escapes, as in `file:///C:/Priv%61te`.
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn is_explorer_class(class: &str) -> bool {
    class.eq_ignore_ascii_case("CabinetWClass") || class.eq_ignore_ascii_case("ExploreWClass")
}

/// Component-wise prefix test on normalized paths.
pub(crate) fn path_under(path: &str, root: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    path == root || (path.starts_with(root) && path[root.len()..].starts_with('\\'))
}

/// Case-sensitive glob over already-lowercased input: `*` any run, `?` one char.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Path-shaped tokens in free text: `X:\...` and `\\server\...` runs up to
/// a quote, newline or tab. Spaces are kept because Windows paths use them.
pub(crate) fn path_tokens(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let drive = i + 2 < chars.len()
            && chars[i].is_ascii_alphabetic()
            && chars[i + 1] == ':'
            && (chars[i + 2] == '\\' || chars[i + 2] == '/')
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric());
        let unc = i + 1 < chars.len() && chars[i] == '\\' && chars[i + 1] == '\\';
        if drive || unc {
            let mut j = i;
            while j < chars.len() && !matches!(chars[j], '"' | '\'' | '\n' | '\r' | '\t' | '<' | '>' | '|') {
                j += 1;
            }
            out.push(chars[i..j].iter().collect::<String>().trim_end().to_string());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
