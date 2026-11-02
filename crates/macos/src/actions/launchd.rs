//! Install and tear down launchd agents/daemons.
//!
//! Generates a `.plist` from a `LaunchdEntry`, writes it to
//! `~/Library/LaunchAgents/<label>.plist` (agent) or `/Library/LaunchDaemons/<label>.plist`
//! (daemon), and calls `launchctl bootstrap` to load it. On uninstall,
//! `launchctl bootout` + `rm` reverses this.
//!
//! launchd refuses to bootstrap a label that is already loaded, so when an
//! install finds the job loaded, [`unload_loaded_job`] boots it out before the
//! files are copied, and the job is bootstrapped again from the new plist. A
//! failed install puts the old plist back and bootstraps the old job.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use crate::config::{LaunchdEntry, LaunchdOnInstall, LaunchdScope};
use crate::manifest::Action;

/// How long a booted-out job may take to exit. launchd kills it after its
/// `ExitTimeOut`, 20 seconds unless its plist sets another.
const EXIT_TIMEOUT: Duration = Duration::from_secs(60);

/// Boot out the job `entry` declares if it is loaded, so its files can be
/// replaced, and record that, so the launchd phase and a failed install's
/// rollback bootstrap it again.
pub fn unload_loaded_job(
    entry: &LaunchdEntry,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let scope = scope_str(&entry.scope);
    let target = domain_target(scope);
    if !is_loaded(&target, &entry.label) {
        return Ok(());
    }
    let plist_path = plist_dir_for(&entry.scope)?.join(format!("{}.plist", entry.label));

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "launchd: {target}/{} is loaded; booting it out so its files can be replaced",
            entry.label
        ),
    );
    manifest.record(Action::LaunchdServiceUnloaded {
        label: entry.label.clone(),
        scope: scope.to_string(),
        plist_path,
    });
    bootout(&target, &entry.label, callbacks)
}

pub fn install_launchd_entry(
    entry: &LaunchdEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let plist_dir = plist_dir_for(&entry.scope)?;
    std::fs::create_dir_all(&plist_dir).map_err(|e| InstallerError::DirOp {
        path: plist_dir.clone(),
        source: e,
    })?;
    let plist_path = plist_dir.join(format!("{}.plist", entry.label));
    let scope = scope_str(&entry.scope);

    let program = resolver.resolve(&entry.program)?;
    let resolved_args: Vec<String> = entry
        .program_arguments
        .iter()
        .map(|a| resolver.resolve(a))
        .collect::<InstallerResult<_>>()?;

    let working_dir = entry
        .working_directory
        .as_deref()
        .map(|s| resolver.resolve(s))
        .transpose()?;
    let stdout = entry
        .standard_out_path
        .as_deref()
        .map(|s| resolver.resolve(s))
        .transpose()?;
    let stderr = entry
        .standard_error_path
        .as_deref()
        .map(|s| resolver.resolve(s))
        .transpose()?;

    let plist = build_plist(
        &entry.label,
        &program,
        &resolved_args,
        entry.run_at_load,
        entry.keep_alive,
        entry.start_interval,
        entry.exit_timeout,
        entry.user_name.as_deref(),
        entry.group_name.as_deref(),
        working_dir.as_deref(),
        stdout.as_deref(),
        stderr.as_deref(),
    );

    let backup = if plist_path.exists() {
        let backup = backup_path(&plist_path);
        std::fs::copy(&plist_path, &backup).map_err(|e| InstallerError::FileOp {
            path: plist_path.clone(),
            source: e,
        })?;
        Some(backup)
    } else {
        None
    };

    callbacks.on_log(
        LogLevel::Info,
        &format!("launchd: writing {}", plist_path.display()),
    );

    manifest.record(Action::LaunchdPlistInstalled {
        label: entry.label.clone(),
        plist_path: plist_path.clone(),
        scope: scope.to_string(),
        backup,
    });

    plist
        .to_file_xml(&plist_path)
        .map_err(|e| InstallerError::Other(format!("launchd: plist write failed: {e}")))?;

    let unloaded_for_install = manifest.actions.iter().any(
        |a| matches!(a, Action::LaunchdServiceUnloaded { label, .. } if *label == entry.label),
    );
    if matches!(entry.on_install, LaunchdOnInstall::Load) || unloaded_for_install {
        manifest.record(Action::LaunchdServiceLoaded {
            label: entry.label.clone(),
            scope: scope.to_string(),
        });
        bootstrap(&entry.label, &plist_path, &domain_target(scope), callbacks)?;
    }

    Ok(())
}

