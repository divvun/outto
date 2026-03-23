use std::fs;
use std::path::Path;

use crate::error::{InstallerError, InstallerResult};
use crate::manifest::rollback::rollback_actions;
use crate::manifest::InstallManifest;
use crate::{InstallerCallbacks, LogLevel};

pub fn uninstall(install_dir: &Path, callbacks: &dyn InstallerCallbacks) -> InstallerResult<()> {
    callbacks.on_log(LogLevel::Info, "Starting uninstallation");
    callbacks.on_progress("uninstall", 0, 1);

    let manifest = InstallManifest::load(install_dir).map_err(|e| {
        InstallerError::Manifest(format!(
            "failed to load manifest from {}: {e}",
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

    // Replay actions in reverse
    rollback_actions(&manifest.actions, callbacks, false)?;

    // Remove from Add/Remove Programs
    crate::detect::remove_uninstall_registry(&manifest.package_id)?;

    callbacks.on_log(LogLevel::Info, "Uninstallation complete");
    callbacks.on_progress("uninstall", 1, 1);

    Ok(())
}
