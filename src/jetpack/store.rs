//! The jetpack folder on disk. Plain Markdown, readable and editable by the
//! person in any text editor.

use serde_json::{Value, json};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_READ_BYTES: usize = 64 * 1024;
const MAX_MATCHES: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Section {
    Index,
    Preferences,
    Recipes,
    Log,
}

pub(crate) struct Store {
    root: PathBuf,
}

/// An app name as a safe file name: lowercase letters, digits, dots and
/// dashes, never a path.
pub(crate) fn slug(app: &str) -> String {
    let s: String = app
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    let s = s.trim_matches(|c| c == '.' || c == '-').to_string();
    s.chars().take(64).collect()
}

impl Store {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// `<profile>\.otherhand\jetpack`. Not overridable from the environment,
    /// so an agent can't point the writes somewhere else.
    pub(crate) fn at_default() -> Option<Self> {
        crate::fence::audit::home_dir().map(|h| Self::new(h.join(".otherhand").join("jetpack")))
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn ensure(&self) -> std::io::Result<()> {
        for dir in [self.root.clone(), self.root.join("recipes"), self.root.join("sessions")] {
            fs::create_dir_all(&dir)?;
            private_dir(&dir);
        }
        let index = self.root.join("INDEX.md");
        if !index.exists() {
            self.write_index()?;
        }
        let prefs = self.root.join("preferences.md");
        if !prefs.exists() {
            fs::write(&prefs, "# Preferences\n\nHow the person likes things done. One line each.\n\n")?;
            private_file(&prefs);
        }
        Ok(())
    }

    pub(crate) fn read(&self, section: Section, app: Option<&str>) -> std::io::Result<Value> {
        self.ensure()?;
        Ok(match section {
            Section::Index => json!({ "section": "index", "text": bounded(&fs::read_to_string(self.root.join("INDEX.md"))?) }),
            Section::Preferences => json!({ "section": "preferences", "text": bounded(&fs::read_to_string(self.root.join("preferences.md"))?) }),
            Section::Recipes => match app {
                Some(a) => {
                    let path = self.recipe_path(a)?;
                    let text = fs::read_to_string(&path).unwrap_or_default();
                    json!({ "section": "recipes", "app": slug(a), "text": bounded(&text) })
                }
                None => json!({ "section": "recipes", "apps": self.recipe_apps()? }),
            },
            Section::Log => {
                let mut days = self.session_days()?;
                days.sort();
                let last = days.last().cloned();
                let text = match &last {
                    Some(d) => fs::read_to_string(self.root.join("sessions").join(format!("{d}.md")))?,
                    None => String::new(),
                };
                json!({ "section": "log", "day": last, "days": days.len(), "text": bounded(&text) })
            }
        })
    }

    pub(crate) fn add_preference(&self, text: &str) -> std::io::Result<PathBuf> {
        self.ensure()?;
        let path = self.root.join("preferences.md");
        append(&path, &format!("- {}\n", one_line(text)))?;
        Ok(path)
    }

    pub(crate) fn log(&self, text: &str) -> std::io::Result<PathBuf> {
        self.ensure()?;
        let (day, time) = utc_now();
        let path = self.root.join("sessions").join(format!("{day}.md"));
        if !path.exists() {
            append(&path, &format!("# Session log {day} (times are UTC)\n\n"))?;
        }
        append(&path, &format!("- {time} {}\n", one_line(text)))?;
        Ok(path)
    }

    pub(crate) fn add_recipe(&self, app: &str, reference: Option<&str>, role: Option<&str>, text: &str) -> std::io::Result<PathBuf> {
        self.ensure()?;
        let path = self.recipe_path(app)?;
        if !path.exists() {
            append(&path, &format!("# {}\n\nHow to get things done in this app.\n", slug(app)))?;
        }
        let (day, time) = utc_now();
        let mut entry = format!("\n## {day} {time} UTC\n");
        if let Some(r) = reference {
            entry.push_str(&format!("ref: {}\n", one_line(r)));
        }
        if let Some(r) = role {
            entry.push_str(&format!("role: {}\n", one_line(r)));
        }
        entry.push_str(text.trim());
        entry.push('\n');
        append(&path, &entry)?;
        self.write_index()?;
        Ok(path)
    }

    /// Recipe entries that contain every word of the query, newest first.
    pub(crate) fn find_recipes(&self, query: &str, app: Option<&str>) -> std::io::Result<Vec<Value>> {
        self.ensure()?;
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let apps = match app {
            Some(a) => vec![slug(a)],
            None => self.recipe_apps()?,
        };
        let mut out = Vec::new();
        for a in apps {
            let Ok(text) = fs::read_to_string(self.root.join("recipes").join(format!("{a}.md"))) else {
                continue;
            };
            let mut entries: Vec<&str> = text.split("\n## ").skip(1).collect();
            entries.reverse();
            for e in entries {
                let lower = e.to_lowercase();
                if words.iter().all(|w| lower.contains(w.as_str()) || a.contains(w.as_str())) {
                    out.push(json!({ "app": a, "entry": format!("## {}", e.trim_end()) }));
                    if out.len() >= MAX_MATCHES {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    fn recipe_path(&self, app: &str) -> std::io::Result<PathBuf> {
        let s = slug(app);
        if s.is_empty() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "empty app name"));
        }
        Ok(self.root.join("recipes").join(format!("{s}.md")))
    }

    fn recipe_apps(&self) -> std::io::Result<Vec<String>> {
        let mut apps: Vec<String> = fs::read_dir(self.root.join("recipes"))?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".md")).map(str::to_string))
            .collect();
        apps.sort();
        Ok(apps)
    }

