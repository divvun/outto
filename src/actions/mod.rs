pub mod associations;
pub mod com;
pub mod dirs;
pub mod environment;
pub mod files;
pub mod fonts;
pub mod prerequisites;
pub mod registry;
pub mod run;
pub mod services;
pub mod shortcuts;

use std::collections::HashSet;
use std::path::Path;

use crate::config::*;
use crate::error::InstallerResult;
use crate::manifest::InstallManifest;
use crate::InstallerCallbacks;

pub fn execute_install(
    config: &Config,
    source_dir: &Path,
    selected_components: &Option<HashSet<String>>,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    // Phase: install cleanup (pre-install)
    execute_install_cleanup(&config.install_cleanup, resolver, callbacks)?;

    // Phase: before_install commands
    run::execute_phase_commands(&config.run, &RunPhase::BeforeInstall, resolver, manifest, callbacks)?;

    // Create directories
    let total_dirs = config.dirs.len();
    for (i, dir) in config.dirs.iter().enumerate() {
        if !should_include(dir.component.as_deref(), selected_components) {
            continue;
        }
        if !arch_matches_entry(dir.arch.as_ref()) {
            continue;
        }
        callbacks.on_progress("directories", i as u64, total_dirs as u64);
        dirs::create_directory(dir, resolver, manifest, callbacks)?;
    }

    // Copy files
    let total_files = config.files.len();
    for (i, file) in config.files.iter().enumerate() {
        if !should_include(file.component.as_deref(), selected_components) {
            continue;
        }
        if !arch_matches_entry(file.arch.as_ref()) {
            continue;
        }
        callbacks.on_progress("files", i as u64, total_files as u64);
        files::install_files(file, source_dir, resolver, manifest, callbacks)?;
    }

    // Registry entries
    let total_reg = config.registry.len();
    for (i, reg) in config.registry.iter().enumerate() {
        if !should_include(reg.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("registry", i as u64, total_reg as u64);
        registry::apply_registry_entry(reg, resolver, manifest, callbacks)?;
    }

    // Shortcuts
    let total_shortcuts = config.shortcuts.len();
    for (i, shortcut) in config.shortcuts.iter().enumerate() {
        if !should_include(shortcut.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("shortcuts", i as u64, total_shortcuts as u64);
        shortcuts::create_shortcut(shortcut, resolver, manifest, callbacks)?;
    }

    // Environment variables
    let total_env = config.environment.len();
    for (i, env) in config.environment.iter().enumerate() {
        if !should_include(env.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("environment", i as u64, total_env as u64);
        environment::apply_env_entry(env, resolver, manifest, callbacks)?;
    }

    // Services
    let total_services = config.services.len();
    for (i, svc) in config.services.iter().enumerate() {
        if !should_include(svc.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("services", i as u64, total_services as u64);
        services::install_service(svc, resolver, manifest, callbacks)?;
    }

    // File associations
    let total_assoc = config.associations.len();
    for (i, assoc) in config.associations.iter().enumerate() {
        if !should_include(assoc.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("associations", i as u64, total_assoc as u64);
        associations::create_association(assoc, resolver, manifest, callbacks)?;
    }

    // COM registration
    let total_com = config.com.len();
    for (i, com_entry) in config.com.iter().enumerate() {
        if !should_include(com_entry.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("com", i as u64, total_com as u64);
        com::register_com(com_entry, resolver, manifest, callbacks)?;
    }

    // Fonts
    let total_fonts = config.fonts.len();
    for (i, font) in config.fonts.iter().enumerate() {
        if !should_include(font.component.as_deref(), selected_components) {
            continue;
        }
        callbacks.on_progress("fonts", i as u64, total_fonts as u64);
        fonts::install_font(font, source_dir, manifest, callbacks)?;
    }

    // Phase: after_install commands
    run::execute_phase_commands(&config.run, &RunPhase::AfterInstall, resolver, manifest, callbacks)?;

    Ok(())
}

fn arch_matches_entry(arch: Option<&Architecture>) -> bool {
    match arch {
        None => true,
        Some(a) => crate::detect::arch_matches(a),
    }
}

fn execute_install_cleanup(
    cleanup: &InstallCleanup,
    resolver: &PathResolver,
    callbacks: &dyn crate::InstallerCallbacks,
) -> InstallerResult<()> {
    use crate::LogLevel;

    // Delete paths
    for path_str in &cleanup.delete_paths {
        if let Ok(path) = resolver.resolve_path(path_str) {
            if path.exists() {
                callbacks.on_log(LogLevel::Info, &format!("Cleanup: deleting {}", path.display()));
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(&path);
                } else {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    // Delete registry keys
    #[cfg(windows)]
    for reg in &cleanup.delete_registry {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Cleanup: deleting registry key {:?}\\{}", reg.root, reg.key),
        );
        let _ = registry::delete_key(&format!("{:?}", reg.root).to_uppercase(), &reg.key);
    }

    // Uninstall existing installations by ID
    for id in &cleanup.uninstall_ids {
        if let Ok(Some(existing)) = crate::detect::detect_existing_install(id) {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Cleanup: uninstalling existing {} from {}", id, existing.install_dir.display()),
            );
            let _ = crate::uninstall::uninstall(&existing.install_dir, callbacks);
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
        (Some(_), None) => true, // No filtering = include everything
        (Some(comp), Some(selected)) => selected.contains(comp),
    }
}
