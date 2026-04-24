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
//! by self-relaunching through `osascript`. Notarization and `.app` bundle
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

use std::path::PathBuf;

use outto_core::callbacks::{InstallOptions, InstallerCallbacks, LogLevel};
use outto_core::config::VariableResolver;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{rollback::rollback_actions, CoreAction, InstallManifest};

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

    // If we need admin rights and aren't root, relaunch via osascript.
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
        callbacks.on_log(LogLevel::Info, "Elevating to admin via osascript...");
        elevation::elevate_self(&[])?;
        // elevate_self exits on success; if it returns, something unexpected happened.
        return Err(InstallerError::ElevationRequired(
            "osascript returned unexpectedly after elevation".into(),
        ));
    }

    let resolver = make_resolver(config, Some(&install_dir));

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

    std::fs::create_dir_all(&install_dir).map_err(|e| InstallerError::DirOp {
        path: install_dir.clone(),
        source: e,
    })?;

    let mut install_manifest = InstallManifest::<MacosAction>::new(
        &config.package.id,
        &config.package.name,
        &config.package.version,
        &install_dir,
        config.package.depends_on.clone(),
    );
    install_manifest.record(CoreAction::DirectoryCreated {
        path: install_dir.clone(),
    });

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

            install_manifest.save_to(&base)?;

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
                let new_files: std::collections::HashSet<PathBuf> = install_manifest
                    .actions
                    .iter()
                    .filter_map(|a| match a {
                        MacosAction::FileCopied { dest, .. } => Some(dest.clone()),
                        _ => None,
                    })
                    .collect();
                for action in &old.actions {
                    if let MacosAction::FileCopied { dest, .. } = action {
                        if !new_files.contains(dest) && dest.exists() {
                            callbacks.on_log(
                                LogLevel::Info,
                                &format!("Upgrade: removing orphaned file {}", dest.display()),
                            );
                            let _ = std::fs::remove_file(dest);
                        }
                    }
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
