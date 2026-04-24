pub mod associations;
pub mod com;
pub mod dirs;
pub mod environment;
pub mod fonts;
pub mod registry;
pub mod services;
pub mod shortcuts;

use std::collections::HashSet;
use std::path::Path;

use outto_core::actions::{dirs as core_dirs, files, prerequisites, run};
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{Architecture, Config, InstallCleanup, RunPhase, VariableResolver};
use outto_core::error::InstallerResult;
use outto_core::manifest::InstallManifest;

use crate::manifest::Action;

use crate::detect;

/// Orchestrates the Windows install pipeline for a single `InstallManifest`.
///
/// Runs the phases in order: install cleanup → before_install → dirs → files →
/// registry → shortcuts → environment → services → associations → com → fonts →
/// after_install. Callers typically invoke [`crate::install`] rather than this
/// directly; it exists as a seam for tests.
pub fn execute_install(
    config: &Config,
    source_dir: &Path,
    selected_components: &Option<HashSet<String>>,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    execute_install_cleanup(&config.install_cleanup, resolver, callbacks)?;

    run::execute_phase_commands(
        &config.run,
        &RunPhase::BeforeInstall,
        resolver,
        manifest,
        callbacks,
    )?;

    let total_dirs = config.dirs.len();
    for (i, dir) in config.dirs.iter().enumerate() {
        if !should_include(dir.component.as_deref(), selected_components) {
            continue;
        }
        if !arch_matches_entry(dir.arch.as_ref()) {
            continue;
        }
        core_dirs::create_directory(dir, resolver, manifest, callbacks)?;
        let path = resolver.resolve_path(&dir.path)?;
        dirs::apply_permissions(dir, &path, manifest, callbacks)?;
        callbacks.on_progress("directories", (i + 1) as u64, total_dirs as u64);
    }

    let total_files = config.files.len();
    for (i, file) in config.files.iter().enumerate() {
        if !should_include(file.component.as_deref(), selected_components) {
            continue;
        }
        if !arch_matches_entry(file.arch.as_ref()) {
            continue;
        }
        files::install_files(file, source_dir, resolver, manifest, callbacks)?;
        callbacks.on_progress("files", (i + 1) as u64, total_files as u64);
    }

    let total_reg = config.registry.len();
    for (i, reg) in config.registry.iter().enumerate() {
        if !should_include(reg.component.as_deref(), selected_components) {
            continue;
        }
        registry::apply_registry_entry(reg, resolver, manifest, callbacks)?;
        callbacks.on_progress("registry", (i + 1) as u64, total_reg as u64);
    }

    let total_shortcuts = config.shortcuts.len();
    for (i, shortcut) in config.shortcuts.iter().enumerate() {
        if !should_include(shortcut.component.as_deref(), selected_components) {
            continue;
        }
        shortcuts::create_shortcut(shortcut, resolver, manifest, callbacks)?;
        callbacks.on_progress("shortcuts", (i + 1) as u64, total_shortcuts as u64);
    }

    let total_env = config.environment.len();
    for (i, env) in config.environment.iter().enumerate() {
        if !should_include(env.component.as_deref(), selected_components) {
            continue;
        }
        environment::apply_env_entry(env, resolver, manifest, callbacks)?;
        callbacks.on_progress("environment", (i + 1) as u64, total_env as u64);
    }

    let total_services = config.services.len();
    for (i, svc) in config.services.iter().enumerate() {
        if !should_include(svc.component.as_deref(), selected_components) {
            continue;
        }
        services::install_service(svc, resolver, manifest, callbacks)?;
        callbacks.on_progress("services", (i + 1) as u64, total_services as u64);
    }

    let total_assoc = config.associations.len();
    for (i, assoc) in config.associations.iter().enumerate() {
        if !should_include(assoc.component.as_deref(), selected_components) {
            continue;
        }
        associations::create_association(assoc, resolver, manifest, callbacks)?;
        callbacks.on_progress("associations", (i + 1) as u64, total_assoc as u64);
    }

    let total_com = config.com.len();
    for (i, com_entry) in config.com.iter().enumerate() {
        if !should_include(com_entry.component.as_deref(), selected_components) {
            continue;
        }
        com::register_com(com_entry, resolver, manifest, callbacks)?;
        callbacks.on_progress("com", (i + 1) as u64, total_com as u64);
    }

    let total_fonts = config.fonts.len();
    for (i, font) in config.fonts.iter().enumerate() {
        if !should_include(font.component.as_deref(), selected_components) {
            continue;
        }
        fonts::install_font(font, source_dir, manifest, callbacks)?;
        callbacks.on_progress("fonts", (i + 1) as u64, total_fonts as u64);
    }

    run::execute_phase_commands(
        &config.run,
        &RunPhase::AfterInstall,
        resolver,
        manifest,
        callbacks,
    )?;

    Ok(())
}

pub fn check_prerequisites_windows(
    config: &Config,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    prerequisites::check_prerequisites(&config.prerequisites, callbacks)
}

fn arch_matches_entry(arch: Option<&Architecture>) -> bool {
    match arch {
        None => true,
        Some(a) => detect::arch_matches(a),
    }
}

fn execute_install_cleanup(
    cleanup: &InstallCleanup,
    resolver: &VariableResolver,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for path_str in &cleanup.delete_paths {
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

    for reg in &cleanup.delete_registry {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Cleanup: deleting registry key {:?}\\{}", reg.root, reg.key),
        );
        let _ = registry::delete_key(&format!("{:?}", reg.root).to_uppercase(), &reg.key);
    }

    for id in &cleanup.uninstall_ids {
        if let Ok(Some(existing)) = detect::detect_existing_install(id) {
            callbacks.on_log(
                LogLevel::Info,
                &format!(
                    "Cleanup: uninstalling existing {} from {}",
                    id,
                    existing.install_dir.display()
                ),
            );
            let _ = crate::uninstall::uninstall(&existing.install_dir, id, callbacks);
        }
    }

    Ok(())
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