    fn session_days(&self) -> std::io::Result<Vec<String>> {
        Ok(fs::read_dir(self.root.join("sessions"))?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".md")).map(str::to_string))
            .collect())
    }

    fn write_index(&self) -> std::io::Result<()> {
        let apps = self.recipe_apps().unwrap_or_default();
        let mut text = String::from(
            "# Jetpack\n\nWhat this computer has taught the agent. Plain files; edit or delete anything.\n\n\
             - preferences.md: how the person likes things done\n\
             - sessions/: a log, one file per day\n\
             - recipes/: per-app notes (element refs, roles, what works)\n\n\
             Never stored here: passwords, keys, Dark Room handles, or anything from a fenced window.\n\n## Recipes\n\n",
        );
        if apps.is_empty() {
            text.push_str("(none yet)\n");
        }
        for a in &apps {
            let n = fs::read_to_string(self.root.join("recipes").join(format!("{a}.md")))
                .map(|t| t.matches("\n## ").count())
                .unwrap_or(0);
            text.push_str(&format!("- {a}: {n} {}\n", if n == 1 { "entry" } else { "entries" }));
        }
        let path = self.root.join("INDEX.md");
        fs::write(&path, text)?;
        private_file(&path);
        Ok(())
    }
}

fn append(path: &Path, text: &str) -> std::io::Result<()> {
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    private_file(path);
    f.write_all(text.as_bytes())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded(text: &str) -> String {
    if text.len() <= MAX_READ_BYTES {
        return text.to_string();
    }
    let mut end = text.len() - MAX_READ_BYTES;
    while !text.is_char_boundary(end) {
        end += 1;
    }
    format!("(older part left out)\n{}", &text[end..])
}

#[cfg(unix)]
fn private_dir(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn private_dir(_p: &Path) {}

#[cfg(unix)]
fn private_file(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o600));
}
#[cfg(not(unix))]
fn private_file(_p: &Path) {}

/// Today's date and the time of day in UTC, without a time crate.
pub(crate) fn utc_now() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    civil(secs)
}

/// Seconds since 1970 as a UTC date and time (Howard Hinnant's civil-from-days).
pub(crate) fn civil(secs: u64) -> (String, String) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (format!("{y:04}-{m:02}-{d:02}"), format!("{:02}:{:02}", rem / 3600, (rem % 3600) / 60))
}
