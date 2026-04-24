pub mod environment;
pub mod files_ext;
pub mod fonts;
pub mod launchd;
pub mod plist;
pub mod symlinks;

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use outto_core::actions::{files as core_files, prerequisites, run};
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{RunPhase as CoreRunPhase, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{CoreAction, InstallManifest};

use crate::config::{Config, FileEntry, RunEntry, RunPhase};
use crate::manifest::Action;

/// Run the macOS install pipeline: cleanup → before_install → dirs → files
/// → symlinks → plist → launchd → fonts → environment → associations → after_install.
pub fn execute_install(
    config: &Config,
    source_dir: &Path,
    selected_components: &Option<HashSet<String>>,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    execute_install_cleanup(config, resolver, callbacks)?;
    execute_run_phase(
        &config.run,
        &RunPhase::BeforeInstall,
        resolver,
        manifest,
        callbacks,
    )?;

    for dir in &config.dirs {
        if !should_include(dir.component.as_deref(), selected_components) {
            continue;
        }
        install_dir(dir, resolver, manifest, callbacks)?;
    }

    for file in &config.files {
        if !should_include(file.component.as_deref(), selected_components) {
            continue;
        }
        install_file_entry(file, source_dir, resolver, manifest, callbacks)?;
    }

    for sym in &config.symlinks {
        if !should_include(sym.component.as_deref(), selected_components) {
            continue;
        }
        symlinks::create_symlink(sym, resolver, manifest, callbacks)?;
    }

    for p in &config.plist {
        if !should_include(p.component.as_deref(), selected_components) {
            continue;
        }
        plist::apply_plist_entry(p, resolver, manifest, callbacks)?;
    }

    for l in &config.launchd {
        if !should_include(l.component.as_deref(), selected_components) {
            continue;
        }
        launchd::install_launchd_entry(l, resolver, manifest, callbacks)?;
    }

    for f in &config.fonts {
        if !should_include(f.component.as_deref(), selected_components) {
            continue;
        }
        fonts::install_font(f, source_dir, manifest, callbacks)?;
    }

    for env in &config.environment {
        if !should_include(env.component.as_deref(), selected_components) {
            continue;
        }
        environment::apply_environment_entry(
            env,
            &config.package.id,
            resolver,
            manifest,
            callbacks,
        )?;
    }

    for assoc in &config.associations {
        if !should_include(assoc.component.as_deref(), selected_components) {
            continue;
        }
        if assoc.lsregister {
            run_lsregister(&assoc.app_path, resolver, manifest, callbacks)?;
        }
    }

    execute_run_phase(
        &config.run,
        &RunPhase::AfterInstall,
        resolver,
        manifest,
        callbacks,
    )?;

    Ok(())
}

/// Map each macos RunEntry into core's shape and delegate to core's phase runner.
fn execute_run_phase(
    entries: &[RunEntry],
    phase: &RunPhase,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for entry in entries.iter().filter(|e| &e.phase == phase) {
        let core_phase = match entry.phase {
            RunPhase::BeforeInstall => CoreRunPhase::BeforeInstall,
            RunPhase::AfterInstall => CoreRunPhase::AfterInstall,
            RunPhase::BeforeUninstall => CoreRunPhase::BeforeUninstall,
            RunPhase::AfterUninstall => CoreRunPhase::AfterUninstall,
        };
        let core_entry = outto_core::config::RunEntry {
            phase: core_phase,
            command: entry.command.clone(),
            arguments: entry.arguments.clone(),
            wait: entry.wait,
            show: outto_core::config::ShowWindow::Normal,
            component: entry.component.clone(),
            working_dir: entry.working_dir.clone(),
            arch: None,
            run_as_original_user: false,
        };

        run::execute_phase_commands(
            std::slice::from_ref(&core_entry),
            &core_entry.phase,
            resolver,
            manifest,
            callbacks,
        )?;
    }
    Ok(())
}

/// macOS-flavoured directory install: create the directory, apply POSIX mode
/// and chown if the TOML specified them.
fn install_dir(
    entry: &crate::config::DirEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let path = resolver.resolve_path(&entry.path)?;
    if !path.exists() {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Dirs: creating {}", path.display()),
        );
        std::fs::create_dir_all(&path).map_err(|e| InstallerError::DirOp {
            path: path.clone(),
            source: e,
        })?;
        manifest.record(CoreAction::DirectoryCreated { path: path.clone() });
    }

    if entry.permissions.is_some() || entry.owner.is_some() {
        apply_posix_mode_and_owner(&path, entry, manifest, callbacks)?;
    }
    Ok(())
}

fn apply_posix_mode_and_owner(
    path: &Path,
    entry: &crate::config::DirEntry,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    if let Some(ref mode) = entry.permissions {
        let status = Command::new("chmod")
            .arg(mode)
            .arg(path)
            .status()
            .map_err(|e| InstallerError::Other(format!("chmod failed to launch: {e}")))?;
        if !status.success() {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("chmod {mode} {} returned {status}", path.display()),
            );
        }
    }
    if let Some(ref owner) = entry.owner {
        let status = Command::new("chown")
            .arg(owner)
            .arg(path)
            .status()
            .map_err(|e| InstallerError::Other(format!("chown failed to launch: {e}")))?;
        if !status.success() {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("chown {owner} {} returned {status}", path.display()),
            );
        }
    }
    manifest.record(Action::PermissionsSet {
        path: path.to_path_buf(),
        mode: entry.permissions.clone().unwrap_or_default(),
        owner: entry.owner.clone(),
    });
    Ok(())
}

