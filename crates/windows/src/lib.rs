//! Windows backend for the outto installer framework.
//!
//! Owns the Windows-specific install and uninstall pipelines: UAC elevation
//! checks, Add/Remove Programs registration, PE section embedding, all
//! Windows-only action types (registry, COM, services, shortcuts, fonts,
//! associations, environment variables), and the rollback dispatcher that
//! reverses them. The shared framework (config parsing, manifest, rollback
//! scaffolding, neutral action primitives) lives in `outto-core`.

#![cfg(windows)]

pub mod actions;
pub mod detect;
pub mod discard;
pub mod elevation;
pub mod manifest;
pub mod paths;
pub mod pe;
pub mod pending_renames;
pub mod restart_manager;
pub mod self_delete;
pub mod uninstall;

use std::path::PathBuf;

use outto_core::callbacks::{InstallOptions, InstallerCallbacks, LogLevel};
use outto_core::config::{
    DowngradePolicy, InstalledVersionFrom, LegacyInnoMode, RebootPolicy, UpgradePolicy,
    VariableResolver,
};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{CoreAction, InstallManifest, rollback};

pub use manifest::Action as WindowsAction;
pub use outto_core::Config;
pub use uninstall::uninstall as uninstall_package;

/// Build a Windows-flavoured `VariableResolver`, pre-populated with package
/// metadata, install dir, and all the Windows shell-folder variables.
pub fn make_resolver(config: &Config, install_dir: Option<&std::path::Path>) -> VariableResolver {
    let mut r = VariableResolver::new()
        .with_windows_paths(true)
        .with_package(&config.package.name, &config.package.version);
    r = paths::with_windows_env(r);
    if let Some(dir) = install_dir {
        r = r.with_install_dir(dir);
    }
    r
}

