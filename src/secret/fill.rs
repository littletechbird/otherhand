//! The `fill-secret` command body.

use agent_desktop_core::{
    AdapterError, AppError, DeliverySemantics, ErrorCode, PlatformAdapter,
    commands::{set_value as set_value_command, type_text as type_command},
    context::CommandContext,
    refs::refs_store::RefStore,
};
use serde_json::{Value, json};

use super::{origin, vault};
use crate::fence::audit;

pub(crate) struct FillSecretRequest {
    pub ref_id: String,
    pub handle: String,
    pub timeout_ms: Option<u64>,
    pub use_type: bool,
}

fn refused(code: ErrorCode, message: &str, suggestion: &str) -> AppError {
    AdapterError::new(code, message.to_string())
        .with_suggestion(suggestion.to_string())
        .with_disposition(DeliverySemantics::not_delivered())
        .into()
}

/// Strips everything but the error code from a failed fill, so no detail
/// computed from the plaintext (lengths, read-backs) can travel out.
fn scrub(error: AppError) -> AppError {
    let code = match &error {
        AppError::Adapter(a) => a.code.clone(),
        _ => ErrorCode::ActionFailed,
    };
    let disposition = match &error {
        AppError::Adapter(a) => a.disposition,
        _ => DeliverySemantics::uncertain(),
    };
    AdapterError::new(code, "The secret could not be filled into that field")
        .with_suggestion("Take a fresh snapshot and check the field; the secret was not shown anywhere")
        .with_disposition(disposition)
        .into()
}

pub(crate) fn execute(
    req: FillSecretRequest,
    adapter: &dyn PlatformAdapter,
    context: &CommandContext,
) -> Result<Value, AppError> {
    if context.trace_enabled() || tracing::level_filters::LevelFilter::current() >= tracing::level_filters::LevelFilter::DEBUG {
        return Err(refused(
            ErrorCode::PolicyDenied,
            "fill-secret refuses to run with tracing or verbose logging on",
            "Run it again without --trace, --verbose or RUST_LOG",
        ));
    }
    if !req.handle.starts_with(vault::HANDLE_PREFIX) {
        return Err(refused(ErrorCode::InvalidArgs, "HANDLE must be a Dark Room handle (dr_sec_...)", "Pass the handle, never the secret itself"));
    }
    let entry = RefStore::for_session(context.session_id())?.load_ref(&req.ref_id)?;
    // Is the field one whose text can be read back? Asked before anything is decrypted.
    let deadline = agent_desktop_core::Deadline::after(req.timeout_ms.unwrap_or(5_000).max(500))?;
    let secure = adapter
        .resolve_element_strict(&entry, deadline)
        .ok()
        .and_then(|h| adapter.get_live_state(&h, deadline).ok().flatten())
        .is_some_and(|s| agent_desktop_core::tree::state::has_state(&s.states, agent_desktop_core::tree::state::SECURE));
    let probe = crate::fence::platform_probe();
    let facts = probe.process(entry.process.pid.get());
    let origins: Vec<String> = if origin::is_chromium(&facts) {
        let window = entry.source.source_window_id.as_deref().unwrap_or("");
        match origin::page_origin(origin::chromium_document_url(window).as_deref(), origin::chromium_address(window).as_deref()) {
            Some(o) => vec![o],
            None => {
                audit::append("resolve_origin_unknown", &[("handle", &req.handle), ("via", "agent-desktop")]);
                return Err(refused(
                    ErrorCode::PolicyDenied,
                    "The page's full URL could not be read, or the address bar disagrees with it, so the secret was not used",
                    "Make sure the browser window shows its address bar, then snapshot and retry",
                ));
            }
        }
    } else {
        origin::native_origins(&facts)
    };
    let dir = audit::darkroom_dir().ok_or_else(|| refused(ErrorCode::InvalidArgs, "No Dark Room vault directory", "Set DARKROOM_HOME or create ~/.darkroom with Dark Room"))?;
    let resolved = vault::resolve_entry(&dir, &req.handle, &origins).map_err(|e| {
        let code = match e {
            vault::VaultError::OriginDenied | vault::VaultError::NoOrigins => ErrorCode::PolicyDenied,
            _ => ErrorCode::InvalidArgs,
        };
        refused(code, &format!("Dark Room refused the handle ({})", e.as_str()), "Ask the user to mint or re-scope the handle in Dark Room")
    })?;
    if !secure && !resolved.fill_visible {
        drop(resolved);
        audit::append("fill_refused_visible_field", &[("handle", &req.handle), ("via", "agent-desktop")]);
        return Err(refused(
            ErrorCode::PolicyDenied,
            "That field shows its text, so anything could read the secret back; only password fields are filled",
            "Use the password field, or ask the user to allow this handle in visible fields in Dark Room",
        ));
    }
    let mut value = resolved.plain.as_str().to_string();
    drop(resolved);
    let result = if req.use_type {
        type_command::execute(
            type_command::TypeArgs { ref_id: req.ref_id.clone(), text: value.clone(), timeout_ms: req.timeout_ms, interval_ms: None },
            adapter,
            context,
        )
    } else {
        set_value_command::execute(
            set_value_command::SetValueArgs { ref_id: req.ref_id.clone(), value: value.clone(), timeout_ms: req.timeout_ms },
            adapter,
            context,
        )
    };
    unsafe { value.as_mut_vec().iter_mut().for_each(|b| *b = 0) };
    drop(value);
    let origin = origins.first().cloned().unwrap_or_default();
    match result {
        Ok(_) => {
            audit::append("fill_ok", &[("handle", &req.handle), ("origin", &origin), ("via", "agent-desktop")]);
            Ok(json!({
                "filled": true,
                "ref": req.ref_id,
                "handle": req.handle,
                "origin": origin,
                "method": if req.use_type { "type" } else { "set-value" },
            }))
        }
        Err(e) => {
            audit::append("fill_failed", &[("handle", &req.handle), ("origin", &origin), ("via", "agent-desktop")]);
            Err(scrub(e))
        }
    }
}
