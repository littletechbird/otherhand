//! What may never be written to the jetpack: Dark Room handles, anything
//! shaped like a secret, fenced paths and programs, and text the person's
//! fenced windows show (their titles, the only fenced text this tool reads).
//! Every check here is pure so it can be tested on any platform.

use crate::fence::policy::{Fence, ProcessFacts, path_tokens};

/// Why a write was refused. The refused text itself is never kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    DarkRoomHandle,
    SecretShaped,
    FencedPath,
    FencedProgram,
    FencedWindowText,
    TooLong,
    ControlCharacters,
}

impl Refusal {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DarkRoomHandle => "dark_room_handle",
            Self::SecretShaped => "secret_shaped",
            Self::FencedPath => "fenced_path",
            Self::FencedProgram => "fenced_program",
            Self::FencedWindowText => "fenced_window_text",
            Self::TooLong => "too_long",
            Self::ControlCharacters => "control_characters",
        }
    }

    pub(crate) fn message(self) -> &'static str {
        match self {
            Self::DarkRoomHandle => "The jetpack never stores Dark Room handles",
            Self::SecretShaped => {
                "That looks like a password, key or token, and the jetpack never stores secrets"
            }
            Self::FencedPath => "That names a fenced file or folder",
            Self::FencedProgram => "That names a fenced program",
            Self::FencedWindowText => "That repeats text shown by a fenced window",
            Self::TooLong => "A jetpack entry is limited to 4000 characters",
            Self::ControlCharacters => {
                "A jetpack entry may only hold printable text, tabs and newlines"
            }
        }
    }
}

pub(crate) const MAX_ENTRY_CHARS: usize = 4000;
const MIN_TITLE_CHARS: usize = 4;

/// The fence as it stands right now, plus the titles of fenced windows on screen.
pub(crate) struct Guard<'a> {
    pub fence: &'a Fence,
    pub fenced_titles: Vec<String>,
    /// Resolves a path the way the fence does (junctions, short names).
    pub resolve: &'a dyn Fn(&str) -> String,
    /// Expands `%NAME%` the way the fence does.
    pub expand: &'a dyn Fn(&str) -> String,
}

impl Guard<'_> {
    pub(crate) fn screen(&self, text: &str) -> Result<(), Refusal> {
        if text.chars().count() > MAX_ENTRY_CHARS {
            return Err(Refusal::TooLong);
        }
        if text
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
        {
            return Err(Refusal::ControlCharacters);
        }
        let lower = text.to_lowercase();
        if lower.contains(crate::secret::vault::HANDLE_PREFIX) {
            return Err(Refusal::DarkRoomHandle);
        }
        if looks_secret(text) {
            return Err(Refusal::SecretShaped);
        }
        let expanded = (self.expand)(text);
        for form in [
            text.to_string(),
            crate::fence::policy::percent_decode(text),
            expanded,
        ] {
            if self.fence.text_mentions_fenced_root(&form)
                || self.fence.relative_mentions_root(&form)
            {
                return Err(Refusal::FencedPath);
            }
            for token in path_tokens(&form) {
                if self.fence.path_is_fenced(&token)
                    || self.fence.path_is_fenced(&(self.resolve)(&token))
                {
                    return Err(Refusal::FencedPath);
                }
            }
        }
        if self.fence.text_mentions_fenced_program(&lower) {
            return Err(Refusal::FencedProgram);
        }
        for title in &self.fenced_titles {
            let t = title.trim().to_lowercase();
            if t.chars().count() >= MIN_TITLE_CHARS && lower.contains(&t) {
                return Err(Refusal::FencedWindowText);
            }
        }
        Ok(())
    }

    /// A recipe is about one app; an app the fence covers gets no recipe.
    pub(crate) fn screen_app(&self, app: &str) -> Result<(), Refusal> {
        self.screen(app)?;
        let name = if app.to_lowercase().ends_with(".exe") || !cfg!(windows) {
            app.to_string()
        } else {
            format!("{app}.exe")
        };
        let facts = ProcessFacts {
            image_name: Some(name),
            image_path: None,
        };
        if self.fence.match_process(&facts).is_some() {
            return Err(Refusal::FencedProgram);
        }
        Ok(())
    }
}

/// Passwords, keys and tokens by their usual shapes. Deliberately eager: a
/// refused note can be reworded, a stored secret can't be taken back.
pub(crate) fn looks_secret(text: &str) -> bool {
    let lower = text.to_lowercase();
    const MARKERS: [&str; 14] = [
        "-----begin",
        "password=",
        "password:",
        "passwd=",
        "pwd=",
        "secret=",
        "api_key",
        "apikey",
        "api-key",
        "access_token",
        "refresh_token",
        "bearer ",
        "private_key",
        "client_secret",
    ];
    if MARKERS.iter().any(|m| lower.contains(m)) {
        return true;
    }
    if assignment_phrase(&lower) || short_pin(&lower) {
        return true;
    }
    for word in text.split(|c: char| {
        c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == ';' || c == '(' || c == ')'
    }) {
        let w = word.trim_matches(|c: char| c == '.' || c == ':' || c == '`');
        const PREFIXES: [&str; 7] = [
            "sk-",
            "ghp_",
            "gho_",
            "github_pat_",
            "xoxb-",
            "xoxp-",
            "akia",
        ];
        if w.len() >= 16 && PREFIXES.iter().any(|p| w.to_lowercase().starts_with(p)) {
            return true;
        }
        if token_like(w) {
            return true;
        }
    }
    false
}

/// A long run of letters, digits and token punctuation that mixes cases and
/// digits (or is long hex), the way generated keys look and words don't.
fn assignment_phrase(lower: &str) -> bool {
    const HEADS: [&str; 6] = ["password", "passcode", "passwd", "pwd", "pin", "secret"];
    const VERBS: [&str; 2] = [" is ", " was "];
    for head in HEADS {
        for verb in VERBS {
            let needle = format!("{head}{verb}");
            if let Some(index) = lower.find(&needle) {
                let rest = lower[index + needle.len()..].trim_start();
                let word = rest.split_whitespace().next().unwrap_or("");
                let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
                if value_word(word) {
                    return true;
                }
            }
        }
    }
    false
}

fn value_word(word: &str) -> bool {
    const STOP: [&str; 10] = [
        "the", "a", "an", "this", "that", "field", "empty", "required", "first", "second",
    ];
    if word.is_empty() || STOP.contains(&word) {
        return false;
    }
    let digits = word.chars().all(|c| c.is_ascii_digit());
    digits && (4..=8).contains(&word.len()) || word.len() >= 4
}

fn short_pin(lower: &str) -> bool {
    let words: Vec<&str> = lower.split_whitespace().collect();
    for pair in words.windows(2) {
        let head = pair[0].trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if !matches!(head, "pin" | "passcode" | "code") {
            continue;
        }
        let num = pair[1].trim_matches(|c: char| !c.is_ascii_digit());
        if (4..=8).contains(&num.len()) && num.chars().all(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    false
}

fn token_like(w: &str) -> bool {
    if w.len() < 24
        || !w
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '+' | '/' | '='))
    {
        return false;
    }
    let upper = w.chars().filter(|c| c.is_ascii_uppercase()).count();
    let lower = w.chars().filter(|c| c.is_ascii_lowercase()).count();
    let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
    let hex = w.len() >= 32 && w.chars().all(|c| c.is_ascii_hexdigit());
    hex || (upper >= 2 && lower >= 2 && digits >= 2)
        || (w.len() >= 40 && digits >= 4 && upper + lower >= 10)
}
