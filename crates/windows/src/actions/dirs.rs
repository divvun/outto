use std::path::Path;

use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::DirEntry;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

/// Apply Windows ACLs (`icacls`) declared in a `DirEntry`'s `permissions` list.
///
/// Directory creation itself happens in `outto_core::actions::dirs::create_directory`.
/// This is the Windows-only follow-up that grants ACL entries and records them.
pub fn apply_permissions(
    entry: &DirEntry,
    path: &Path,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for perm in &entry.permissions {
        apply_permission(path, &perm.identity, &perm.access, manifest, callbacks)?;
    }
    Ok(())
}

fn apply_permission(
    path: &Path,
    identity: &str,
    access: &str,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Dirs: setting permissions {}:{} on {}",
            identity,
            access,
            path.display()
        ),
    );

    let access_flag = match access {
        "full" => "(OI)(CI)F",
        "modify" => "(OI)(CI)M",
        "read" => "(OI)(CI)R",
        "write" => "(OI)(CI)W",
        "read_execute" => "(OI)(CI)RX",
        _ => {
            return Err(InstallerError::Other(format!(
                "unknown access level: {access}"
            )));
        }
    };

    let output = std::process::Command::new("icacls")
        .arg(path.as_os_str())
        .arg("/grant")
        .arg(format!("{identity}:{access_flag}"))
        .output()
        .map_err(|e| InstallerError::DirOp {
            path: path.to_path_buf(),
            source: e,
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        callbacks.on_log(LogLevel::Warn, &format!("Dirs: icacls warning: {stderr}"));
    }

    manifest.record(Action::PermissionsSet {
        path: path.to_path_buf(),
        identity: identity.to_string(),
        access: access.to_string(),
    });

    Ok(())
}
