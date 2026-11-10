//! The person-only switch that turns the screen warning off.
//!
//! `config.json` and `--no-glow` cannot do it. The only file that does is
//! written by `glow set --off` after the person types a confirmation at a
//! real console. While that file is present, focus-taking commands refuse
//! instead of acting with the frame dark.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use agent_desktop_core::AppError;

pub(crate) const OFF_PHRASE: &str = "GLOW OFF";
const FILE_NAME: &str = "glow-person-off";
const FILE_BODY: &str = "person-off\n";

pub(crate) fn off_path() -> Option<PathBuf> {
    super::note_path(FILE_NAME)
}

pub(crate) fn is_off() -> bool {
    off_path().is_some_and(|path| file_is_off(&path))
}

pub(crate) fn file_is_off(path: &Path) -> bool {
    std::fs::read_to_string(path).is_ok_and(|body| body.trim() == FILE_BODY.trim())
}

pub(crate) fn phrase_authorizes_off(interactive: bool, line: &str) -> bool {
    interactive && line.trim() == OFF_PHRASE
}

pub(crate) fn write_off(path: &Path) -> Result<(), AppError> {
    write_exact(path, FILE_BODY)
}

pub(crate) fn clear_off(path: &Path) -> Result<(), AppError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::invalid_input(format!(
            "The glow warning could not be turned back on ({error})"
        ))),
    }
}

/// Asks on the console. A redirected stdin cannot confirm, so a command
/// the agent runs with a pipe cannot turn the warning off.
pub(crate) fn confirm_on_console() -> Result<(), AppError> {
    if !std::io::stdin().is_terminal() {
        return Err(AppError::invalid_input_with_suggestion(
            "The screen glow cannot be turned off from a script or a pipe",
            "From an interactive console, run `otherhand glow set --off` and type GLOW OFF when asked. Focus-taking commands refuse while the warning is off.",
        ));
    }
    let _ = writeln!(
        std::io::stderr(),
        "Type {OFF_PHRASE} to turn the screen warning off. Focus-taking commands will refuse until `otherhand glow set --on`."
    );
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|error| {
        AppError::invalid_input(format!("The confirmation could not be read ({error})"))
    })?;
    if phrase_authorizes_off(true, &line) {
        Ok(())
    } else {
        Err(AppError::invalid_input_with_suggestion(
            "The screen glow was left on",
            "Type GLOW OFF exactly, at an interactive console, to turn the warning off",
        ))
    }
}

fn write_exact(path: &Path, body: &str) -> Result<(), AppError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            AppError::invalid_input(format!(
                "The glow setting folder could not be created ({error})"
            ))
        })?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, body).map_err(|error| {
        AppError::invalid_input(format!("The glow setting could not be written ({error})"))
    })?;
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        AppError::invalid_input(format!("The glow setting could not be saved ({error})"))
    })
}

#[cfg(test)]
#[path = "person_tests.rs"]
mod tests;
