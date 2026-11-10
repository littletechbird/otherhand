//! `otherhand glow show`, `glow set` and `glow reset`.
//!
//! They read and write the `glow` block of `<profile>\.otherhand\config.json`.
//! The next command that draws the frame picks the saved color, thickness,
//! linger and countdown up. Turning the warning off is not one of those
//! fields: `glow set --off` writes the person-only switch after a console
//! confirmation, and focus-taking commands refuse while it is set.

use super::warn::{MAX_COUNTDOWN_MS, MIN_COUNTDOWN_MS, clamp_countdown};
use super::{
    DEFAULT_COLOR, DEFAULT_LINGER_MS, DEFAULT_THICKNESS, MAX_THICKNESS, MIN_THICKNESS, config_from,
    format_color, parse_color, strip_bom,
};
use agent_desktop_core::AppError;
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::path::Path;

const USAGE: &str = "usage: otherhand glow show
       otherhand glow set [--on] [--thickness N] [--color HEX] [--linger-ms N] [--countdown-ms N]
       otherhand glow set --off
       otherhand glow reset";

/// `Some` when argv is a glow-settings command.
pub(crate) fn from_args(args: &[OsString]) -> Option<(String, Result<Value, AppError>)> {
    if args.get(1).and_then(|a| a.to_str()) != Some("glow") {
        return None;
    }
    let rest = match args[2..]
        .iter()
        .map(|a| a.to_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
    {
        Some(r) => r,
        None => {
            return Some((
                "glow".into(),
                Err(AppError::invalid_input(
                    "glow arguments must be valid Unicode",
                )),
            ));
        }
    };
    let name = match rest.first().map(String::as_str) {
        Some(verb @ ("show" | "set" | "reset")) => format!("glow {verb}"),
        _ => "glow".to_string(),
    };
    let Some(path) = super::config_path() else {
        return Some((
            name,
            Err(AppError::invalid_input(
                "The profile folder can't be found, so glow settings have nowhere to live",
            )),
        ));
    };
    Some((name, run(&path, &rest)))
}

pub(crate) fn run(path: &Path, args: &[String]) -> Result<Value, AppError> {
    let mut words: Vec<&str> = args.iter().map(String::as_str).collect();
    let verb = if words.is_empty() {
        ""
    } else {
        words.remove(0)
    };
    match verb {
        "show" => {
            reject_extra(&words, "show")?;
            Ok(view(&read_doc(path)?, path))
        }
        "set" => {
            let mut doc = read_doc(path)?;
            apply_set(path, &mut doc, &words)?;
            write_doc(path, &doc)?;
            Ok(view(&doc, path))
        }
        "reset" => {
            reject_extra(&words, "reset")?;
            let mut doc = read_doc(path)?;
            insert_glow(&mut doc, defaults_value());
            write_doc(path, &doc)?;
            super::person::clear_off(&off_beside(path)?)?;
            Ok(view(&doc, path))
        }
        _ => Err(usage("unknown glow verb")),
    }
}

fn reject_extra(words: &[&str], verb: &str) -> Result<(), AppError> {
    if words.is_empty() {
        Ok(())
    } else {
        Err(usage(&format!("{verb} takes no arguments")))
    }
}

struct Patch {
    on: bool,
    off: bool,
    thickness: Option<u32>,
    color: Option<u32>,
    linger_ms: Option<u64>,
    countdown_ms: Option<u64>,
}

fn apply_set(config: &Path, doc: &mut Value, words: &[&str]) -> Result<(), AppError> {
    let patch = parse_set(words)?;
    if patch.off {
        super::person::confirm_on_console()?;
        let path = off_beside(config)?;
        super::person::write_off(&path)?;
    }
    if patch.on {
        let path = off_beside(config)?;
        super::person::clear_off(&path)?;
    }
    let mut glow = glow_object(doc);
    if let Some(thickness) = patch.thickness {
        glow.insert("thickness".into(), json!(thickness));
    }
    if let Some(color) = patch.color {
        glow.insert("color".into(), json!(format_color(color)));
    }
    if let Some(linger) = patch.linger_ms {
        glow.insert("linger_ms".into(), json!(linger));
    }
    if let Some(ms) = patch.countdown_ms {
        glow.insert("countdown_ms".into(), json!(ms));
    }
    glow.remove("enabled");
    insert_glow(doc, Value::Object(glow));
    Ok(())
}

fn off_beside(config: &Path) -> Result<std::path::PathBuf, AppError> {
    let dir = config.parent().ok_or_else(|| {
        AppError::invalid_input("The glow config has no folder to keep the person switch in")
    })?;
    Ok(dir.join("glow-person-off"))
}

fn parse_set(words: &[&str]) -> Result<Patch, AppError> {
    let mut patch = Patch {
        on: false,
        off: false,
        thickness: None,
        color: None,
        linger_ms: None,
        countdown_ms: None,
    };
    let mut i = 0;
    while i < words.len() {
        let flag = words[i];
        match flag {
            "--on" => patch.on = true,
            "--off" => patch.off = true,
            "--thickness" => patch.thickness = Some(parse_thickness(need(&mut i, words, flag)?)?),
            "--color" => patch.color = Some(parse_one_color(need(&mut i, words, flag)?)?),
            "--linger-ms" => patch.linger_ms = Some(parse_linger(need(&mut i, words, flag)?)?),
            "--countdown-ms" => {
                patch.countdown_ms = Some(parse_countdown(need(&mut i, words, flag)?)?)
            }
            other => return Err(usage(&format!("unknown flag '{other}'"))),
        }
        i += 1;
    }
    if patch.on && patch.off {
        return Err(usage("pass --on or --off, not both"));
    }
    if !patch.on
        && !patch.off
        && patch.thickness.is_none()
        && patch.color.is_none()
        && patch.linger_ms.is_none()
        && patch.countdown_ms.is_none()
    {
        return Err(usage("set needs at least one setting"));
    }
    Ok(patch)
}

fn need<'a>(i: &mut usize, words: &[&'a str], flag: &str) -> Result<&'a str, AppError> {
    *i += 1;
    words
        .get(*i)
        .copied()
        .ok_or_else(|| usage(&format!("{flag} needs a value")))
}