/// File-entry install: uses `ditto` for bundle = true, otherwise hands off to
/// core's `install_files`.
fn install_file_entry(
    entry: &FileEntry,
    source_dir: &Path,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    if entry.bundle {
        return files_ext::install_bundle(entry, source_dir, resolver, manifest, callbacks);
    }

    let core_entry = outto_core::config::FileEntry {
        source: entry.source.clone(),
        dest: entry.dest.clone(),
        overwrite: map_overwrite(&entry.overwrite),
        component: entry.component.clone(),
        arch: None,
        dest_name: entry.dest_name.clone(),
        excludes: entry.excludes.clone(),
        attribs: None,
        permissions: Vec::new(),
        hash: entry.hash.clone(),
        skip_if_missing: entry.skip_if_missing,
        delete_after_install: entry.delete_after_install,
        touch: false,
        overwrite_readonly: false,
        only_if_dest_exists: entry.only_if_dest_exists,
        preserve_on_uninstall: entry.preserve_on_uninstall,
        uninst_remove_readonly: false,
        uninst_restart_delete: false,
        restart_replace: false,
        set_ntfs_compression: None,
        codesign: entry.codesign,
    };
    core_files::install_files(&core_entry, source_dir, resolver, manifest, callbacks)
}

fn map_overwrite(p: &crate::config::OverwritePolicy) -> outto_core::config::OverwritePolicy {
    use crate::config::OverwritePolicy as M;
    use outto_core::config::OverwritePolicy as C;
    match p {
        M::Always => C::Always,
        M::Never => C::Never,
        M::IfNewer => C::IfNewer,
        M::Prompt => C::Prompt,
        M::IgnoreVersion => C::IgnoreVersion,
        M::ReplaceSameVersion => C::ReplaceSameVersion,
        M::PromptIfOlder => C::PromptIfOlder,
    }
}

fn execute_install_cleanup(
    config: &Config,
    resolver: &VariableResolver,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for path_str in &config.install_cleanup.delete_paths {
        if let Ok(path) = resolver.resolve_path(path_str) {
            if path.exists() {
                callbacks.on_log(
                    LogLevel::Info,
                    &format!("Cleanup: deleting {}", path.display()),
                );
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    for id in &config.install_cleanup.uninstall_ids {
        if let Ok(Some(_existing)) = crate::detect::detect_existing_install(id) {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Cleanup: uninstalling existing {id}"),
            );
            let _ = crate::uninstall::uninstall(id, callbacks);
        }
    }

    Ok(())
}

fn run_lsregister(
    app_path: &str,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let resolved = resolver.resolve_path(app_path)?;
    let lsregister = "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister";
    callbacks.on_log(
        LogLevel::Info,
        &format!("lsregister: -f -r {}", resolved.display()),
    );
    let status = Command::new(lsregister)
        .args(["-f", "-r"])
        .arg(&resolved)
        .status();
    match status {
        Ok(s) if s.success() => {
            manifest.record(Action::LsregisterRan { app_path: resolved });
            Ok(())
        }
        Ok(s) => {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("lsregister returned {s} (non-fatal)"),
            );
            Ok(())
        }
        Err(e) => {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("lsregister failed to launch: {e} (non-fatal)"),
            );
            Ok(())
        }
    }
}

pub fn check_prerequisites(
    config: &Config,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for entry in &config.prerequisites {
        if let Some(ref min) = entry.check.min_macos_version {
            if !macos_version_at_least(min)? {
                return Err(InstallerError::Prerequisite {
                    name: format!("{} (needs macOS {min}+)", entry.name),
                });
            }
        }
    }

    let core_entries: Vec<outto_core::config::PrerequisiteEntry> = config
        .prerequisites
        .iter()
        .map(|e| outto_core::config::PrerequisiteEntry {
            name: e.name.clone(),
            check: outto_core::config::PrerequisiteCheck {
                registry: None,
                value: None,
                equals: None,
                file: e.check.file.clone(),
                command: e.check.command.clone(),
            },
            download_url: e.download_url.clone(),
            installer: e.installer.clone(),
            arguments: e.arguments.clone(),
            required: e.required,
        })
        .collect();

    prerequisites::check_prerequisites(&core_entries, callbacks)
}

fn macos_version_at_least(min: &str) -> InstallerResult<bool> {
    let Ok(min_sem) = semver::Version::parse(&ensure_semver(min)) else {
        return Ok(true);
    };
    let current = current_macos_version()?;
    let Ok(cur_sem) = semver::Version::parse(&ensure_semver(&current)) else {
        return Ok(true);
    };
    Ok(cur_sem >= min_sem)
}

fn ensure_semver(v: &str) -> String {
    if v.matches('.').count() == 1 {
        format!("{v}.0")
    } else {
        v.to_string()
    }
}

fn current_macos_version() -> InstallerResult<String> {
    let out = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .map_err(|e| InstallerError::Other(format!("sw_vers failed to launch: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn should_include(
    entry_component: Option<&str>,
    selected_components: &Option<HashSet<String>>,
) -> bool {
    match (entry_component, selected_components) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(comp), Some(selected)) => selected.contains(comp),
    }
}
