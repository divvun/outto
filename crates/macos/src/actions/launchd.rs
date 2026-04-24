//! Install and tear down launchd agents/daemons.
//!
//! Generates a `.plist` from a `LaunchdEntry`, writes it to
//! `~/Library/LaunchAgents/<label>.plist` (agent) or `/Library/LaunchDaemons/<label>.plist`
//! (daemon), and calls `launchctl bootstrap` to load it. On uninstall,
//! `launchctl bootout` + `rm` reverses this.

use std::path::{Path, PathBuf};
use std::process::Command;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use crate::config::{LaunchdEntry, LaunchdOnInstall, LaunchdScope};
use crate::manifest::Action;

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
        entry.user_name.as_deref(),
        entry.group_name.as_deref(),
        working_dir.as_deref(),
        stdout.as_deref(),
        stderr.as_deref(),
    );

    callbacks.on_log(
        LogLevel::Info,
        &format!("launchd: writing {}", plist_path.display()),
    );

    plist
        .to_file_xml(&plist_path)
        .map_err(|e| InstallerError::Other(format!("launchd: plist write failed: {e}")))?;

    manifest.record(Action::LaunchdPlistInstalled {
        label: entry.label.clone(),
        plist_path: plist_path.clone(),
        scope: scope_str(&entry.scope).to_string(),
    });

    if matches!(entry.on_install, LaunchdOnInstall::Load) {
        bootstrap(&entry.label, &plist_path, &entry.scope, callbacks)?;
        manifest.record(Action::LaunchdServiceLoaded {
            label: entry.label.clone(),
            scope: scope_str(&entry.scope).to_string(),
        });
    }

    Ok(())
}

pub fn rollback_plist_installed(
    _label: &str,
    plist_path: &Path,
    _scope: &str,
    _callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    if plist_path.exists() {
        std::fs::remove_file(plist_path).map_err(|e| InstallerError::FileOp {
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
    let target = bootout_target(scope);
    callbacks.on_log(
        LogLevel::Info,
        &format!("launchd: bootout {target}/{label}"),
    );
    let status = Command::new("launchctl")
        .args(["bootout", &format!("{target}/{label}")])
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => {
            // "No such process" / already-unloaded is idempotent — not a real failure.
            if matches!(s.code(), Some(3) | Some(77) | Some(113)) {
                Ok(())
            } else {
                Err(InstallerError::Other(format!(
                    "launchctl bootout exited with {s}"
                )))
            }
        }
        Err(e) => Err(InstallerError::Other(format!(
            "launchctl bootout failed to launch: {e}"
        ))),
    }
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

fn bootstrap(
    label: &str,
    plist_path: &Path,
    scope: &LaunchdScope,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let target = match scope {
        LaunchdScope::Agent => format!("gui/{}", unsafe { libc::getuid() }),
        LaunchdScope::Daemon => "system".to_string(),
    };
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "launchd: bootstrap {target}/{label} from {}",
            plist_path.display()
        ),
    );
    let status = Command::new("launchctl")
        .args(["bootstrap", &target])
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

fn bootout_target(scope: &str) -> String {
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
        );
        let d = p.as_dictionary().unwrap();
        assert_eq!(d.get("Label").unwrap().as_string(), Some("no.divvun.test"));
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
        );
        p.to_file_xml(&path).unwrap();

        let reloaded = plist::Value::from_file(&path).unwrap();
        let d = reloaded.as_dictionary().unwrap();
        assert_eq!(d.get("Label").unwrap().as_string(), Some("no.divvun.rt"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
