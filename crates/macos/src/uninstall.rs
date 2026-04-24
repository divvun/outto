//! Uninstall entry point and cascade detection for the macOS backend.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{rollback::rollback_actions, InstallManifest};

use crate::detect::{self, InstalledPackageInfo};
use crate::manifest::Action;

/// Uninstall a single package by id (plus every package that depends on it).
pub fn uninstall(package_id: &str, callbacks: &dyn InstallerCallbacks) -> InstallerResult<()> {
    callbacks.on_log(LogLevel::Info, "Starting uninstallation");
    callbacks.on_progress("uninstall", 0, 1);

    // Find the receipt. Try user scope first, then system.
    let (base, scope) = find_receipt_base(package_id).ok_or_else(|| {
        InstallerError::Manifest(format!("no outto receipt found for {package_id}"))
    })?;

    let manifest = InstallManifest::<Action>::load_from_base(&base, package_id).map_err(|e| {
        InstallerError::Manifest(format!(
            "failed to load manifest for {package_id} at {}: {e}",
            base.display()
        ))
    })?;

    let dependents = collect_cascade_order(package_id);
    if !dependents.is_empty() {
        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Cascade: {} dependent package(s) will be uninstalled first",
                dependents.len()
            ),
        );
    }
    for dep in &dependents {
        if let Err(e) = uninstall(&dep.package_id, callbacks) {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Cascade: failed to uninstall {}: {e}", dep.package_id),
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
    detect::remove_receipt(&base, package_id)?;

    let _ = scope; // retained for future logging
    callbacks.on_log(LogLevel::Info, "Uninstallation complete");
    callbacks.on_progress("uninstall", 1, 1);

    Ok(())
}

fn find_receipt_base(package_id: &str) -> Option<(PathBuf, &'static str)> {
    if let Some(user) = detect::user_receipt_base() {
        if user.join(package_id).join("receipt.json").exists() {
            return Some((user, "user"));
        }
    }
    let sys = detect::system_receipt_base();
    if sys.join(package_id).join("receipt.json").exists() {
        return Some((sys, "system"));
    }
    None
}

/// Return packages that depend on `target_id`, in leaf-first topological order.
pub fn collect_cascade_order(target_id: &str) -> Vec<InstalledPackageInfo> {
    let all = detect::enumerate_outto_packages();

    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    let mut pkg_map: HashMap<String, InstalledPackageInfo> = HashMap::new();
    for pkg in all {
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

    while let Some(cur) = queue.pop_front() {
        if let Some(dependents) = reverse_deps.get(&cur) {
            for d in dependents {
                if to_uninstall.insert(d.clone()) {
                    queue.push_back(d.clone());
                }
            }
        }
    }

    let mut ordered: Vec<InstalledPackageInfo> = Vec::new();
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
