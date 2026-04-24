//! Windows-specific environment/shell-folder variables for the `VariableResolver`.
//!
//! Adds `#{pf}`, `#{pf32}`, `#{win}`, `#{sys}`, `#{userappdata}`, `#{localappdata}`,
//! `#{commonappdata}`, `#{tmp}`, `#{desktop}`, `#{startmenu}`, `#{startup}`.

use std::path::PathBuf;

use outto_core::config::VariableResolver;

/// Populate Windows shell-folder variables onto the given resolver.
///
/// Call this after `VariableResolver::new()` to get all the Windows-specific
/// variables that the existing `outto.toml` schema uses.
pub fn with_windows_env(mut resolver: VariableResolver) -> VariableResolver {
    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    if let Some(pf) = env("ProgramFiles") {
        resolver.set_variable("pf", pf);
    }
    if let Some(pf86) = env("ProgramFiles(x86)") {
        resolver.set_variable("pf32", pf86);
    }

    if let Some(sys) = env("SystemRoot") {
        let sys_path = PathBuf::from(&sys);
        resolver.set_variable("win", sys.clone());
        resolver.set_variable("sys", sys_path.join("System32").to_string_lossy());
    }

    if let Some(appdata) = env("APPDATA") {
        resolver.set_variable("userappdata", appdata);
    }
    if let Some(localappdata) = env("LOCALAPPDATA") {
        resolver.set_variable("localappdata", localappdata);
    }
    if let Some(programdata) = env("ProgramData") {
        resolver.set_variable("commonappdata", programdata);
    }
    if let Some(temp) = env("TEMP") {
        resolver.set_variable("tmp", temp);
    }

    if let Some(userprofile) = env("USERPROFILE") {
        let up = PathBuf::from(&userprofile);
        resolver.set_variable("desktop", up.join("Desktop").to_string_lossy());
        resolver.set_variable(
            "startmenu",
            up.join("AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs")
                .to_string_lossy(),
        );
        resolver.set_variable(
            "startup",
            up.join("AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup")
                .to_string_lossy(),
        );
    }

    resolver
}
