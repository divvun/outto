use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use crate::error::{InstallerError, InstallerResult};
use crate::manifest::rollback::rollback_actions;
use crate::manifest::InstallManifest;
use crate::{InstallerCallbacks, LogLevel};

/// Uninstall a package and all packages that depend on it (cascade).
pub fn uninstall(
    install_dir: &Path,
    package_id: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    callbacks.on_log(LogLevel::Info, "Starting uninstallation");
    callbacks.on_progress("uninstall", 0, 1);

    let manifest = InstallManifest::load(install_dir, package_id).map_err(|e| {
        InstallerError::Manifest(format!(
            "failed to load manifest for {package_id} from {}: {e}",
            install_dir.display()
        ))
    })?;

    let target_id = &manifest.package_id;

    // Find and uninstall all packages that depend on this one (cascade)
    let dependents = collect_cascade_order(target_id);
    if !dependents.is_empty() {
        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Cascade: {} dependent package(s) will be uninstalled first",
                dependents.len()
            ),
        );
    }

    for dep_info in &dependents {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Cascade: uninstalling dependent {}", dep_info.package_id),
        );
        if let Err(e) = uninstall_single(&dep_info.install_dir, &dep_info.package_id, callbacks) {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Cascade: failed to uninstall {}: {e}", dep_info.package_id),
            );
        }
    }

    // Now uninstall the target package itself
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Uninstalling {} v{} ({} recorded actions)",
            manifest.package_name,
            manifest.package_version,
            manifest.actions.len()
        ),
    );

    rollback_actions(&manifest.actions, callbacks, false)?;
    crate::detect::remove_uninstall_registry(&manifest.package_id)?;

    callbacks.on_log(LogLevel::Info, "Uninstallation complete");
    callbacks.on_progress("uninstall", 1, 1);

    Ok(())
}

/// Uninstall a single package without cascade (used by the cascade loop).
fn uninstall_single(
    install_dir: &Path,
    package_id: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let manifest = InstallManifest::load(install_dir, package_id).map_err(|e| {
        InstallerError::Manifest(format!(
            "failed to load manifest for {package_id} from {}: {e}",
            install_dir.display()
        ))
    })?;

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Uninstalling {} v{} ({} recorded actions)",
            manifest.package_name,
            manifest.package_version,
            manifest.actions.len()
        ),
    );

    rollback_actions(&manifest.actions, callbacks, false)?;
    crate::detect::remove_uninstall_registry(&manifest.package_id)?;

    Ok(())
}

/// Build the ordered list of packages that must be uninstalled before `target_id`.
/// Returns dependents in leaf-first order (topological sort).
pub fn collect_cascade_order(target_id: &str) -> Vec<crate::detect::InstalledPackageInfo> {
    let all_packages = crate::detect::enumerate_outto_packages();

    // Build reverse dependency map: package_id -> list of packages that depend on it
    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut pkg_map: HashMap<String, crate::detect::InstalledPackageInfo> = HashMap::new();

    for pkg in all_packages {
        for dep in &pkg.depends_on {
            reverse_deps
                .entry(dep.clone())
                .or_default()
                .push(pkg.package_id.clone());
        }
        pkg_map.insert(pkg.package_id.clone(), pkg);
    }

    // BFS to collect all transitive dependents of target_id
    let mut to_uninstall: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(target_id.to_string());

    while let Some(current) = queue.pop_front() {
        if let Some(dependents) = reverse_deps.get(&current) {
            for dep_id in dependents {
                if to_uninstall.insert(dep_id.clone()) {
                    queue.push_back(dep_id.clone());
                }
            }
        }
    }

    // Topological sort: leaves first (packages with no dependents in the set come first)
    let mut ordered: Vec<crate::detect::InstalledPackageInfo> = Vec::new();
    let mut remaining = to_uninstall.clone();

    while !remaining.is_empty() {
        // Find a package in `remaining` that has no dependents also in `remaining`
        let leaf = remaining
            .iter()
            .find(|id| {
                reverse_deps
                    .get(*id)
                    .map_or(true, |deps| deps.iter().all(|d| !remaining.contains(d)))
            })
            .cloned();

        match leaf {
            Some(id) => {
                remaining.remove(&id);
                if let Some(info) = pkg_map.remove(&id) {
                    ordered.push(info);
                }
            }
            None => {
                // Cycle detected — just drain remaining to avoid infinite loop
                for id in remaining.drain() {
                    if let Some(info) = pkg_map.remove(&id) {
                        ordered.push(info);
                    }
                }
            }
        }
    }

    ordered
}
