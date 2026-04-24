use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{rollback::rollback_actions, InstallManifest};

use crate::detect;
use crate::manifest::Action as WindowsAction;

/// Uninstall a package and all packages that depend on it (cascade).
pub fn uninstall(
    install_dir: &Path,
    package_id: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    callbacks.on_log(LogLevel::Info, "Starting uninstallation");
    callbacks.on_progress("uninstall", 0, 1);

    let manifest =
        InstallManifest::<WindowsAction>::load(install_dir, package_id).map_err(|e| {
            InstallerError::Manifest(format!(
                "failed to load manifest for {package_id} from {}: {e}",
                install_dir.display()
            ))
        })?;

    let target_id = &manifest.package_id;

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
    detect::remove_uninstall_registry(&manifest.package_id)?;

    callbacks.on_log(LogLevel::Info, "Uninstallation complete");
    callbacks.on_progress("uninstall", 1, 1);

    Ok(())
}

fn uninstall_single(
    install_dir: &Path,
    package_id: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let manifest =
        InstallManifest::<WindowsAction>::load(install_dir, package_id).map_err(|e| {
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
    detect::remove_uninstall_registry(&manifest.package_id)?;

    Ok(())
}

/// Build the ordered list of packages that must be uninstalled before `target_id`.
/// Returns dependents in leaf-first order (topological sort).
pub fn collect_cascade_order(target_id: &str) -> Vec<detect::InstalledPackageInfo> {
    let all_packages = detect::enumerate_outto_packages();

    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut pkg_map: HashMap<String, detect::InstalledPackageInfo> = HashMap::new();

    for pkg in all_packages {
        for dep in &pkg.depends_on {
            reverse_deps
                .entry(dep.clone())
                .or_default()
                .push(pkg.package_id.clone());
        }
        pkg_map.insert(pkg.package_id.clone(), pkg);
    }

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

    let mut ordered: Vec<detect::InstalledPackageInfo> = Vec::new();
    let mut remaining = to_uninstall.clone();

    while !remaining.is_empty() {
        let leaf = remaining
            .iter()
            .find(|id| {
                reverse_deps
                    .get(*id)
                    .is_none_or(|deps| deps.iter().all(|d| !remaining.contains(d)))
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
