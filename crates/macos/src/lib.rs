//! macOS backend for the outto installer framework.
//!
//! Install primitives: copy `.app` bundles via `ditto`, write plist values,
//! install launchd agents/daemons, create symlinks, place fonts, register with
//! LaunchServices, run user/admin commands. Receipts (the "Add/Remove Programs"
//! equivalent) live at `~/Library/no.divvun.install/packages/<pkg-id>/` for
//! user-scope installs and `/Library/no.divvun.install/packages/<pkg-id>/` for
//! system-scope.
//!
//! Elevation (when install paths or TOML `[privileges]` require root) is done
//! by re-running the installer as a one-shot root launchd job authorized via
//! Authorization Services (see [`elevation`]). Notarization and `.app` bundle
//! construction happen in the build pipeline (`outto-cli`), not at install time.

#![cfg(target_os = "macos")]

pub mod actions;
pub mod config;
pub mod detect;
pub mod elevation;
pub mod macho;
pub mod manifest;
pub mod paths;
pub mod uninstall;

#[cfg(test)]
pub(crate) mod test_util;

use std::path::PathBuf;

use outto_core::callbacks::{InstallOptions, InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{CoreAction, InstallManifest, rollback::rollback_actions};

pub use config::Config;
pub use manifest::Action as MacosAction;
pub use uninstall::uninstall as uninstall_package_by_id;

/// Build a fully-configured resolver for macOS: package metadata + install
/// directory + the macOS path-variable table.
pub fn make_resolver(config: &Config, install_dir: Option<&std::path::Path>) -> VariableResolver {
    paths::make_resolver(&config.package.name, &config.package.version, install_dir)
}

/// macOS install entry point.
///
/// Pipeline: arch + macOS-version check → elevation decision → existing-install
/// detection / upgrade handling → prerequisites → create install dir → execute
/// actions → save receipt + manifest → on failure, rollback.
pub fn install(
    config: &Config,
    options: &InstallOptions,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Starting installation: {} v{}",
            config.package.name, config.package.version
        ),
    );

    // Enforce [package] min_macos_version before touching anything.
    if let Some(ref min) = config.package.min_macos_version {
        if !actions::macos_version_at_least(min)? {
            return Err(InstallerError::Validation(format!(
                "{} requires macOS {min} or later",
                config.package.name
            )));
        }
    }

    // Resolve install dir (from options, else from default_dir in config).
    let install_dir = if let Some(ref dir) = options.install_dir {
        dir.clone()
    } else if let Some(ref default_dir) = config.package.default_dir {
        // Build a resolver without `app` to expand the default_dir expression.
        make_resolver(config, None).resolve_path(default_dir)?
    } else {
        return Err(InstallerError::Config(
            "no install directory specified (set install_dir in options or default_dir in config)"
                .into(),
        ));
    };

    // Decide scope by inspecting the install path.
    let scope = classify_scope(&install_dir);

    // If we need admin rights and aren't root, re-run this command line as a
    // root launchd job and report its result as ours.
    if elevation::needs_elevation(
        &config.privileges.required,
        &install_dir,
        elevation::DEFAULT_SYSTEM_ROOTS,
    ) {
        if !config.privileges.auto_elevate {
            return Err(InstallerError::ElevationRequired(
                "this install requires admin privileges; run with sudo or set [privileges] auto_elevate = true"
                    .into(),
            ));
        }
        callbacks.on_log(LogLevel::Info, "Requesting administrator authorization...");
        let prompt = elevation::AuthPrompt::install(&config.package.id, &config.package.name);
        return elevation::elevate_self(&prompt, callbacks);
    }

    let resolver = make_resolver(config, Some(&install_dir));
    let uninstall_hooks = actions::uninstall_hooks(config, &resolver)?;

    // Check existing install → honour upgrade policy.
    let mut old_manifest: Option<InstallManifest<MacosAction>> = None;
    if let Some(existing) = detect::detect_existing_install(&config.package.id)? {
        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Existing installation found: {} v{} at {}",
                existing.display_name.as_deref().unwrap_or("unknown"),
                existing.version.as_deref().unwrap_or("unknown"),
                existing.install_dir.display()
            ),
        );

        match config.upgrade.policy {
            config::UpgradePolicy::Fail => {
                return Err(InstallerError::UpgradeConflict(format!(
                    "{} is already installed",
                    config.package.name
                )));
            }
            config::UpgradePolicy::SideBySide => {}
            config::UpgradePolicy::Overwrite => {
                let base = receipt_base_for(&existing.scope);
                old_manifest =
                    InstallManifest::<MacosAction>::load_from_base(&base, &config.package.id).ok();
            }
        }
    }

    actions::check_prerequisites(config, callbacks)?;

    let mut install_manifest = InstallManifest::<MacosAction>::new(
        &config.package.id,
        &config.package.name,
        &config.package.version,
        &install_dir,
        config.package.depends_on.clone(),
    );
    // The install dir is the package's own even when it already existed
    // (an upgrade), so it is always recorded; uninstall removes it once empty.
    if !outto_core::actions::dirs::create_dir_all_recorded(&install_dir, &mut install_manifest)? {
        install_manifest.record(CoreAction::DirectoryCreated {
            path: install_dir.clone(),
        });
    }
    install_manifest.uninstall_hooks = Some(uninstall_hooks);

    let result = actions::execute_install(
        config,
        &options.source_dir,
        &options.selected_components,
        &resolver,
        &mut install_manifest,
        callbacks,
    );

    match result {
        Ok(()) => {
            let base = receipt_base_for(&scope);
            std::fs::create_dir_all(&base).map_err(|e| InstallerError::DirOp {
                path: base.clone(),
                source: e,
            })?;

            if let Some(old) = &old_manifest {
                inherit_from_previous(&mut install_manifest, old);
            }
            let backups = take_backups(&mut install_manifest);
            install_manifest.save_to(&base)?;
            for backup in &backups {
                remove_backup(backup, callbacks);
            }

            // Write the lightweight receipt.json (display_name/version/install_dir/depends_on).
            detect::write_receipt(
                &base,
                &detect::Receipt {
                    package_id: config.package.id.clone(),
                    display_name: config.package.name.clone(),
                    version: config.package.version.clone(),
                    install_dir: install_dir.clone(),
                    depends_on: config.package.depends_on.clone(),
                    scope: scope.clone(),
                },
            )?;

            // Copy the pre-built uninstall.app into the receipt directory, if provided.
            if let Some(ref uninstall_app) = options.uninstall_exe {
                let dest = base.join(&config.package.id).join("uninstall.app");
                if dest.exists() {
                    let _ = std::fs::remove_dir_all(&dest);
                }
                let status = std::process::Command::new("ditto")
                    .arg(uninstall_app)
                    .arg(&dest)
                    .status();
                match status {
                    Ok(s) if s.success() => {
                        callbacks.on_log(
                            LogLevel::Info,
                            &format!("Copied uninstaller to {}", dest.display()),
                        );
                    }
                    Ok(s) => callbacks.on_log(
                        LogLevel::Warn,
                        &format!("ditto uninstall.app returned {s} (non-fatal)"),
                    ),
                    Err(e) => callbacks.on_log(
                        LogLevel::Warn,
                        &format!("ditto for uninstall.app failed to launch: {e}"),
                    ),
                }
            }

            // Clean up orphaned files from a prior install (upgrade).
            if let Some(old) = old_manifest {
                let mut written: Vec<&std::path::Path> = Vec::new();
                for action in &install_manifest.actions {
                    if let MacosAction::FileCopied { dest, backup, .. } = action {
                        written.push(dest);
                        written.extend(backup.as_deref());
                    }
                }
                let old_files = old.actions.iter().filter_map(|a| match a {
                    MacosAction::FileCopied { dest, .. } => Some(dest.as_path()),
                    _ => None,
                });
                let orphans = outto_core::manifest::orphans::orphaned_files(
                    old_files,
                    written,
                    Some(&resolver),
                    false,
                );
                for dest in orphans.iter().filter(|d| d.exists()) {
                    callbacks.on_log(
                        LogLevel::Info,
                        &format!("Upgrade: removing orphaned file {}", dest.display()),
                    );
                    if let Err(e) = std::fs::remove_file(dest) {
                        callbacks.on_log(
                            LogLevel::Warn,
                            &format!("Upgrade: could not remove {}: {e}", dest.display()),
                        );
                    }
                }

                // Backups an older outto left behind after its install.
                let new_files: Vec<&std::path::Path> = install_manifest
                    .actions
                    .iter()
                    .filter_map(|a| match a {
                        MacosAction::FileCopied { dest, .. } => Some(dest.as_path()),
                        _ => None,
                    })
                    .collect();
                let old_backups = old.actions.iter().filter_map(|a| match a {
                    MacosAction::FileCopied {
                        backup: Some(b), ..
                    } => Some(b.as_path()),
                    _ => None,
                });
                for backup in outto_core::manifest::orphans::orphaned_files(
                    old_backups,
                    new_files,
                    Some(&resolver),
                    false,
                ) {
                    remove_backup(&backup, callbacks);
                }
            }

            callbacks.on_log(LogLevel::Info, "Installation complete");
            callbacks.on_progress("complete", 1, 1);
            Ok(())
        }
        Err(e) => {
            callbacks.on_log(
                LogLevel::Error,
                &format!("Installation failed: {e}. Rolling back..."),
            );
            let rollback_result = rollback_actions(&install_manifest.actions, callbacks, true);
            match rollback_result {
                Ok(()) => {
                    callbacks.on_log(LogLevel::Info, "Rollback completed successfully");
                    Err(e)
                }
                Err(rb) => Err(InstallerError::RollbackFailed {
                    original_error: e.to_string(),
                    rollback_error: rb.to_string(),
                }),
            }
        }
    }
}