/// Windows install entry point. Mirrors the pipeline the root crate used to own:
/// arch + elevation check → ARP detect → prerequisites → create install dir →
/// [`actions::execute_install`] → manifest save → ARP write → upgrade cleanup.
/// On failure, rolls back every recorded action in reverse.
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

    if !detect::arch_matches(&config.package.architecture) {
        return Err(InstallerError::Validation(format!(
            "architecture mismatch: package requires {:?} but system is {}",
            config.package.architecture,
            elevation::get_system_architecture()
        )));
    }

    if elevation::needs_elevation(&config.package.privileges) {
        return Err(InstallerError::ElevationRequired(
            "this installer requires administrator privileges".into(),
        ));
    }

    // Without an uninstaller the ARP entry gets no UninstallString and the
    // package can never be removed, so refuse before touching anything.
    let uninstall_exe_src = match options.uninstall_exe.as_deref() {
        Some(p) if p.is_file() => p.to_path_buf(),
        Some(p) => {
            return Err(InstallerError::Validation(format!(
                "uninstaller not found: {}",
                p.display()
            )));
        }
        None => {
            return Err(InstallerError::Validation(
                "no uninstaller was supplied; the package could not be uninstalled".into(),
            ));
        }
    };

    let install_dir = if let Some(ref dir) = options.install_dir {
        PathBuf::from(
            dir.to_string_lossy()
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        )
    } else if let Some(ref default_dir) = config.package.default_dir {
        let config_resolver = make_resolver(config, None);
        config_resolver.resolve_path(default_dir)?
    } else {
        return Err(InstallerError::Config(
            "no install directory specified (set install_dir in options or default_dir in config)"
                .into(),
        ));
    };

    let resolver = make_resolver(config, Some(&install_dir));
    let uninstall_hooks =
        outto_core::actions::run::resolve_uninstall_hooks(&config.run, &resolver)?;
    let prune_dirs = config
        .upgrade
        .prune
        .iter()
        .map(|dir| resolver.resolve_path(dir))
        .collect::<InstallerResult<Vec<_>>>()?;
    let declared_dirs = config
        .dirs
        .iter()
        .map(|dir| resolver.resolve_path(&dir.path))
        .collect::<InstallerResult<Vec<_>>>()?;
    let uninstall_guards = resolve_uninstall_guards(config, &resolver)?;

    let existing = detect::detect_existing_install(&config.package.id)?;
    let legacy_inno = match &config.package.legacy_inno_app_id {
        Some(guid) => detect::detect_legacy_inno_install(guid),
        None => None,
    };
    let existing_version = existing
        .as_ref()
        .and_then(|e| e.version.clone())
        .or_else(|| legacy_inno.as_ref().and_then(|i| i.version.clone()));
    check_downgrade(config, &resolver, existing_version, callbacks)?;

    let mut old_manifest: Option<InstallManifest<WindowsAction>> = None;
    let mut old_install_dir: Option<PathBuf> = None;
    if let Some(existing) = existing {
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
            UpgradePolicy::Fail => {
                return Err(InstallerError::UpgradeConflict(format!(
                    "{} is already installed",
                    config.package.name
                )));
            }
            UpgradePolicy::SideBySide => {}
            UpgradePolicy::Overwrite => {
                old_manifest =
                    InstallManifest::load(&existing.install_dir, &config.package.id).ok();
                if existing.install_dir != install_dir {
                    old_install_dir = Some(existing.install_dir);
                }
            }
        }
    }

    if let Some(inno) = legacy_inno {
        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Inno Setup installation found: {} v{} at {} ({}\\{})",
                inno.display_name.as_deref().unwrap_or("unknown"),
                inno.version.as_deref().unwrap_or("unknown"),
                inno.install_dir.display(),
                inno.root,
                inno.key
            ),
        );
        match config.upgrade.policy {
            UpgradePolicy::Fail => {
                return Err(InstallerError::UpgradeConflict(format!(
                    "{} is already installed",
                    config.package.name
                )));
            }
            UpgradePolicy::SideBySide => {}
            // We install over it, so it has to be gone before anything else runs.
            UpgradePolicy::Overwrite => match config.package.legacy_inno_mode {
                LegacyInnoMode::Uninstall => uninstall::uninstall_legacy_inno(&inno, callbacks)?,
                LegacyInnoMode::Adopt => uninstall::adopt_legacy_inno(&inno, callbacks)?,
            },
        }
    }

    actions::check_prerequisites_windows(config, callbacks)?;

    let mut install_manifest = InstallManifest::<WindowsAction>::new(
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
    install_manifest.refuse_uninstall_while = uninstall_guards;

    let result = actions::execute_install(
        config,
        &options.source_dir,
        &options.selected_components,
        &resolver,
        &mut install_manifest,
        callbacks,
    );

    let pkg_dir = InstallManifest::<WindowsAction>::package_dir(&install_dir, &config.package.id);
    let uninstall_dest = pkg_dir.join(outto_core::archive::UNINSTALL_EXE);

    match result {
        Ok(()) => {
            // The install can no longer be rolled back, so nothing queued for
            // the next restart may undo it.
            let mut written = written_after_install(&install_manifest);
            written.extend(installed_by_actions(&install_manifest));
            let manifest_path =
                InstallManifest::<WindowsAction>::manifest_path(&install_dir, &config.package.id);
            written.push(&manifest_path);
            written.push(&uninstall_dest);
            pending_renames::cancel_clashing(
                &written,
                &install_manifest.replaced_at_restart,
                callbacks,
            );

            if let Some(old) = &old_manifest {
                remove_orphaned_shortcuts(old, &install_manifest, &resolver, callbacks);
                inherit_kept(&mut install_manifest, old);
                inherit_created(&mut install_manifest, old);
            }
            let backups = take_backups(&mut install_manifest);
            install_manifest.save()?;
            for backup in &backups {
                remove_backup(backup, callbacks);
            }

            if let Some(old) = old_manifest {
                let mut written: Vec<&std::path::Path> = Vec::new();
                for action in &install_manifest.actions {
                    if let WindowsAction::FileCopied { dest, backup, .. } = action {
                        written.push(dest);
                        written.extend(backup.as_deref());
                    }
                }
                let old_files = old.actions.iter().filter_map(|a| match a {
                    WindowsAction::FileCopied { dest, .. } => Some(dest.as_path()),
                    _ => None,
                });
                let orphans = outto_core::manifest::orphans::orphaned_files(
                    old_files,
                    written,
                    Some(&resolver),
                    true,
                );
                for dest in orphans.iter().filter(|d| d.exists()) {
                    callbacks.on_log(
                        LogLevel::Info,
                        &format!("Upgrade: removing orphaned file {}", dest.display()),
                    );
                    if let Err(e) = discard::discard_file_logged(dest, callbacks) {
                        callbacks.on_log(
                            LogLevel::Warn,
                            &format!("Upgrade: could not remove {}: {e}", dest.display()),
                        );
                    }
                }

                // Backups an older outto left behind after its install.
                let old_backups = old.actions.iter().filter_map(|a| match a {
                    WindowsAction::FileCopied {
                        backup: Some(b), ..
                    } => Some(b.as_path()),
                    _ => None,
                });
                for backup in outto_core::manifest::orphans::orphaned_files(
                    old_backups,
                    written_after_install(&install_manifest),
                    Some(&resolver),
                    true,
                ) {
                    remove_backup(&backup, callbacks);
                }

                if let Some(ref old_dir) = old_install_dir {
                    let old_pkg_dir =
                        InstallManifest::<WindowsAction>::package_dir(old_dir, &config.package.id);
                    if old_pkg_dir.exists() {
                        callbacks.on_log(
                            LogLevel::Info,
                            &format!(
                                "Upgrade: removing old package dir {}",
                                old_pkg_dir.display()
                            ),
                        );
                        let _ = std::fs::remove_dir_all(&old_pkg_dir);
                    }
                    if old_dir.exists() {
                        let _ = std::fs::remove_dir(old_dir);
                    }
                }
            }

            for dir in &prune_dirs {
                prune(dir, &install_manifest, &declared_dirs, callbacks);
            }

            std::fs::copy(&uninstall_exe_src, &uninstall_dest).map_err(|e| {
                InstallerError::FileOp {
                    path: uninstall_dest.clone(),
                    source: e,
                }
            })?;
            callbacks.on_log(
                LogLevel::Info,
                &format!("Copied uninstaller to {}", uninstall_dest.display()),
            );
            let uninstall_string = format!(
                "\"{}\" --dir \"{}\"",
                uninstall_dest.display(),
                install_dir.display()
            );

            let display_icon = config
                .uninstall
                .display_icon
                .as_deref()
                .map(|i| resolver.resolve(i))
                .transpose()?;

            detect::write_uninstall_registry(&detect::UninstallRegistryInfo {
                package_id: &config.package.id,
                display_name: &config.package.name,
                version: &config.package.version,
                publisher: config.package.publisher.as_deref(),
                install_dir: &install_dir,
                display_icon: display_icon.as_deref(),
                url: config.package.url.as_deref(),
                support_url: config.package.support_url.as_deref(),
                uninstall_string: &uninstall_string,
                depends_on: &config.package.depends_on,
            })?;

            callbacks.on_log(
                LogLevel::Info,
                &format!(
                    "Registered uninstall entry {} (UninstallString: {uninstall_string})",
                    config.package.id
                ),
            );

            callbacks.on_log(LogLevel::Info, "Installation complete");
            callbacks.on_progress("complete", 1, 1);

            // Apply the reboot policy. `Never` suppresses even a requested
            // restart; `IfNeeded` honours a 3010/1641 from a [[run]] command;
            // `Always` forces one. The front-end decides whether/when to act,
            // respecting /NORESTART and silent mode.
            let want_reboot = match config.reboot.policy {
                RebootPolicy::Never => false,
                RebootPolicy::Always => true,
                RebootPolicy::IfNeeded => install_manifest.reboot_needed,
            };
            if want_reboot {
                callbacks.on_log(
                    LogLevel::Info,
                    "A system restart is required to complete installation",
                );
                callbacks.on_reboot_required();
            }
            Ok(())
        }
        Err(e) => {
            callbacks.on_log(
                LogLevel::Error,
                &format!("Installation failed: {e}. Rolling back..."),
            );

            let rollback_result =
                rollback::rollback_actions(&install_manifest.actions, callbacks, true);

            match rollback_result {
                Ok(()) => {
                    callbacks.on_log(LogLevel::Info, "Rollback completed successfully");
                    Err(e)
                }
                Err(rollback_err) => Err(InstallerError::RollbackFailed {
                    original_error: e.to_string(),
                    rollback_error: rollback_err.to_string(),
                }),
            }
        }
    }
}