fn parse_thickness(raw: &str) -> Result<u32, AppError> {
    match raw.trim().parse::<u32>() {
        Ok(n) if (MIN_THICKNESS..=MAX_THICKNESS).contains(&n) => Ok(n),
        _ => Err(AppError::invalid_input_with_suggestion(
            format!("thickness '{raw}' must be an integer from {MIN_THICKNESS} to {MAX_THICKNESS}"),
            "Pass --thickness 24, or another integer in that range",
        )),
    }
}

fn parse_one_color(raw: &str) -> Result<u32, AppError> {
    parse_color(raw).ok_or_else(|| {
        AppError::invalid_input_with_suggestion(
            format!("color '{raw}' is not a hex color"),
            "Use #RGB or #RRGGBB, for example --color #00D4FF",
        )
    })
}

fn parse_linger(raw: &str) -> Result<u64, AppError> {
    match raw.trim().parse::<u64>() {
        Ok(n) if n <= 10_000 => Ok(n),
        _ => Err(AppError::invalid_input_with_suggestion(
            format!("linger-ms '{raw}' must be a whole number of milliseconds from 0 to 10000"),
            "linger-ms is the fade after the work session releases, not the warning countdown",
        )),
    }
}

fn parse_countdown(raw: &str) -> Result<u64, AppError> {
    match raw.trim().parse::<u64>() {
        Ok(n) if (MIN_COUNTDOWN_MS..=MAX_COUNTDOWN_MS).contains(&n) => Ok(n),
        _ => Err(AppError::invalid_input_with_suggestion(
            format!(
                "countdown-ms '{raw}' must be a whole number of milliseconds from {MIN_COUNTDOWN_MS} to {MAX_COUNTDOWN_MS}"
            ),
            "The countdown is shown before focus or input. It cannot be 0",
        )),
    }
}

fn glow_object(doc: &Value) -> Map<String, Value> {
    match doc.get("glow") {
        Some(Value::Object(map)) => map.clone(),
        _ => Map::new(),
    }
}

fn insert_glow(doc: &mut Value, glow: Value) {
    if let Value::Object(map) = doc {
        map.insert("glow".into(), glow);
    } else {
        *doc = json!({ "glow": glow });
    }
}

fn defaults_value() -> Value {
    json!({
        "color": format_color(DEFAULT_COLOR),
        "thickness": DEFAULT_THICKNESS,
        "linger_ms": DEFAULT_LINGER_MS,
        "countdown_ms": super::warn::DEFAULT_COUNTDOWN_MS,
        "require_capture_exclusion": false,
    })
}

fn view(doc: &Value, config: &Path) -> Value {
    let cfg = config_from(Some(doc));
    let person_off = off_beside(config).is_ok_and(|path| super::person::file_is_off(&path));
    json!({
        "color": format_color(cfg.color),
        "thickness": cfg.thickness,
        "linger_ms": cfg.linger_ms,
        "countdown_ms": clamp_countdown(cfg.countdown_ms),
        "require_capture_exclusion": cfg.strict,
        "person_off": person_off,
        "warning": if person_off { "off" } else { "on" },
        "config": config.display().to_string(),
    })
}

fn read_doc(path: &Path) -> Result<Value, AppError> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(strip_bom(&bytes)).or(Ok(json!({}))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(AppError::invalid_input(format!(
            "The glow config could not be read ({error})"
        ))),
    }
}

fn write_doc(path: &Path, doc: &Value) -> Result<(), AppError> {
    let body = serde_json::to_vec_pretty(doc).map_err(|error| {
        AppError::invalid_input(format!("The glow config could not be encoded ({error})"))
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            AppError::invalid_input(format!(
                "The glow config folder could not be created ({error})"
            ))
        })?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, body).map_err(|error| {
        AppError::invalid_input(format!("The glow config could not be written ({error})"))
    })?;
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        AppError::invalid_input(format!("The glow config could not be saved ({error})"))
    })
}

fn usage(detail: &str) -> AppError {
    AppError::invalid_input_with_suggestion(
        format!("{detail}. {USAGE}"),
        "Run `otherhand glow show` to read the current warning settings",
    )
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