/// Remove the plist an install wrote. A failed install puts back the one it
/// replaced; uninstall removes that copy too.
pub fn rollback_plist_installed(
    plist_path: &Path,
    backup: Option<&Path>,
    restore_backups: bool,
) -> InstallerResult<()> {
    if plist_path.exists() {
        std::fs::remove_file(plist_path).map_err(|e| InstallerError::FileOp {
            path: plist_path.to_path_buf(),
            source: e,
        })?;
    }
    if let Some(backup) = backup.filter(|b| b.exists()) {
        let result = if restore_backups {
            std::fs::rename(backup, plist_path)
        } else {
            std::fs::remove_file(backup)
        };
        result.map_err(|e| InstallerError::FileOp {
            path: plist_path.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

pub fn rollback_service_loaded(
    label: &str,
    scope: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    bootout(&domain_target(scope), label, callbacks)
}

/// Bootstrap again the job an install booted out. Reached after the old
/// plist and files are back, so the old job runs.
pub fn rollback_service_unloaded(
    label: &str,
    scope: &str,
    plist_path: &Path,
    restore_backups: bool,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let target = domain_target(scope);
    if !restore_backups || is_loaded(&target, label) {
        return Ok(());
    }
    bootstrap(label, plist_path, &target, callbacks)
}

// --- Internal helpers ---

fn plist_dir_for(scope: &LaunchdScope) -> InstallerResult<PathBuf> {
    match scope {
        LaunchdScope::Agent => {
            let home = std::env::var_os("HOME").ok_or_else(|| {
                InstallerError::Other("HOME not set; can't locate ~/Library/LaunchAgents".into())
            })?;
            Ok(PathBuf::from(home).join("Library/LaunchAgents"))
        }
        LaunchdScope::Daemon => Ok(PathBuf::from("/Library/LaunchDaemons")),
    }
}

fn scope_str(scope: &LaunchdScope) -> &'static str {
    match scope {
        LaunchdScope::Agent => "agent",
        LaunchdScope::Daemon => "daemon",
    }
}

fn backup_path(plist_path: &Path) -> PathBuf {
    let mut name = plist_path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

fn bootstrap(
    label: &str,
    plist_path: &Path,
    target: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "launchd: bootstrap {target}/{label} from {}",
            plist_path.display()
        ),
    );
    let status = Command::new("launchctl")
        .args(["bootstrap", target])
        .arg(plist_path)
        .status()
        .map_err(|e| InstallerError::Other(format!("launchctl bootstrap failed to launch: {e}")))?;
    if !status.success() {
        return Err(InstallerError::Other(format!(
            "launchctl bootstrap exited with {status}"
        )));
    }
    Ok(())
}

/// Boot the job out and wait for it to be gone. `launchctl bootout` returns
/// while the job is still exiting, and until it has, launchd refuses to
/// bootstrap the label again.
fn bootout(target: &str, label: &str, callbacks: &dyn InstallerCallbacks) -> InstallerResult<()> {
    callbacks.on_log(
        LogLevel::Info,
        &format!("launchd: bootout {target}/{label}"),
    );
    let status = Command::new("launchctl")
        .args(["bootout", &format!("{target}/{label}")])
        .status();
    match status {
        Ok(s) if s.success() => {}
        // "No such process" / already-unloaded is idempotent — not a real failure.
        Ok(s) if matches!(s.code(), Some(3) | Some(77) | Some(113)) => {}
        Ok(s) => {
            return Err(InstallerError::Other(format!(
                "launchctl bootout exited with {s}"
            )));
        }
        Err(e) => {
            return Err(InstallerError::Other(format!(
                "launchctl bootout failed to launch: {e}"
            )));
        }
    }

    let deadline = Instant::now() + EXIT_TIMEOUT;
    while is_loaded(target, label) {
        if Instant::now() >= deadline {
            return Err(InstallerError::Other(format!(
                "launchd: {target}/{label} did not exit within {} seconds",
                EXIT_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn is_loaded(target: &str, label: &str) -> bool {
    Command::new("launchctl")
        .args(["print", &format!("{target}/{label}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn domain_target(scope: &str) -> String {
    match scope {
        "daemon" => "system".to_string(),
        _ => format!("gui/{}", unsafe { libc::getuid() }),
    }
}

#[allow(clippy::too_many_arguments)]
fn build_plist(
    label: &str,
    program: &str,
    program_arguments: &[String],
    run_at_load: bool,
    keep_alive: bool,
    start_interval: Option<u64>,
    exit_timeout: Option<u64>,
    user_name: Option<&str>,
    group_name: Option<&str>,
    working_directory: Option<&str>,
    standard_out_path: Option<&str>,
    standard_error_path: Option<&str>,
) -> plist::Value {
    let mut dict = plist::Dictionary::new();
    dict.insert("Label".to_string(), plist::Value::String(label.to_string()));

    // ProgramArguments is conventional: always includes the program as argv[0].
    let mut args: Vec<plist::Value> = Vec::with_capacity(1 + program_arguments.len());
    args.push(plist::Value::String(program.to_string()));
    for a in program_arguments {
        args.push(plist::Value::String(a.clone()));
    }
    dict.insert("ProgramArguments".to_string(), plist::Value::Array(args));

    if run_at_load {
        dict.insert("RunAtLoad".to_string(), plist::Value::Boolean(true));
    }
    if keep_alive {
        dict.insert("KeepAlive".to_string(), plist::Value::Boolean(true));
    }
    if let Some(i) = start_interval {
        dict.insert(
            "StartInterval".to_string(),
            plist::Value::Integer((i as i64).into()),
        );
    }
    if let Some(t) = exit_timeout {
        dict.insert(
            "ExitTimeOut".to_string(),
            plist::Value::Integer((t as i64).into()),
        );
    }
    if let Some(u) = user_name {
        dict.insert("UserName".to_string(), plist::Value::String(u.to_string()));
    }
    if let Some(g) = group_name {
        dict.insert("GroupName".to_string(), plist::Value::String(g.to_string()));
    }
    if let Some(wd) = working_directory {
        dict.insert(
            "WorkingDirectory".to_string(),
            plist::Value::String(wd.to_string()),
        );
    }
    if let Some(p) = standard_out_path {
        dict.insert(
            "StandardOutPath".to_string(),
            plist::Value::String(p.to_string()),
        );
    }
    if let Some(p) = standard_error_path {
        dict.insert(
            "StandardErrorPath".to_string(),
            plist::Value::String(p.to_string()),
        );
    }

    plist::Value::Dictionary(dict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_plist_minimal_agent() {
        let p = build_plist(
            "no.divvun.test",
            "/Applications/MyApp.app/Contents/MacOS/myapp",
            &[],
            true,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        let d = p.as_dictionary().unwrap();
        assert_eq!(d.get("Label").unwrap().as_string(), Some("no.divvun.test"));
        assert!(d.get("ExitTimeOut").is_none());
        assert_eq!(d.get("RunAtLoad").unwrap().as_boolean(), Some(true));
        let args = d.get("ProgramArguments").unwrap().as_array().unwrap();
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn test_build_plist_full_daemon() {
        let p = build_plist(
            "no.divvun.daemon",
            "/usr/local/bin/myd",
            &["--foreground".to_string(), "--verbose".to_string()],
            true,
            true,
            Some(3600),
            Some(60),
            Some("_myapp"),
            Some("_myapp"),
            Some("/var/lib/myapp"),
            Some("/var/log/myapp.out"),
            Some("/var/log/myapp.err"),
        );
        let d = p.as_dictionary().unwrap();
        assert_eq!(d.get("KeepAlive").unwrap().as_boolean(), Some(true));
        assert_eq!(
            d.get("StartInterval").unwrap().as_signed_integer(),
            Some(3600)
        );
        assert_eq!(d.get("ExitTimeOut").unwrap().as_signed_integer(), Some(60));
        assert_eq!(d.get("UserName").unwrap().as_string(), Some("_myapp"));
        let args = d.get("ProgramArguments").unwrap().as_array().unwrap();
        assert_eq!(args.len(), 3);
        assert_eq!(args[0].as_string(), Some("/usr/local/bin/myd"));
        assert_eq!(args[1].as_string(), Some("--foreground"));
    }

    #[test]
    fn test_plist_xml_serialization_roundtrip() {
        let dir = std::env::temp_dir().join(format!("outto-lpr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.plist");

        let p = build_plist(
            "no.divvun.rt",
            "/bin/echo",
            &["hi".to_string()],
            true,
            false,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        );
        p.to_file_xml(&path).unwrap();

        let reloaded = plist::Value::from_file(&path).unwrap();
        let d = reloaded.as_dictionary().unwrap();
        assert_eq!(d.get("Label").unwrap().as_string(), Some("no.divvun.rt"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