/// With `[upgrade] downgrade = "refuse"`, fail before anything changes when
/// the installed version has higher precedence than this package's. The
/// installed version is `[upgrade.installed_version]`'s when set, else
/// `existing_version`, the version of the install being replaced.
fn check_downgrade(
    config: &Config,
    resolver: &VariableResolver,
    existing_version: Option<String>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    if config.upgrade.downgrade != DowngradePolicy::Refuse {
        return Ok(());
    }
    let installed = match &config.upgrade.installed_version {
        Some(probe) => {
            let key = resolver.resolve(&probe.registry_key)?;
            let value = actions::registry::read_string(&key, probe.value.as_deref().unwrap_or(""))?;
            let version = value.and_then(|v| match probe.from {
                InstalledVersionFrom::Value => Some(v),
                InstalledVersionFrom::ParentDirName => installed_version_dir(&v),
            });
            callbacks.on_log(
                LogLevel::Info,
                &format!(
                    "Installed version from {key}: {}",
                    version.as_deref().unwrap_or("none")
                ),
            );
            version
        }
        None => existing_version,
    };
    let Some(installed) = installed else {
        return Ok(());
    };
    match is_downgrade(&installed, &config.package.version) {
        Some(true) => Err(InstallerError::UpgradeConflict(format!(
            "{} {installed} is installed, which is newer than {}; not downgrading",
            config.package.name, config.package.version
        ))),
        Some(false) => Ok(()),
        None => {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Installed version {installed} is not SemVer; installing over it"),
            );
            Ok(())
        }
    }
}

/// The name of the directory holding the file `path` names.
fn installed_version_dir(path: &str) -> Option<String> {
    let path = path.trim().trim_matches('"');
    let dir = path.rsplit_once(['\\', '/'])?.0;
    let name = dir.rsplit(['\\', '/']).next()?;
    (!name.is_empty()).then(|| name.to_string())
}

/// Whether installing `new` over `installed` goes back to a version of lower
/// SemVer precedence (build metadata does not count); `None` if either is
/// not SemVer.
fn is_downgrade(installed: &str, new: &str) -> Option<bool> {
    let installed = semver::Version::parse(installed.trim()).ok()?;
    let new = semver::Version::parse(new.trim()).ok()?;
    Some(installed.cmp_precedence(&new) == std::cmp::Ordering::Greater)
}

/// `[[uninstall.refuse_while]]` with its variables expanded, for the receipt.
fn resolve_uninstall_guards(
    config: &Config,
    resolver: &VariableResolver,
) -> InstallerResult<Vec<outto_core::manifest::UninstallGuard>> {
    config
        .uninstall
        .refuse_while
        .iter()
        .map(|refusal| {
            Ok(outto_core::manifest::UninstallGuard {
                registry_key: resolver.resolve(&refusal.registry_key)?,
                message: match &refusal.message {
                    Some(message) => resolver.resolve(message)?,
                    None => format!("{} is still in use.", config.package.name),
                },
            })
        })
        .collect()
}

/// Keep owning the files the previous install recorded that this one found
/// in place and left alone (an overwrite policy such as `never`), so the
/// upgrade does not remove them as orphans and uninstall still does. They go
/// before everything but directories, where the file phase records them.
fn inherit_kept(new: &mut InstallManifest<WindowsAction>, old: &InstallManifest<WindowsAction>) {
    use outto_core::manifest::orphans::path_key;
    let key = |p: &std::path::Path| path_key(p, None, true);
    let kept: std::collections::HashSet<String> = new.kept_files.iter().map(|p| key(p)).collect();
    let mut owned: std::collections::HashSet<String> =
        written_after_install(new).into_iter().map(key).collect();
    let inherited: Vec<WindowsAction> = old
        .actions
        .iter()
        .filter_map(|a| match a {
            WindowsAction::FileCopied {
                dest,
                preserve_on_uninstall,
                uninst_remove_readonly,
                uninst_restart_delete,
                restart_replace,
                ..
            } if kept.contains(&key(dest)) && owned.insert(key(dest)) => {
                Some(WindowsAction::FileCopied {
                    dest: dest.clone(),
                    backup: None,
                    preserve_on_uninstall: *preserve_on_uninstall,
                    uninst_remove_readonly: *uninst_remove_readonly,
                    uninst_restart_delete: *uninst_restart_delete,
                    restart_replace: *restart_replace,
                })
            }
            _ => None,
        })
        .collect();
    new.actions.splice(0..0, inherited);
}

/// Remove everything under `dir` that is not this install's: a file
/// `manifest` does not record as copied, a directory that is neither one of
/// `declared_dirs` (`[[dirs]]`) nor holds one of those or a copied file.
/// Directories an earlier install created are not this install's, so an
/// earlier version's directory goes. Receipt (`.outto`) directories and
/// files staged to replace others at the next restart stay. A protected
/// directory such as `%ProgramFiles%` is never pruned.
fn prune(
    dir: &std::path::Path,
    manifest: &InstallManifest<WindowsAction>,
    declared_dirs: &[PathBuf],
    callbacks: &dyn InstallerCallbacks,
) {
    use outto_core::manifest::orphans::path_key;
    use outto_core::manifest::rollback::{is_protected_dir, protected_dirs};
    let key = |p: &std::path::Path| path_key(p, None, true);

    if is_protected_dir(dir, &protected_dirs()) {
        callbacks.on_log(
            LogLevel::Warn,
            &format!("Upgrade: not pruning {}, a system directory", dir.display()),
        );
        return;
    }
    let files: std::collections::HashSet<String> = written_after_install(manifest)
        .into_iter()
        .map(key)
        .collect();
    let dirs: std::collections::HashSet<String> = declared_dirs.iter().map(|d| key(d)).collect();
    let holds_recorded = |d: &std::path::Path| {
        let prefix = format!("{}\\", key(d));
        dirs.contains(&key(d))
            || files
                .iter()
                .chain(dirs.iter())
                .any(|k| k.starts_with(&prefix))
    };

    fn walk(
        dir: &std::path::Path,
        keep_file: &dyn Fn(&std::path::Path) -> bool,
        keep_dir: &dyn Fn(&std::path::Path) -> bool,
        callbacks: &dyn InstallerCallbacks,
    ) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if name == ".outto" {
                    continue;
                }
                if keep_dir(&path) {
                    walk(&path, keep_file, keep_dir, callbacks);
                } else {
                    callbacks.on_log(
                        LogLevel::Info,
                        &format!("Upgrade: removing {}", path.display()),
                    );
                    discard::discard_tree(&path, callbacks);
                }
            } else if !keep_file(&path) && !name.ends_with(".outto-pending") {
                callbacks.on_log(
                    LogLevel::Info,
                    &format!("Upgrade: removing {}", path.display()),
                );
                if let Err(e) = discard::discard_file_logged(&path, callbacks) {
                    callbacks.on_log(
                        LogLevel::Warn,
                        &format!("Upgrade: could not remove {}: {e}", path.display()),
                    );
                }
            }
        }
    }

    walk(
        dir,
        &|f| files.contains(&key(f)),
        &holds_recorded,
        callbacks,
    );
}