/// Uninstall by receipt lookup (macOS has no install-dir-embedded receipt).
///
/// The `_install_dir` parameter is ignored on macOS — kept for signature
/// compatibility with the Windows backend. The package id alone is sufficient
/// to locate the receipt under `~/Library/no.divvun.install/packages/` or
/// `/Library/no.divvun.install/packages/`.
pub fn uninstall_package(
    _install_dir: &std::path::Path,
    package_id: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    uninstall::uninstall(package_id, callbacks)
}

/// Detach the backups of overwritten files from a committed install; see the
/// Windows backend for why nothing needs them once the install has succeeded.
fn take_backups(manifest: &mut InstallManifest<MacosAction>) -> Vec<PathBuf> {
    manifest
        .actions
        .iter_mut()
        .filter_map(|a| match a {
            MacosAction::FileCopied { backup, .. } => backup.take(),
            _ => None,
        })
        .collect()
}

fn remove_backup(backup: &std::path::Path, callbacks: &dyn InstallerCallbacks) {
    if !backup.exists() {
        return;
    }
    match std::fs::remove_file(backup) {
        Ok(()) => callbacks.on_log(
            LogLevel::Debug,
            &format!("Removed backup {}", backup.display()),
        ),
        Err(e) => callbacks.on_log(
            LogLevel::Warn,
            &format!("Could not remove backup {}: {e}", backup.display()),
        ),
    }
}

