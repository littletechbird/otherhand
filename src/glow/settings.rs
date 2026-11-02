//! `otherhand glow show`, `glow set` and `glow reset`.
//!
//! They read and write the `glow` block of `<profile>\.otherhand\config.json`,
//! the same file the running glow already loads. The next command that draws
//! the frame picks the saved values up. `countdown_ms` is only stored.

use super::{
    DEFAULT_COLOR, DEFAULT_LINGER_MS, DEFAULT_OPACITY, DEFAULT_THICKNESS, MAX_COUNTDOWN_MS,
    MAX_EXTRA_COLORS, config_from, format_color, parse_color, strip_bom,
};
use agent_desktop_core::AppError;
use serde_json::{Map, Value, json};
use std::ffi::OsString;
use std::path::Path;

const USAGE: &str = "usage: otherhand glow show
       otherhand glow set [--on|--off] [--thickness N] [--color HEX] [--colors HEX,HEX] [--opacity N] [--brightness N] [--countdown-ms N]
       otherhand glow reset";

const APPLIES: &str = "next_glowing_command";

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
            apply_set(&mut doc, &words)?;
            write_doc(path, &doc)?;
            Ok(view(&doc, path))
        }
        "reset" => {
            reject_extra(&words, "reset")?;
            let mut doc = read_doc(path)?;
            insert_glow(&mut doc, defaults_value());
            write_doc(path, &doc)?;
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
    enabled: Option<bool>,
    thickness: Option<u32>,
    colors: Option<Vec<u32>>,
    opacity: Option<u8>,
    countdown_ms: Option<u64>,
}

fn apply_set(doc: &mut Value, words: &[&str]) -> Result<(), AppError> {
    let patch = parse_set(words)?;
    let mut glow = glow_object(doc);
    if let Some(on) = patch.enabled {
        glow.insert("enabled".into(), json!(on));
    }
    if let Some(thickness) = patch.thickness {
        glow.insert("thickness".into(), json!(thickness));
    }
    if let Some(colors) = &patch.colors {
        let text: Vec<String> = colors.iter().copied().map(format_color).collect();
        glow.insert("color".into(), json!(text[0]));
        glow.insert("colors".into(), json!(text));
    }
    if let Some(opacity) = patch.opacity {
        glow.insert("opacity".into(), json!(opacity));
        glow.remove("brightness");
    }
    if let Some(ms) = patch.countdown_ms {
        glow.insert("countdown_ms".into(), json!(ms));
    }
    insert_glow(doc, Value::Object(glow));
    Ok(())
}

fn parse_set(words: &[&str]) -> Result<Patch, AppError> {
    let mut enabled = None;
    let mut thickness = None;
    let mut single = None;
    let mut many = None;
    let mut opacity = None;
    let mut countdown_ms = None;
    let mut i = 0;
    while i < words.len() {
        let flag = words[i];
        match flag {
            "--on" => set_enabled(&mut enabled, true)?,
            "--off" => set_enabled(&mut enabled, false)?,
            "--thickness" => thickness = Some(parse_thickness(need(&mut i, words, flag)?)?),
            "--color" => {
                if many.is_some() {
                    return Err(usage("pass --color or --colors, not both"));
                }
                single = Some(parse_one_color(need(&mut i, words, flag)?)?);
            }
            "--colors" => {
                if single.is_some() {
                    return Err(usage("pass --color or --colors, not both"));
                }
                many = Some(parse_colors(need(&mut i, words, flag)?)?);
            }
            "--opacity" | "--brightness" => {
                let parsed = parse_opacity(need(&mut i, words, flag)?)?;
                if opacity.is_some_and(|prev| prev != parsed) {
                    return Err(usage("pass --opacity or --brightness, not both"));
                }
                opacity = Some(parsed);
            }
            "--countdown-ms" => countdown_ms = Some(parse_countdown(need(&mut i, words, flag)?)?),
            other => return Err(usage(&format!("unknown flag '{other}'"))),
        }
        i += 1;
    }
    let colors = many.or_else(|| single.map(|color| vec![color]));
    if enabled.is_none()
        && thickness.is_none()
        && colors.is_none()
        && opacity.is_none()
        && countdown_ms.is_none()
    {
        return Err(usage("set needs at least one setting"));
    }
    Ok(Patch {
        enabled,
        thickness,
        colors,
        opacity,
        countdown_ms,
    })
}

