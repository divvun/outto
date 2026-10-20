//! Windows-specific environment/shell-folder variables for the `VariableResolver`.
//!
//! Adds `#{pf}`, `#{pf32}`, `#{win}`, `#{sys}`, `#{userappdata}`, `#{localappdata}`,
//! `#{commonappdata}`, `#{tmp}`, the current user's `#{startmenu}`,
//! `#{desktop}` and `#{startup}`, and all users' `#{commonstartmenu}`,
//! `#{commondesktop}` and `#{commonstartup}`.

use std::path::PathBuf;

use windows_sys::Win32::UI::Shell::{
    FOLDERID_CommonPrograms, FOLDERID_CommonStartup, FOLDERID_Desktop, FOLDERID_Programs,
    FOLDERID_PublicDesktop, FOLDERID_Startup, KF_FLAG_DONT_VERIFY, SHGetKnownFolderPath,
};
use windows_sys::core::GUID;

use outto_core::config::VariableResolver;

/// The shell folders shortcuts go in, as Windows reports them, so that a
/// redirected or moved folder (a desktop in OneDrive) is where it really is.
/// The current user's are those of the account the installer runs as: for
/// an install elevated with another administrator's credentials, that
/// administrator's, as with Inno Setup's `{user...}` folders.
const SHELL_FOLDERS: [(&str, GUID); 6] = [
    ("startmenu", FOLDERID_Programs),
    ("desktop", FOLDERID_Desktop),
    ("startup", FOLDERID_Startup),
    ("commonstartmenu", FOLDERID_CommonPrograms),
    ("commondesktop", FOLDERID_PublicDesktop),
    ("commonstartup", FOLDERID_CommonStartup),
];

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

    for (name, id) in &SHELL_FOLDERS {
        if let Some(path) = known_folder(id) {
            resolver.set_variable(*name, path);
        }
    }

    resolver
}

/// `SHGetKnownFolderPath` for the process token's user, without requiring
/// the folder to exist (the shortcut action creates it).
fn known_folder(id: &GUID) -> Option<String> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;

    let mut path: windows_sys::core::PWSTR = std::ptr::null_mut();
    unsafe {
        let hr = SHGetKnownFolderPath(
            id,
            KF_FLAG_DONT_VERIFY as u32,
            std::ptr::null_mut(),
            &mut path,
        );
        let result = (hr >= 0 && !path.is_null()).then(|| {
            let len = (0..).take_while(|&i| *path.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(path, len))
        });
        // The buffer is the caller's to free whether or not the call succeeded.
        CoTaskMemFree(path as *const std::ffi::c_void);
        result.filter(|p| !p.is_empty())
    }
}