/// Keep what the package owned before an upgrade.
///
/// The new install sees the previous one's work as already there, so on its
/// own it would record no directory or plist file as created, and would take
/// the previous install's plist values and symlink targets as "what was
/// there before" — uninstall would then restore them instead of removing
/// them. So: directories and plist files the previous install created are
/// carried over (first, so uninstall reaches them last), and a plist value or
/// symlink both installs set keeps the state from before the first one.
fn inherit_from_previous(
    new: &mut InstallManifest<MacosAction>,
    old: &InstallManifest<MacosAction>,
) {
    use outto_core::manifest::orphans::path_key;
    let key = |p: &std::path::Path| path_key(p, None, false);

    for action in new.actions.iter_mut() {
        match action {
            MacosAction::PlistValueSet {
                path,
                key_path,
                previous_value,
            } => {
                if let Some(original) = old.actions.iter().find_map(|a| match a {
                    MacosAction::PlistValueSet {
                        path: p,
                        key_path: k,
                        previous_value: v,
                    } if k == key_path && key(p) == key(path) => Some(v.clone()),
                    _ => None,
                }) {
                    *previous_value = original;
                }
            }
            MacosAction::SymlinkCreated {
                link,
                previous_target,
                ..
            } => {
                if let Some(original) = old.actions.iter().find_map(|a| match a {
                    MacosAction::SymlinkCreated {
                        link: l,
                        previous_target: t,
                        ..
                    } if key(l) == key(link) => Some(t.clone()),
                    _ => None,
                }) {
                    *previous_target = original;
                }
            }
            _ => {}
        }
    }

    let created_path = |a: &MacosAction| match a {
        MacosAction::DirectoryCreated { path } | MacosAction::PlistFileCreated { path } => {
            Some(key(path))
        }
        _ => None,
    };
    let inherited: Vec<MacosAction> = old
        .actions
        .iter()
        .filter(|a| {
            created_path(a).is_some_and(|k| {
                !new.actions
                    .iter()
                    .any(|b| created_path(b).as_ref() == Some(&k))
            })
        })
        .cloned()
        .collect();
    new.actions.splice(0..0, inherited);
    outto_core::manifest::order_directories_last_to_undo(&mut new.actions, |a| match a {
        MacosAction::DirectoryCreated { path } => Some(path.as_path()),
        _ => None,
    });
}