fn set_enabled(slot: &mut Option<bool>, value: bool) -> Result<(), AppError> {
    if slot.is_some_and(|prev| prev != value) {
        return Err(usage("pass --on or --off, not both"));
    }
    *slot = Some(value);
    Ok(())
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
        Ok(n) if (2..=24).contains(&n) => Ok(n),
        _ => Err(AppError::invalid_input_with_suggestion(
            format!("thickness '{raw}' must be an integer from 2 to 24"),
            "Pass --thickness 12, or another integer in that range",
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

fn parse_colors(raw: &str) -> Result<Vec<u32>, AppError> {
    let parts: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(AppError::invalid_input_with_suggestion(
            "colors needs at least one hex color",
            "Pass --colors #00D4FF,#7C5CFF",
        ));
    }
    if parts.len() > 1 + MAX_EXTRA_COLORS {
        return Err(AppError::invalid_input_with_suggestion(
            format!("colors accepts at most {} colors", 1 + MAX_EXTRA_COLORS),
            "Pass one to four #RGB or #RRGGBB colors, separated by commas",
        ));
    }
    parts.into_iter().map(parse_one_color).collect()
}

fn parse_opacity(raw: &str) -> Result<u8, AppError> {
    match raw.trim().parse::<u32>() {
        Ok(n) if n <= 100 => Ok(n as u8),
        _ => Err(AppError::invalid_input_with_suggestion(
            format!("opacity '{raw}' must be an integer from 0 to 100"),
            "100 is full brightness. --brightness is the same setting",
        )),
    }
}

fn parse_countdown(raw: &str) -> Result<u64, AppError> {
    match raw.trim().parse::<u64>() {
        Ok(n) if n <= MAX_COUNTDOWN_MS => Ok(n),
        _ => Err(AppError::invalid_input_with_suggestion(
            format!(
                "countdown-ms '{raw}' must be a whole number of milliseconds from 0 to {MAX_COUNTDOWN_MS}"
            ),
            "This only saves the timing. The countdown itself is not shown yet",
        )),
    }
}

fn glow_object(doc: &Value) -> Map<String, Value> {
    match doc.get("glow") {
        Some(Value::Bool(on)) => Map::from_iter([("enabled".into(), json!(on))]),
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
    let color = format_color(DEFAULT_COLOR);
    json!({
        "enabled": true,
        "color": color,
        "colors": [color],
        "thickness": DEFAULT_THICKNESS,
        "opacity": DEFAULT_OPACITY,
        "linger_ms": DEFAULT_LINGER_MS,
        "require_capture_exclusion": false,
    })
}

fn view(doc: &Value, path: &Path) -> Value {
    let cfg = config_from(Some(doc));
    let (colors, n) = cfg.palette();
    json!({
        "enabled": cfg.enabled,
        "color": format_color(cfg.color),
        "colors": colors[..n].iter().copied().map(format_color).collect::<Vec<_>>(),
        "thickness": cfg.thickness,
        "opacity": cfg.opacity,
        "countdown_ms": countdown_of(doc),
        "linger_ms": cfg.linger_ms,
        "require_capture_exclusion": cfg.strict,
        "path": path.display().to_string(),
        "applies": APPLIES,
    })
}

fn countdown_of(doc: &Value) -> Value {
    match doc
        .get("glow")
        .and_then(|glow| glow.get("countdown_ms"))
        .and_then(Value::as_u64)
    {
        Some(ms) if ms <= MAX_COUNTDOWN_MS => json!(ms),
        _ => Value::Null,
    }
}

fn read_doc(path: &Path) -> Result<Value, AppError> {
    match std::fs::read(path) {
        Ok(bytes) if bytes.is_empty() => Ok(json!({})),
        Ok(bytes) => parse_doc(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(error) => Err(AppError::invalid_input(format!(
            "could not read {}: {error}",
            path.display()
        ))),
    }
}

fn parse_doc(bytes: &[u8]) -> Result<Value, AppError> {
    match serde_json::from_slice::<Value>(strip_bom(bytes)) {
        Ok(Value::Object(map)) => Ok(Value::Object(map)),
        Ok(_) => Err(AppError::invalid_input("config.json must be a JSON object")),
        Err(_) => Err(AppError::invalid_input("config.json is not valid JSON")),
    }
}

fn write_doc(path: &Path, doc: &Value) -> Result<(), AppError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|error| {
            AppError::invalid_input(format!("could not create {}: {error}", dir.display()))
        })?;
    }
    let body = serde_json::to_vec_pretty(doc)
        .map_err(|error| AppError::invalid_input(format!("could not encode settings: {error}")))?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, &body)
        .map_err(|error| AppError::invalid_input(format!("could not write settings: {error}")))?;
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AppError::invalid_input(format!(
            "could not save settings: {error}"
        )));
    }
    Ok(())
}

fn usage(what: &str) -> AppError {
    AppError::invalid_input(format!("{what}\n{USAGE}"))
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
