use std::fs;
use std::path::Path;

use crate::config::{DirEntry, PathResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel};

pub fn create_directory(
    entry: &DirEntry,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let path = resolver.resolve_path(&entry.path)?;

    if !path.exists() {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Dirs: creating {}", path.display()),
        );
        fs::create_dir_all(&path).map_err(|e| InstallerError::DirOp {
            path: path.clone(),
            source: e,
        })?;
        manifest.record(ActionRecord::DirectoryCreated { path: path.clone() });
    }

    // Apply file attributes
    if let Some(ref attribs) = entry.attribs {
        super::files::apply_attribs(&path, attribs);
    }

    // Apply permissions
    #[cfg(windows)]
    for perm in &entry.permissions {
        apply_permission(&path, &perm.identity, &perm.access, manifest, callbacks)?;
    }

    Ok(())
}

#[cfg(windows)]
fn apply_permission(
    path: &Path,
    identity: &str,
    access: &str,
    manifest: &mut InstallManifest,
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

    // Use icacls as a pragmatic approach to ACLs
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

    manifest.record(ActionRecord::PermissionsSet {
        path: path.to_path_buf(),
        identity: identity.to_string(),
        access: access.to_string(),
    });

    Ok(())
}