/// Remove the shortcuts the previous install created that this one did not
/// create again, such as one in a user's Start Menu that is now in all
/// users', and the folders the previous install created for them once
/// they are empty.
fn remove_orphaned_shortcuts(
    old: &InstallManifest<WindowsAction>,
    new: &InstallManifest<WindowsAction>,
    resolver: &VariableResolver,
    callbacks: &dyn InstallerCallbacks,
) {
    use outto_core::manifest::orphans::{orphaned_files, path_key};
    use outto_core::manifest::rollback::{is_protected_dir, protected_dirs};
    let key = |p: &std::path::Path| path_key(p, None, true);
    let shortcuts = |m: &InstallManifest<WindowsAction>| -> Vec<PathBuf> {
        m.actions
            .iter()
            .filter_map(|a| match a {
                WindowsAction::ShortcutCreated { path } => Some(path.clone()),
                _ => None,
            })
            .collect()
    };
    let created = |m: &InstallManifest<WindowsAction>| -> std::collections::HashSet<String> {
        m.actions
            .iter()
            .filter_map(|a| match a {
                WindowsAction::DirectoryCreated { path } => Some(key(path)),
                _ => None,
            })
            .collect()
    };
    let (old_shortcuts, new_shortcuts) = (shortcuts(old), shortcuts(new));
    let orphans = orphaned_files(
        old_shortcuts.iter().map(PathBuf::as_path),
        new_shortcuts.iter().map(PathBuf::as_path),
        Some(resolver),
        true,
    );
    if orphans.is_empty() {
        return;
    }
    let (old_dirs, new_dirs) = (created(old), created(new));
    let protected = protected_dirs();

    for shortcut in orphans {
        if shortcut.exists() {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Upgrade: removing old shortcut {}", shortcut.display()),
            );
            if let Err(e) = std::fs::remove_file(&shortcut) {
                callbacks.on_log(
                    LogLevel::Warn,
                    &format!("Upgrade: could not remove {}: {e}", shortcut.display()),
                );
                continue;
            }
        }
        for dir in shortcut.ancestors().skip(1) {
            let k = key(dir);
            if !old_dirs.contains(&k) || new_dirs.contains(&k) || is_protected_dir(dir, &protected)
            {
                break;
            }
            if std::fs::remove_dir(dir).is_err() {
                break;
            }
            callbacks.on_log(
                LogLevel::Info,
                &format!("Upgrade: removed empty folder {}", dir.display()),
            );
        }
    }
}

/// Every file the install wrote.
fn written_after_install(manifest: &InstallManifest<WindowsAction>) -> Vec<&std::path::Path> {
    manifest
        .actions
        .iter()
        .filter_map(|a| match a {
            WindowsAction::FileCopied { dest, .. } => Some(dest.as_path()),
            _ => None,
        })
        .collect()
}

/// The files other than `[[files]]` the install put in place: fonts and
/// shortcuts.
fn installed_by_actions(manifest: &InstallManifest<WindowsAction>) -> Vec<&std::path::Path> {
    manifest
        .actions
        .iter()
        .filter_map(|a| match a {
            WindowsAction::FontInstalled { file, .. } => Some(file.as_path()),
            WindowsAction::ShortcutCreated { path } => Some(path.as_path()),
            _ => None,
        })
        .collect()
}