fn classify_scope(install_dir: &std::path::Path) -> String {
    let s = install_dir.to_string_lossy();
    if s.starts_with("/Library")
        || s.starts_with("/usr/local")
        || s.starts_with("/System")
        || s.starts_with("/Applications")
    {
        // /Applications is writable for the current user on most setups, so
        // treat it as user scope if HOME-adjacent, system otherwise.
        // Simplification: if we had to elevate, scope is system.
        if elevation::is_root() {
            "system".to_string()
        } else {
            "user".to_string()
        }
    } else {
        "user".to_string()
    }
}

fn receipt_base_for(scope: &str) -> PathBuf {
    match scope {
        "system" => detect::system_receipt_base(),
        _ => detect::user_receipt_base().unwrap_or_else(|| detect::system_receipt_base()),
    }
}

/// Restart the machine via `osascript` (System Events), which asks the logged-in
/// session to restart without needing root. Provided for cross-platform parity
/// with the Windows backend; the macOS config schema has no reboot policy, so
/// in practice this is only reachable if a front-end calls it directly.
pub fn reboot_system() -> InstallerResult<()> {
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to restart")
        .status()
        .map_err(|e| InstallerError::Other(format!("osascript failed to launch: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(InstallerError::Other(format!(
            "osascript restart exited with {status}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::callbacks::NoOpCallbacks;

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() && !p.is_symlink() {
                    out.extend(walk(&p));
                }
                out.push(p);
            }
        }
        out
    }

    /// After install → upgrade → uninstall nothing the package created is
    /// left: no `.bak` files after either install; nested directories (also
    /// ones only the first install created), the plist file it created and
    /// its symlink are gone; a directory and a plist that existed beforehand
    /// stay, the plist with its original value back.
    #[test]
    fn upgrade_then_uninstall_leaves_nothing_behind() {
        let _home_guard = crate::test_util::lock_home();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        let install_dir = root.join("app");
        let shared = root.join("shared");
        let preexisting = root.join("preexisting");
        let own_plist = root.join("prefs").join("own.plist");
        let user_plist = preexisting.join("user.plist");
        let link = root.join("links").join("tool");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&preexisting).unwrap();
        let mut user_dict = plist::Dictionary::new();
        user_dict.insert("Setting".into(), plist::Value::String("user".into()));
        plist::Value::Dictionary(user_dict)
            .to_file_xml(&user_plist)
            .unwrap();
        let old_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };

        let run = |version: &str, content: &str| -> InstallerResult<()> {
            let source = root.join(format!("source-{version}"));
            std::fs::create_dir_all(source.join("deps")).unwrap();
            std::fs::write(source.join("app.txt"), content).unwrap();
            std::fs::write(source.join("deps").join("dep.txt"), content).unwrap();
            let toml = format!(
                r##"
[package]
id = "no.divvun.outto-leftover-test"
name = "Leftover Test"
version = "{version}"

[[files]]
source = "app.txt"
dest = "#{{app}}"

[[files]]
source = "deps/*"
dest = "#{{app}}/dependencies/deep"

[[files]]
source = "app.txt"
dest = "{shared}/a/b"

[[files]]
source = "app.txt"
dest = "{preexisting}"

[[plist]]
path = "{own_plist}"
values = [{{ key = "Version", type = "string", data = "#{{package.version}}" }}]

[[plist]]
path = "{user_plist}"
values = [{{ key = "Setting", type = "string", data = "#{{package.version}}" }}]

[[symlinks]]
target = "#{{app}}/app.txt"
link = "{link}"
overwrite = "always"
"##,
                shared = shared.display(),
                preexisting = preexisting.display(),
                own_plist = own_plist.display(),
                user_plist = user_plist.display(),
                link = link.display(),
            );
            let config = Config::from_toml(&toml).unwrap();
            let options = InstallOptions {
                source_dir: source,
                install_dir: Some(install_dir.clone()),
                selected_components: None,
                uninstall_exe: None,
            };
            install(&config, &options, &NoOpCallbacks)
        };

        let mut baks = Vec::new();
        let v1 = run("1.0.0", "v1");
        baks.extend(
            walk(&root)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "bak")),
        );
        let v2 = run("2.0.0", "v2");
        baks.extend(
            walk(&root)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "bak")),
        );
        let app_after_upgrade = std::fs::read_to_string(install_dir.join("app.txt"));
        let uninstalled = uninstall_package(
            &install_dir,
            "no.divvun.outto-leftover-test",
            &NoOpCallbacks,
        );

        match old_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        v1.unwrap();
        v2.unwrap();
        uninstalled.unwrap();
        assert!(baks.is_empty(), "{baks:?}");
        assert_eq!(app_after_upgrade.unwrap(), "v2");

        assert!(!install_dir.exists(), "{:?}", walk(&install_dir));
        assert!(!shared.exists(), "{:?}", walk(&shared));
        assert!(!own_plist.parent().unwrap().exists(), "{:?}", walk(&root));
        assert!(!link.parent().unwrap().exists(), "{:?}", walk(&root));
        assert!(preexisting.is_dir());
        assert!(!preexisting.join("app.txt").exists());
        let user = plist::Value::from_file(&user_plist).unwrap();
        assert_eq!(
            user.as_dictionary()
                .and_then(|d| d.get("Setting"))
                .and_then(|v| v.as_string()),
            Some("user")
        );
    }

    /// `before_uninstall` / `after_uninstall` commands are recorded at install
    /// time and run, in order around the rollback, by uninstall.
    #[test]
    fn uninstall_runs_recorded_hooks() {
        let _home_guard = crate::test_util::lock_home();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let source = tmp.path().join("source");
        let install_dir = tmp.path().join("app");
        let trace = tmp.path().join("trace.txt");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.txt"), "a").unwrap();
        let old_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };

        let toml = format!(
            r##"
[package]
id = "no.divvun.outto-hook-test"
name = "Hook Test"
version = "1.0.0"

[[files]]
source = "a.txt"
dest = "#{{app}}"

[[run]]
phase = "before_uninstall"
command = "/bin/sh"
arguments = "-c \"test -f '#{{app}}/a.txt' && echo before >> '{trace}'\""

[[run]]
phase = "after_uninstall"
command = "/bin/sh"
arguments = "-c \"test -f '#{{app}}/a.txt' || echo after >> '{trace}'\""
"##,
            trace = trace.display()
        );
        let config = Config::from_toml(&toml).unwrap();
        let options = InstallOptions {
            source_dir: source,
            install_dir: Some(install_dir.clone()),
            selected_components: None,
            uninstall_exe: None,
        };
        let result = install(&config, &options, &NoOpCallbacks);
        let hooks = InstallManifest::<MacosAction>::load_from_base(
            &detect::user_receipt_base().unwrap(),
            "no.divvun.outto-hook-test",
        )
        .map(|m| m.uninstall_hooks);
        let uninstalled =
            uninstall_package(&install_dir, "no.divvun.outto-hook-test", &NoOpCallbacks);

        match old_home {
            Some(h) => unsafe { std::env::set_var("HOME", h) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        result.unwrap();
        assert_eq!(hooks.unwrap().map(|h| h.len()), Some(2));
        uninstalled.unwrap();
        assert_eq!(std::fs::read_to_string(&trace).unwrap(), "before\nafter\n");
        assert!(!install_dir.join("a.txt").exists());
    }
}
