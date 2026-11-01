//! Exclusion fence: a user-owned deny list (`fence.json`) on top of a
//! built-in floor, enforced inside the tool before any read, action,
//! capture or launch reaches the platform adapter.

pub(crate) mod adapter;
pub(crate) mod audit;
/// Local notes, screened by the fence before every write.
#[path = "../jetpack/mod.rs"]
pub(crate) mod jetpack;
pub(crate) mod policy;
pub(crate) mod probe;
#[cfg(target_os = "windows")]
pub(crate) mod occlusion_windows;
#[cfg(target_os = "windows")]
pub(crate) mod probe_windows;
pub(crate) mod redact;

use agent_desktop_core::{AdapterError, DeliverySemantics, ErrorCode};
use std::path::PathBuf;

use policy::{Fence, FenceFile};
use probe::Probe;

/// One fence file to read. `required` files must exist and parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FenceSource {
    pub path: PathBuf,
    pub required: bool,
}

/// The user's own fence file is always read. `AGENT_DESKTOP_FENCE` can only
/// add a second file; it can't replace or switch off the first, and if it is
/// set the file it names must exist, so a typo or a missing file fails closed.
pub(crate) fn fence_sources_from(env: Option<std::ffi::OsString>, home: Option<PathBuf>) -> Vec<FenceSource> {
    let mut out = Vec::new();
    if let Some(h) = home {
        out.push(FenceSource { path: h.join(".agent-desktop").join("fence.json"), required: false });
    }
    if let Some(p) = env.filter(|v| !v.is_empty()) {
        let path = PathBuf::from(p);
        if !out.iter().any(|s| s.path == path) {
            out.push(FenceSource { path, required: true });
        }
    }
    out
}

pub(crate) fn fence_sources() -> Vec<FenceSource> {
    fence_sources_from(std::env::var_os("AGENT_DESKTOP_FENCE"), audit::home_dir())
}

/// `%NAME%` lookup for path checks. Profile folders come from the operating
/// system, not the agent's environment, so a changed `USERPROFILE` can't
/// steer a check away from the real folder.
pub(crate) fn env_lookup(name: &str) -> Option<String> {
    let upper = name.to_ascii_uppercase();
    if upper == "USERPROFILE" || upper == "HOME" {
        if let Some(h) = audit::home_dir().and_then(|h| h.to_str().map(str::to_string)) {
            return Some(h);
        }
    }
    std::env::var(name).ok()
}

pub(crate) fn platform_probe() -> Box<dyn Probe> {
    #[cfg(target_os = "windows")]
    {
        Box::new(probe_windows::WinProbe::new())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Box::new(probe::NullProbe)
    }
}

/// Floor plus every fence file. A file that exists but does not parse, or a
/// required file that is missing, fails closed: every command is refused.
pub(crate) fn load(probe: &dyn Probe) -> Result<Fence, AdapterError> {
    let Some(home) = audit::home_dir() else {
        // Without the profile folder the user's own fence file can't be found.
        return Err(invalid("The user's profile folder can't be found, so the fence can't be read and every action is refused"));
    };
    load_from(probe, &fence_sources(), &audit::darkroom_dirs(), Some(home))
}

fn invalid(message: &str) -> AdapterError {
    AdapterError::new(ErrorCode::Fenced, message.to_string())
        .with_suggestion("Ask the user to fix fence.json; the agent must not edit it")
        .with_disposition(DeliverySemantics::not_delivered())
}

pub(crate) fn load_from(probe: &dyn Probe, sources: &[FenceSource], darkroom_dirs: &[PathBuf], home: Option<PathBuf>) -> Result<Fence, AdapterError> {
    let mut merged = FenceFile::default();
    for source in sources {
        let file = match std::fs::read_to_string(&source.path) {
            Ok(text) => serde_json::from_str::<FenceFile>(&text).map_err(|e| {
                audit::append("fence_file_invalid", &[("error", &e.to_string())]);
                invalid("The fence file is not valid, so every action is refused")
            })?,
            Err(_) if !source.required => FenceFile::default(),
            Err(_) => {
                audit::append("fence_file_missing", &[("source", "AGENT_DESKTOP_FENCE")]);
                return Err(invalid("AGENT_DESKTOP_FENCE names a fence file that can't be read, so every action is refused"));
            }
        };
        if file.version > 1 {
            return Err(invalid("The fence file uses a newer format than this build understands"));
        }
        merged.programs.process_names.extend(file.programs.process_names);
        merged.programs.exe_paths.extend(file.programs.exe_paths);
        merged.programs.window_classes.extend(file.programs.window_classes);
        merged.programs.title_patterns.extend(file.programs.title_patterns);
        merged.paths.extend(file.paths);
    }
    let mut protected: Vec<String> = Vec::new();
    for source in sources {
        if let Some(p) = source.path.to_str() {
            protected.push(p.to_string());
        }
    }
    // The folder that holds the user's fence file, so Explorer and file
    // dialogs can't be used to rename, replace or delete it.
    if let Some(dir) = home.as_ref().map(|h| h.join(".agent-desktop")).and_then(|d| d.to_str().map(str::to_string)) {
        protected.push(dir);
    }
    for d in darkroom_dirs {
        if let Some(d) = d.to_str() {
            protected.push(d.to_string());
        }
    }
    let mut fence = Fence::new(&merged, &protected);
    let resolved_roots: Vec<String> = fence.roots().iter().map(|r| probe.resolve_path(r)).collect();
    for r in resolved_roots {
        fence.add_resolved_root(&r);
    }
    let resolved_exes: Vec<String> = fence.exe_paths().iter().map(|r| probe.resolve_path(r)).collect();
    for r in resolved_exes {
        fence.add_resolved_exe_path(&r);
    }
    Ok(fence)
}

#[cfg(test)]
#[path = "attack_tests.rs"]
mod attack_tests;
