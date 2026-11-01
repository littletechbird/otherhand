//! The jetpack: a plain local folder of what the agent has learned about
//! this computer, so it does not have to relearn it every session.
//!
//! ```text
//! <home>\.otherhand\jetpack\
//!   INDEX.md          what is here, kept up to date by `recipe add`
//!   preferences.md    how the person likes things done
//!   recipes\<app>.md  per-app notes: element refs, roles, what works
//!   sessions\<date>.md  a running log, one file per day
//! ```
//!
//! Every write is screened first (see `guard`): no Dark Room handles, nothing
//! shaped like a secret, no fenced path, program or fenced window text. A
//! write is refused outright when the fence can't be read. Nothing leaves
//! the machine; there is no sync.

pub(crate) mod guard;
pub(crate) mod store;

use agent_desktop_core::{AdapterError, AppError, DeliverySemantics, ErrorCode};
use guard::{Guard, Refusal};
use serde_json::{Value, json};
use std::ffi::OsString;
use store::{Section, Store};

const USAGE: &str = "usage: otherhand jetpack read [index|preferences|recipes|log] [--app NAME]
       otherhand jetpack note [--preference] TEXT
       otherhand jetpack recipe add --app NAME [--ref REF] [--role ROLE] TEXT
       otherhand jetpack recipe find QUERY [--app NAME]
       otherhand jetpack path";

/// `Some(result)` when argv is a jetpack command, `None` for everything else.
pub(crate) fn from_args(args: &[OsString]) -> Option<(String, Result<Value, AppError>)> {
    if args.get(1).and_then(|a| a.to_str()) != Some("jetpack") {
        return None;
    }
    let rest: Vec<String> = match args[2..].iter().map(|a| a.to_str().map(str::to_string)).collect::<Option<Vec<_>>>() {
        Some(r) => r,
        None => return Some(("jetpack".into(), Err(AppError::invalid_input("jetpack arguments must be valid Unicode")))),
    };
    let name = match rest.first().map(String::as_str) {
        Some("recipe") => format!("jetpack recipe {}", rest.get(1).map(String::as_str).unwrap_or("")).trim().to_string(),
        Some(verb) => format!("jetpack {verb}"),
        None => "jetpack".to_string(),
    };
    let store = match Store::at_default() {
        Some(s) => s,
        None => return Some((name, Err(AppError::invalid_input("The profile folder can't be found, so there is no jetpack")))),
    };
    Some((name, run(&store, &rest, &live_guard)))
}

/// The fence and fenced window titles, read fresh for each write.
fn live_guard(f: &mut dyn FnMut(&Guard<'_>) -> Result<Value, AppError>) -> Result<Value, AppError> {
    let probe = crate::fence::platform_probe();
    let fence = crate::fence::load(probe.as_ref()).map_err(AppError::Adapter)?;
    let fenced_titles = probe
        .top_level_windows()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| fence.match_window(&w.facts).is_some())
        .map(|w| w.facts.title)
        .collect();
    let resolve = |p: &str| probe.resolve_path(p);
    let expand = |t: &str| crate::fence::policy::expand_env(t, &crate::fence::env_lookup);
    f(&Guard { fence: &fence, fenced_titles, resolve: &resolve, expand: &expand })
}

type WithGuard<'a> = &'a dyn Fn(&mut dyn FnMut(&Guard<'_>) -> Result<Value, AppError>) -> Result<Value, AppError>;

pub(crate) fn run(store: &Store, args: &[String], with_guard: WithGuard<'_>) -> Result<Value, AppError> {
    let mut words: Vec<&str> = args.iter().map(String::as_str).collect();
    let verb = if words.is_empty() { "" } else { words.remove(0) };
    match verb {
        "path" => Ok(json!({ "path": store.root().display().to_string() })),
        "read" => {
            let app = take_flag(&mut words, "--app")?;
            let section = match words.first().copied() {
                None | Some("index") => Section::Index,
                Some("preferences") => Section::Preferences,
                Some("recipes") => Section::Recipes,
                Some("log") => Section::Log,
                Some(other) => return Err(usage(&format!("unknown section '{other}'"))),
            };
            store.read(section, app.as_deref()).map_err(io)
        }
        "note" => {
            let preference = take_switch(&mut words, "--preference");
            let text = join_text(&words)?;
            with_guard(&mut |g| {
                g.screen(&text).map_err(refused)?;
                let path = if preference { store.add_preference(&text) } else { store.log(&text) }.map_err(io)?;
                Ok(json!({ "saved": true, "file": path.display().to_string() }))
            })
        }
        "recipe" => {
            let sub = if words.is_empty() { "" } else { words.remove(0) };
            match sub {
                "add" => {
                    let app = take_flag(&mut words, "--app")?.ok_or_else(|| usage("recipe add needs --app NAME"))?;
                    let reference = take_flag(&mut words, "--ref")?;
                    let role = take_flag(&mut words, "--role")?;
                    let text = join_text(&words)?;
                    with_guard(&mut |g| {
                        g.screen_app(&app).map_err(refused)?;
                        for extra in [&reference, &role].into_iter().flatten() {
                            g.screen(extra).map_err(refused)?;
                        }
                        g.screen(&text).map_err(refused)?;
                        let path = store.add_recipe(&app, reference.as_deref(), role.as_deref(), &text).map_err(io)?;
                        Ok(json!({ "saved": true, "app": store::slug(&app), "file": path.display().to_string() }))
                    })
                }
                "find" => {
                    let app = take_flag(&mut words, "--app")?;
                    let query = join_text(&words)?;
                    Ok(json!({ "matches": store.find_recipes(&query, app.as_deref()).map_err(io)? }))
                }
                _ => Err(usage("recipe needs add or find")),
            }
        }
        _ => Err(usage("unknown jetpack verb")),
    }
}

fn take_flag(words: &mut Vec<&str>, flag: &str) -> Result<Option<String>, AppError> {
    let Some(i) = words.iter().position(|w| *w == flag) else {
        return Ok(None);
    };
    if i + 1 >= words.len() {
        return Err(usage(&format!("{flag} needs a value")));
    }
    let value = words.remove(i + 1).to_string();
    words.remove(i);
    Ok(Some(value))
}

fn take_switch(words: &mut Vec<&str>, flag: &str) -> bool {
    let before = words.len();
    words.retain(|w| *w != flag);
    words.len() != before
}

fn join_text(words: &[&str]) -> Result<String, AppError> {
    let text = words.join(" ").trim().to_string();
    if text.is_empty() {
        return Err(usage("nothing to write"));
    }
    Ok(text)
}

fn usage(what: &str) -> AppError {
    AppError::invalid_input(format!("{what}\n{USAGE}"))
}

fn io(e: std::io::Error) -> AppError {
    AppError::invalid_input(format!("jetpack file error: {}", e.kind()))
}

fn refused(r: Refusal) -> AppError {
    crate::fence::audit::append("jetpack_refused", &[("reason", r.as_str())]);
    AppError::Adapter(
        AdapterError::new(ErrorCode::Fenced, r.message().to_string())
            .with_suggestion("Leave it out of the jetpack; write down how to find it instead of what it says")
            .with_details(json!({ "reason": r.as_str() }))
            .with_disposition(DeliverySemantics::not_delivered()),
    )
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