/// Detach the backups of overwritten files from a committed install.
///
/// Backups exist so a failed install can be rolled back. Once the install has
/// succeeded nothing uses them: like Inno Setup (and MSI), uninstall removes
/// the installed file rather than restoring whatever it replaced, so keeping
/// `foo.dll.bak` next to every replaced file — in System32 too — only leaves
/// clutter until uninstall, or forever if an upgrade drops the file.
fn take_backups(manifest: &mut InstallManifest<WindowsAction>) -> Vec<PathBuf> {
    manifest
        .actions
        .iter_mut()
        .filter_map(|a| match a {
            WindowsAction::FileCopied { backup, .. } => backup.take(),
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

/// Carry over from the previous install's manifest the directories and
/// registry keys it created that the new install found already there, so
/// the package still owns them and uninstall can remove them once empty.
/// They go first, so uninstall reaches them last, after everything inside.
fn inherit_created(new: &mut InstallManifest<WindowsAction>, old: &InstallManifest<WindowsAction>) {
    use outto_core::manifest::orphans::path_key;
    let same = |a: &WindowsAction, b: &WindowsAction| match (a, b) {
        (
            WindowsAction::DirectoryCreated { path: p },
            WindowsAction::DirectoryCreated { path: q },
        ) => path_key(p, None, true) == path_key(q, None, true),
        (
            WindowsAction::RegistryKeyCreated {
                root: r1, key: k1, ..
            },
            WindowsAction::RegistryKeyCreated {
                root: r2, key: k2, ..
            },
        ) => r1.eq_ignore_ascii_case(r2) && k1.eq_ignore_ascii_case(k2),
        _ => false,
    };
    let inherited: Vec<WindowsAction> = old
        .actions
        .iter()
        .filter(|a| match a {
            // Gone with what was in it, such as an old shortcut's folder.
            WindowsAction::DirectoryCreated { path } => path.exists(),
            WindowsAction::RegistryKeyCreated { .. } => true,
            _ => false,
        })
        .filter(|a| !new.actions.iter().any(|b| same(a, b)))
        .cloned()
        .collect();
    new.actions.splice(0..0, inherited);
    outto_core::manifest::order_directories_last_to_undo(&mut new.actions, |a| match a {
        WindowsAction::DirectoryCreated { path } => Some(path.as_path()),
        _ => None,
    });
}

/// Reboot the machine now. Enables `SeShutdownPrivilege` on the current
/// process token (present-but-disabled by default on an elevated token), then
/// calls `ExitWindowsEx(EWX_REBOOT)`. Returns once the restart is *initiated*;
/// Windows then tears down processes, so callers should treat success as
/// "we're going down".
pub fn reboot_system() -> InstallerResult<()> {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Shutdown::*;
    use windows_sys::Win32::System::Threading::*;

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return Err(InstallerError::Other(format!(
                "OpenProcessToken failed: {}",
                std::io::Error::last_os_error()
            )));
        }

        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SE_SHUTDOWN_NAME, looked up by name to avoid a UTF-16 literal const.
        let priv_name: Vec<u16> = "SeShutdownPrivilege"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        if LookupPrivilegeValueW(std::ptr::null(), priv_name.as_ptr(), &mut luid) == 0 {
            let err = std::io::Error::last_os_error();
            CloseHandle(token);
            return Err(InstallerError::Other(format!(
                "LookupPrivilegeValueW failed: {err}"
            )));
        }

        let tp = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let adjusted =
            AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        let adjust_err = std::io::Error::last_os_error();
        CloseHandle(token);
        // AdjustTokenPrivileges can return success but leave the privilege
        // unassigned (GetLastError == ERROR_NOT_ALL_ASSIGNED); surface that.
        if adjusted == 0 || adjust_err.raw_os_error() == Some(ERROR_NOT_ALL_ASSIGNED as i32) {
            return Err(InstallerError::Other(format!(
                "could not acquire shutdown privilege (are we elevated?): {adjust_err}"
            )));
        }

        // EWX_FORCEIFHUNG lets the reboot proceed past unresponsive apps.
        let ok = ExitWindowsEx(
            EWX_REBOOT | EWX_FORCEIFHUNG,
            SHTDN_REASON_MAJOR_APPLICATION
                | SHTDN_REASON_MINOR_INSTALLATION
                | SHTDN_REASON_FLAG_PLANNED,
        );
        if ok == 0 {
            return Err(InstallerError::Other(format!(
                "ExitWindowsEx failed: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::callbacks::{NoOpCallbacks, Prompt, PromptResponse};
    use outto_core::config::Config;
    use outto_core::error::ErrorAction;
    use std::collections::HashSet;
    use std::fs;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct TestCallbacks {
        logs: Arc<Mutex<Vec<(LogLevel, String)>>>,
    }

    impl InstallerCallbacks for TestCallbacks {
        fn on_progress(&self, _phase: &str, _current: u64, _total: u64) {}
        fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
            PromptResponse::Yes
        }
        fn on_log(&self, level: LogLevel, message: &str) {
            self.logs.lock().unwrap().push((level, message.to_string()));
        }
        fn on_error(&self, _error: &InstallerError) -> ErrorAction {
            ErrorAction::Abort
        }
    }

    /// A stand-in uninstaller. `install` refuses to run without one and only
    /// copies it into the receipt, so any file will do.
    fn fake_uninstaller() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "outto_test_uninstall_{}_{}.exe",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::write(&p, b"MZ").unwrap();
        p
    }

    #[test]
    fn test_install_basic_files() {
        let test_dir = std::env::temp_dir().join("outto_test_install");
        let source_dir = test_dir.join("source");
        let install_dir = test_dir.join("installed");

        let _ = fs::remove_dir_all(&test_dir);

        fs::create_dir_all(source_dir.join("build")).unwrap();
        fs::write(source_dir.join("build/app.exe"), "fake exe").unwrap();
        fs::write(source_dir.join("build/readme.txt"), "readme").unwrap();

        let toml = r##"
[package]
id = "com.test.basic"
name = "BasicTest"
version = "1.0.0"

[[files]]
source = "build/*"
dest = "#{app}"
overwrite = "always"
"##;
        let config = Config::from_toml(toml).unwrap();
        let callbacks = TestCallbacks::default();

        let options = InstallOptions {
            source_dir,
            install_dir: Some(install_dir.clone()),
            selected_components: None,
            uninstall_exe: Some(fake_uninstaller()),
        };

        let result = install(&config, &options, &callbacks);
        assert!(result.is_ok(), "Install failed: {result:?}");

        assert!(install_dir.join("app.exe").exists());
        assert!(install_dir.join("readme.txt").exists());

        assert!(
            InstallManifest::<WindowsAction>::manifest_path(&install_dir, "com.test.basic")
                .exists()
        );
        assert!(
            InstallManifest::<WindowsAction>::package_dir(&install_dir, "com.test.basic")
                .join("uninstall.exe")
                .is_file()
        );

        let result = uninstall_package(&install_dir, "com.test.basic", &callbacks);
        assert!(result.is_ok(), "Uninstall failed: {result:?}");

        assert!(!install_dir.join("app.exe").exists());
        assert!(!install_dir.join("readme.txt").exists());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_install_with_components() {
        let test_dir = std::env::temp_dir().join("outto_test_components");
        let source_dir = test_dir.join("source");
        let install_dir = test_dir.join("installed");

        let _ = fs::remove_dir_all(&test_dir);

        fs::create_dir_all(source_dir.join("core")).unwrap();
        fs::create_dir_all(source_dir.join("extras")).unwrap();
        fs::write(source_dir.join("core/app.exe"), "core").unwrap();
        fs::write(source_dir.join("extras/plugin.dll"), "extras").unwrap();

        let toml = r##"
[package]
id = "com.test.comp"
name = "CompTest"
version = "1.0.0"

[[components]]
name = "core"
required = true

[[components]]
name = "extras"

[[files]]
source = "core/*"
dest = "#{app}"
component = "core"

[[files]]
source = "extras/*"
dest = "#{app}/extras"
component = "extras"
"##;
        let config = Config::from_toml(toml).unwrap();
        let callbacks = TestCallbacks::default();

        let mut selected = HashSet::new();
        selected.insert("core".to_string());

        let options = InstallOptions {
            source_dir,
            install_dir: Some(install_dir.clone()),
            selected_components: Some(selected),
            uninstall_exe: Some(fake_uninstaller()),
        };

        let result = install(&config, &options, &callbacks);
        assert!(result.is_ok());

        assert!(install_dir.join("app.exe").exists());
        assert!(!install_dir.join("extras/plugin.dll").exists());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_install_without_uninstaller_is_refused() {
        let test_dir = std::env::temp_dir().join("outto_test_no_uninstaller");
        let source_dir = test_dir.join("source");
        let install_dir = test_dir.join("installed");
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("app.exe"), "fake exe").unwrap();

        let config = Config::from_toml(
            r##"
[package]
id = "com.test.nouninst"
name = "NoUninstTest"
version = "1.0.0"

[[files]]
source = "*"
dest = "#{app}"
"##,
        )
        .unwrap();
        let callbacks = TestCallbacks::default();

        for uninstall_exe in [None, Some(test_dir.join("missing-uninstall.exe"))] {
            let options = InstallOptions {
                source_dir: source_dir.clone(),
                install_dir: Some(install_dir.clone()),
                selected_components: None,
                uninstall_exe,
            };
            let result = install(&config, &options, &callbacks);
            assert!(
                matches!(result, Err(InstallerError::Validation(_))),
                "{result:?}"
            );
            assert!(!install_dir.exists());
        }

        let _ = fs::remove_dir_all(&test_dir);
    }

    /// `before_uninstall` runs while the package's files are still there,
    /// `after_uninstall` once they are gone, and both survive the trip
    /// through the manifest. The program is `#{sys}/cmd.exe`, which only
    /// works once its path has native separators.
    #[test]
    fn test_uninstall_runs_recorded_hooks() {
        let test_dir = std::env::temp_dir().join("outto_test_uninstall_hooks");
        let source_dir = test_dir.join("source");
        let install_dir = test_dir.join("installed");
        let trace = test_dir.join("trace");
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&trace).unwrap();
        fs::write(source_dir.join("app.exe"), "fake exe").unwrap();

        let toml = format!(
            r##"
[package]
id = "com.test.hooks"
name = "HookTest"
version = "1.0.0"

[[files]]
source = "*"
dest = "#{{app}}"

[[run]]
phase = "after_install"
command = "#{{sys}}/cmd.exe"
arguments = "/c mkdir {trace}\\installed"
wait = true
show = "hidden"

[[run]]
phase = "before_uninstall"
command = "#{{sys}}/cmd.exe"
arguments = "/c if exist #{{app}}\\app.exe mkdir {trace}\\before"
wait = true
show = "hidden"

[[run]]
phase = "after_uninstall"
command = "#{{sys}}/cmd.exe"
arguments = "/c if not exist #{{app}}\\app.exe mkdir {trace}\\after"
wait = true
show = "hidden"
"##,
            trace = trace.display().to_string().replace('\\', "\\\\")
        );
        let config = Config::from_toml(&toml).unwrap();
        let callbacks = TestCallbacks::default();
        let options = InstallOptions {
            source_dir,
            install_dir: Some(install_dir.clone()),
            selected_components: None,
            uninstall_exe: Some(fake_uninstaller()),
        };
        install(&config, &options, &callbacks).unwrap();
        assert!(trace.join("installed").is_dir(), "{:?}", callbacks.logs);

        let manifest =
            InstallManifest::<WindowsAction>::load(&install_dir, "com.test.hooks").unwrap();
        let hooks = manifest.uninstall_hooks.unwrap();
        assert_eq!(hooks.len(), 2);
        assert!(
            hooks[0].command.ends_with(r"\System32\cmd.exe"),
            "{}",
            hooks[0].command
        );
        assert!(!hooks[0].arguments.join(" ").contains("#{"));

        uninstall_package(&install_dir, "com.test.hooks", &callbacks).unwrap();
        assert!(trace.join("before").is_dir(), "{:?}", callbacks.logs);
        assert!(trace.join("after").is_dir(), "{:?}", callbacks.logs);

        let _ = fs::remove_dir_all(&test_dir);
    }

    /// After install → upgrade → uninstall nothing the package created is
    /// left: no `.bak` files after either install, the nested directories it
    /// created (including ones only the first install created) and its HKCU
    /// key are gone, and a directory that existed beforehand stays.
    #[test]
    fn test_upgrade_then_uninstall_leaves_nothing_behind() {
        let test_dir = std::env::temp_dir().join("outto_test_leftovers");
        let install_dir = test_dir.join("installed");
        let shared = test_dir.join("shared");
        let preexisting = test_dir.join("preexisting");
        let reg_key = "Software\\OuttoTest_leftovers";
        let _ = fs::remove_dir_all(&test_dir);
        let _ = actions::registry::delete_key("HKCU", reg_key);
        fs::create_dir_all(&preexisting).unwrap();

        let make = |version: &str, content: &str| {
            let source = test_dir.join(format!("source-{version}"));
            fs::create_dir_all(source.join("deps")).unwrap();
            fs::write(source.join("app.exe"), content).unwrap();
            fs::write(source.join("deps").join("dep.exe"), content).unwrap();
            let toml = format!(
                r##"
[package]
id = "com.test.leftovers"
name = "LeftoversTest"
version = "{version}"

[[files]]
source = "app.exe"
dest = "#{{app}}"
overwrite = "always"

[[files]]
source = "deps/*"
dest = "#{{app}}/dependencies/deep"
overwrite = "always"

[[files]]
source = "app.exe"
dest = "{shared}/a/b"
overwrite = "always"

[[files]]
source = "app.exe"
dest = "{preexisting}"
overwrite = "always"

[[registry]]
root = "hkcu"
key = "{reg_key}"
values = [{{ name = "Version", type = "string", data = "#{{package.version}}" }}]
"##,
                shared = shared.display().to_string().replace('\\', "/"),
                preexisting = preexisting.display().to_string().replace('\\', "/"),
                reg_key = reg_key.replace('\\', "\\\\"),
            );
            (Config::from_toml(&toml).unwrap(), source)
        };
        let callbacks = TestCallbacks::default();
        for (version, content) in [("1.0.0", "v1"), ("2.0.0", "v2")] {
            let (config, source_dir) = make(version, content);
            let options = InstallOptions {
                source_dir,
                install_dir: Some(install_dir.clone()),
                selected_components: None,
                uninstall_exe: Some(fake_uninstaller()),
            };
            install(&config, &options, &callbacks).unwrap();
            let baks: Vec<_> = walk(&test_dir)
                .into_iter()
                .filter(|p| p.extension().is_some_and(|e| e == "bak"))
                .collect();
            assert!(baks.is_empty(), "{version}: {baks:?}");
        }
        assert_eq!(
            fs::read_to_string(install_dir.join("app.exe")).unwrap(),
            "v2"
        );

        uninstall_package(&install_dir, "com.test.leftovers", &callbacks).unwrap();
        // The uninstaller binary removes .outto itself; emulate that.
        let _ = fs::remove_dir_all(install_dir.join(".outto"));
        let _ = fs::remove_dir(&install_dir);

        assert!(!install_dir.exists(), "{:?}", walk(&install_dir));
        assert!(!shared.exists(), "{:?}", walk(&shared));
        assert!(preexisting.is_dir());
        assert!(!preexisting.join("app.exe").exists());
        assert!(!hkcu_key_exists(reg_key), "HKCU\\{reg_key} left behind");

        let _ = fs::remove_dir_all(&test_dir);
    }

    fn hkcu_key_exists(key: &str) -> bool {
        use windows_sys::Win32::System::Registry::*;
        let wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey: HKEY = std::ptr::null_mut();
        let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, wide.as_ptr(), 0, KEY_READ, &mut hkey) };
        if r == 0 {
            unsafe { RegCloseKey(hkey) };
        }
        r == 0
    }

    fn walk(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.extend(walk(&p));
                }
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn test_noop_callbacks() {
        let callbacks = NoOpCallbacks;
        callbacks.on_progress("test", 0, 1);
        callbacks.on_log(LogLevel::Info, "test");
        assert_eq!(
            callbacks.on_prompt(Prompt::OverwriteFile {
                path: PathBuf::from("test")
            }),
            PromptResponse::Yes
        );
        assert_eq!(
            callbacks.on_error(&InstallerError::Other("test".into())),
            ErrorAction::Abort
        );
    }

    #[test]
    fn test_is_downgrade_uses_semver_precedence() {
        assert_eq!(is_downgrade("0.2.0", "0.1.0"), Some(true));
        assert_eq!(is_downgrade("0.1.0", "0.1.0"), Some(false));
        assert_eq!(is_downgrade("0.1.0", "0.2.0"), Some(false));
        assert_eq!(
            is_downgrade("0.1.0-dev.20261020T120000Z", "0.1.0-dev.20261007T180918Z"),
            Some(true)
        );
        assert_eq!(
            is_downgrade("0.1.0", "0.1.0-dev.20261020T120000Z"),
            Some(true)
        );
        assert_eq!(is_downgrade("1.0.0+build.2", "1.0.0+build.1"), Some(false));
        assert_eq!(is_downgrade("not a version", "1.0.0"), None);
    }

    #[test]
    fn test_installed_version_dir() {
        assert_eq!(
            installed_version_dir(
                r"C:\Program Files\Divvun\Text Service\0.1.0-dev.20261007T180918Z\divvun_tip_x64.dll"
            )
            .as_deref(),
            Some("0.1.0-dev.20261007T180918Z")
        );
        assert_eq!(
            installed_version_dir(r#""C:/Apps/X/2.0.0/x.dll""#).as_deref(),
            Some("2.0.0")
        );
        assert_eq!(installed_version_dir("x.dll"), None);
    }

    fn system_dll() -> PathBuf {
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\msimg32.dll")
    }

    fn load_library(path: &std::path::Path) -> *mut std::ffi::c_void {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let module =
            unsafe { windows_sys::Win32::System::LibraryLoader::LoadLibraryW(wide.as_ptr()) };
        assert!(!module.is_null(), "{}", std::io::Error::last_os_error());
        module
    }

    /// The text service's layout: every version in its own directory under
    /// `#{app}`, a registry value naming the installed one (as
    /// `InprocServer32` does), `[upgrade] prune` taking away the others even
    /// while one of their files is loaded, a same-version re-run keeping the
    /// files it left alone, a refused downgrade that changes nothing, and an
    /// uninstall refused while a guard key holds.
    #[test]
    fn test_versioned_install_prune_downgrade_and_uninstall_guard() {
        let test_dir = std::env::temp_dir().join("outto_test_versioned");
        let install_dir = test_dir.join("installed");
        let probe_key = r"Software\OuttoTest_versioned";
        let guard_key = r"Software\OuttoTest_versioned_guard";
        let _ = fs::remove_dir_all(&test_dir);
        let _ = actions::registry::delete_key("HKCU", probe_key);
        let _ = delete_hkcu_tree(guard_key);

        let make = |version: &str| {
            let source = test_dir.join(format!("source-{version}"));
            fs::create_dir_all(&source).unwrap();
            fs::copy(system_dll(), source.join("a.dll")).unwrap();
            fs::write(source.join("b.dat"), version).unwrap();
            let toml = format!(
                r##"
[package]
id = "com.test.versioned"
name = "VersionedTest"
version = "{version}"

[reboot]
restart_manager = false

[upgrade]
downgrade = "refuse"
prune = ["#{{app}}"]

[upgrade.installed_version]
registry_key = 'HKCU\{probe_key}'
from = "parent_dir_name"

[[uninstall.refuse_while]]
registry_key = 'HKCU\{guard_key}\*\*'
message = "Something still uses VersionedTest."

[[files]]
source = "*"
dest = "#{{app}}/#{{package.version}}"
overwrite = "never"

[[registry]]
root = "hkcu"
key = '{probe_key}'
values = [{{ name = "", type = "string", data = '#{{app}}\#{{package.version}}\a.dll' }}]
"##
            );
            (Config::from_toml(&toml).unwrap(), source)
        };
        let callbacks = TestCallbacks::default();
        let install_version = |version: &str| {
            let (config, source_dir) = make(version);
            install(
                &config,
                &InstallOptions {
                    source_dir,
                    install_dir: Some(install_dir.clone()),
                    selected_components: None,
                    uninstall_exe: Some(fake_uninstaller()),
                },
                &callbacks,
            )
        };
        let probe = || {
            actions::registry::read_string(&format!(r"HKCU\{probe_key}"), "")
                .unwrap()
                .unwrap_or_default()
        };

        // What an older installer left: another version and its uninstaller.
        fs::create_dir_all(install_dir.join("0.9.0")).unwrap();
        fs::write(install_dir.join(r"0.9.0\a.dll"), "old").unwrap();
        fs::write(install_dir.join("unins000.dat"), "old").unwrap();

        install_version("1.0.0").unwrap();
        assert!(install_dir.join(r"1.0.0\a.dll").is_file());
        assert!(!install_dir.join("0.9.0").exists());
        assert!(!install_dir.join("unins000.dat").exists());
        assert!(probe().ends_with(r"\1.0.0\a.dll"), "{}", probe());

        // A process keeps 1.0.0's DLL loaded through the upgrade.
        let module = load_library(&install_dir.join(r"1.0.0\a.dll"));
        install_version("2.0.0").unwrap();
        assert!(!install_dir.join("1.0.0").exists(), "{:?}", callbacks.logs);
        assert!(install_dir.join(r"2.0.0\a.dll").is_file());
        assert!(probe().ends_with(r"\2.0.0\a.dll"));
        unsafe { windows_sys::Win32::Foundation::FreeLibrary(module) };

        // The same version again keeps its files, and owns them still.
        let module = load_library(&install_dir.join(r"2.0.0\a.dll"));
        install_version("2.0.0").unwrap();
        assert!(install_dir.join(r"2.0.0\a.dll").is_file());
        let manifest =
            InstallManifest::<WindowsAction>::load(&install_dir, "com.test.versioned").unwrap();
        let copied: Vec<String> = manifest
            .actions
            .iter()
            .filter_map(|a| match a {
                WindowsAction::FileCopied { dest, .. } => Some(dest.display().to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(copied.len(), 2, "{copied:?}");

        // An older version is refused before anything changes.
        let err = install_version("1.0.0").unwrap_err();
        assert!(matches!(err, InstallerError::UpgradeConflict(_)), "{err}");
        assert!(!install_dir.join("1.0.0").exists());
        assert!(probe().ends_with(r"\2.0.0\a.dll"));
        assert_eq!(
            InstallManifest::<WindowsAction>::load(&install_dir, "com.test.versioned")
                .unwrap()
                .package_version,
            "2.0.0"
        );

        // Uninstall is refused while a key matches the guard.
        create_hkcu_key(&format!(
            r"{guard_key}\0x00000409\{{2CC0C11A-EFB4-5241-A262-330A9475C109}}"
        ));
        let err = uninstall_package(&install_dir, "com.test.versioned", &callbacks).unwrap_err();
        assert!(
            matches!(&err, InstallerError::UninstallRefused(m) if m == "Something still uses VersionedTest."),
            "{err}"
        );
        assert!(install_dir.join(r"2.0.0\a.dll").is_file());
        delete_hkcu_tree(guard_key).unwrap();
        uninstall::check_uninstall_allowed(&install_dir, "com.test.versioned").unwrap();

        // The loaded DLL is moved aside on uninstall too.
        uninstall_package(&install_dir, "com.test.versioned", &callbacks).unwrap();
        assert!(!install_dir.join("2.0.0").exists(), "{:?}", callbacks.logs);
        assert!(
            actions::registry::read_string(&format!(r"HKCU\{probe_key}"), "")
                .unwrap()
                .is_none()
        );
        unsafe { windows_sys::Win32::Foundation::FreeLibrary(module) };

        let _ = actions::registry::delete_key("HKCU", probe_key);
        let _ = fs::remove_dir_all(&test_dir);
    }

    /// `legacy_inno_mode = "adopt"` takes over an Inno Setup install
    /// without running its uninstaller, which here is not even a program.
    #[test]
    fn test_adopt_legacy_inno_install() {
        let test_dir = std::env::temp_dir().join("outto_test_adopt_inno");
        let install_dir = test_dir.join("installed");
        let source_dir = test_dir.join("source");
        let guid = "7B1F5C0E-2D3A-4E8B-9C6D-0A1B2C3D4E5F";
        let entry = format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{{{guid}}}_is1");
        let _ = fs::remove_dir_all(&test_dir);
        let _ = actions::registry::delete_key("HKCU", &entry);
        fs::create_dir_all(install_dir.join("0.9.0")).unwrap();
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(source_dir.join("a.dll"), "new").unwrap();
        for name in [
            "unins000.exe",
            "unins000.dat",
            "unins000.msg",
            r"0.9.0\a.dll",
        ] {
            fs::write(install_dir.join(name), "not a program").unwrap();
        }
        create_hkcu_key(&entry);
        let uninstaller = install_dir.join("unins000.exe");
        actions::registry::set_string_value(
            "HKCU",
            &entry,
            "UninstallString",
            &format!("\"{}\"", uninstaller.display()),
        )
        .unwrap();
        actions::registry::set_string_value("HKCU", &entry, "DisplayVersion", "0.9.0").unwrap();
        actions::registry::set_string_value(
            "HKCU",
            &entry,
            "InstallLocation",
            &install_dir.display().to_string(),
        )
        .unwrap();

        let config = Config::from_toml(&format!(
            r##"
[package]
id = "com.test.adopt"
name = "AdoptTest"
version = "1.0.0"
legacy_inno_app_id = "{guid}"
legacy_inno_mode = "adopt"

[upgrade]
downgrade = "refuse"
prune = ["#{{app}}"]

[[files]]
source = "*"
dest = "#{{app}}/#{{package.version}}"
"##
        ))
        .unwrap();
        let callbacks = TestCallbacks::default();
        install(
            &config,
            &InstallOptions {
                source_dir,
                install_dir: Some(install_dir.clone()),
                selected_components: None,
                uninstall_exe: Some(fake_uninstaller()),
            },
            &callbacks,
        )
        .unwrap();
        assert!(!hkcu_key_exists(&entry));
        for name in ["unins000.exe", "unins000.dat", "unins000.msg", "0.9.0"] {
            assert!(
                !install_dir.join(name).exists(),
                "{name}: {:?}",
                callbacks.logs
            );
        }
        assert!(install_dir.join(r"1.0.0\a.dll").is_file());

        uninstall_package(&install_dir, "com.test.adopt", &callbacks).unwrap();
        let _ = fs::remove_dir_all(&test_dir);
    }

    /// An Inno Setup install newer than the package refuses it under
    /// `downgrade = "refuse"` without the installed-version probe.
    #[test]
    fn test_newer_legacy_inno_install_is_not_downgraded() {
        let guid = "1C2D3E4F-5A6B-4C7D-8E9F-A0B1C2D3E4F5";
        let entry = format!(r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{guid}_is1");
        let test_dir = std::env::temp_dir().join("outto_test_inno_newer");
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&test_dir).unwrap();
        create_hkcu_key(&entry);
        actions::registry::set_string_value("HKCU", &entry, "DisplayVersion", "2.0.0").unwrap();
        let config = Config::from_toml(&format!(
            "[package]\nid = \"com.test.inno-newer\"\nname = \"InnoNewer\"\nversion = \"1.0.0\"\n\
             legacy_inno_app_id = \"{guid}\"\n[upgrade]\ndowngrade = \"refuse\"\n"
        ))
        .unwrap();
        let err = install(
            &config,
            &InstallOptions {
                source_dir: test_dir.clone(),
                install_dir: Some(test_dir.join("installed")),
                selected_components: None,
                uninstall_exe: Some(fake_uninstaller()),
            },
            &TestCallbacks::default(),
        )
        .unwrap_err();
        assert!(matches!(err, InstallerError::UpgradeConflict(_)), "{err}");
        assert!(hkcu_key_exists(&entry));
        assert!(!test_dir.join("installed").exists());
        actions::registry::delete_key("HKCU", &entry).unwrap();
        let _ = fs::remove_dir_all(&test_dir);
    }

    fn create_hkcu_key(key: &str) {
        use windows_sys::Win32::System::Registry::*;
        let wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey: HKEY = std::ptr::null_mut();
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(r, 0);
        unsafe { RegCloseKey(hkey) };
    }

    fn delete_hkcu_tree(key: &str) -> Result<(), u32> {
        use windows_sys::Win32::System::Registry::*;
        let wide: Vec<u16> = key.encode_utf16().chain(std::iter::once(0)).collect();
        match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, wide.as_ptr()) } {
            0 => Ok(()),
            e => Err(e),
        }
    }
}
