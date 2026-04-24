//! Privilege detection and `osascript`-based self-elevation.
//!
//! macOS doesn't have an in-process "please elevate me" API (`AuthorizationExecuteWithPrivileges`
//! is deprecated; `SMJobBless` requires a pre-shipped helper tool). The pragmatic
//! option is `osascript -e 'do shell script ... with administrator privileges'`,
//! which shows the standard macOS password prompt and relaunches the installer
//! as root.

use std::ffi::OsString;
use std::path::Path;

use outto_core::error::{InstallerError, InstallerResult};

use crate::config::RequiredPrivileges;

/// True if the current process is running as root.
pub fn is_root() -> bool {
    // SAFETY: `geteuid` has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Decide whether an install needs elevation given the TOML `required` setting
/// and the resolved install directory. `system_roots` is a list of paths that
/// require root to write into; any install path under one of them forces
/// elevation.
pub fn needs_elevation(
    required: &RequiredPrivileges,
    install_dir: &Path,
    system_roots: &[&str],
) -> bool {
    if is_root() {
        return false;
    }
    match required {
        RequiredPrivileges::Admin => true,
        RequiredPrivileges::User => false,
        RequiredPrivileges::Auto => system_roots
            .iter()
            .any(|root| install_dir.starts_with(root)),
    }
}

/// Default set of paths that need root to modify.
pub const DEFAULT_SYSTEM_ROOTS: &[&str] = &[
    "/Library",
    "/usr/local",
    "/Library/LaunchDaemons",
    "/Library/LaunchAgents",
    "/Applications", // technically admin on single-user macs, but writable; treat as user by default
    "/System",
    "/private",
];

/// Relaunch the current process with admin rights, passing the same argv.
/// Returns `Err(ElevationRequired)` on failure; on success, the current process
/// is replaced so this call typically doesn't return.
pub fn elevate_self(extra_args: &[String]) -> InstallerResult<()> {
    let exe = std::env::current_exe()
        .map_err(|e| InstallerError::Other(format!("can't locate current exe: {e}")))?;
    let exe_str = exe.to_string_lossy().into_owned();

    let mut argv: Vec<OsString> = std::env::args_os().skip(1).collect();
    for a in extra_args {
        argv.push(OsString::from(a));
    }

    // Compose a POSIX shell command:  "<exe>" "<arg1>" "<arg2>" ...
    // Quote each piece with single quotes and escape embedded single quotes as '\'' .
    let mut shell_cmd = quote_posix(&exe_str);
    for a in &argv {
        shell_cmd.push(' ');
        shell_cmd.push_str(&quote_posix(&a.to_string_lossy()));
    }

    // AppleScript requires embedded double-quotes/backslashes to be escaped.
    let script = format!(
        "do shell script \"{}\" with administrator privileges",
        escape_applescript(&shell_cmd)
    );

    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .status()
        .map_err(|e| InstallerError::Other(format!("osascript failed to launch: {e}")))?;

    if !status.success() {
        return Err(InstallerError::ElevationRequired(format!(
            "osascript exited with {status} (user may have cancelled the password prompt)"
        )));
    }

    // The elevated child has run to completion; exit so we don't double-install.
    std::process::exit(0);
}

fn quote_posix(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

fn escape_applescript(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_root_returns_bool() {
        let _b: bool = is_root();
    }

    #[test]
    fn test_needs_elevation_user_scope() {
        let p = Path::new("/Users/test/Applications/MyApp.app");
        assert!(!needs_elevation(
            &RequiredPrivileges::User,
            p,
            DEFAULT_SYSTEM_ROOTS
        ));
    }

    #[test]
    fn test_needs_elevation_auto_system_scope() {
        let p = Path::new("/Library/LaunchDaemons");
        let expected = !is_root(); // true unless already root
        assert_eq!(
            needs_elevation(&RequiredPrivileges::Auto, p, DEFAULT_SYSTEM_ROOTS),
            expected
        );
    }

    #[test]
    fn test_needs_elevation_admin_always_unless_root() {
        let p = Path::new("/tmp");
        let expected = !is_root();
        assert_eq!(
            needs_elevation(&RequiredPrivileges::Admin, p, DEFAULT_SYSTEM_ROOTS),
            expected
        );
    }

    #[test]
    fn test_quote_posix_escapes_single_quotes() {
        assert_eq!(quote_posix("foo"), "'foo'");
        assert_eq!(quote_posix("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_escape_applescript() {
        assert_eq!(escape_applescript("hello"), "hello");
        assert_eq!(escape_applescript("he \"said\""), "he \\\"said\\\"");
        assert_eq!(escape_applescript("a\\b"), "a\\\\b");
    }
}
