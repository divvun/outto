use std::path::Path;

use crate::config::{ComAction, ComEntry, PathResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel};

pub fn register_com(
    entry: &ComEntry,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let file = resolver.resolve(&entry.file)?;
    let action_str = match entry.action {
        ComAction::Regserver => "regserver",
        ComAction::Typelib => "typelib",
    };

    callbacks.on_log(
        LogLevel::Info,
        &format!("COM registration ({action_str}): {file}"),
    );

    #[cfg(windows)]
    {
        match entry.action {
            ComAction::Regserver => register_dll(&file)?,
            ComAction::Typelib => register_typelib(&file)?,
        }
    }

    #[cfg(not(windows))]
    {
        callbacks.on_log(
            LogLevel::Info,
            &format!("  [simulated] COM {action_str} for {file}"),
        );
    }

    manifest.record(ActionRecord::ComRegistered {
        file: file.into(),
        action: action_str.to_string(),
    });

    Ok(())
}

#[cfg(windows)]
fn register_dll(dll_path: &str) -> InstallerResult<()> {
    // Use regsvr32 for reliability
    let output = std::process::Command::new("regsvr32")
        .args(["/s", dll_path])
        .output()
        .map_err(|e| InstallerError::ComRegistration {
            file: dll_path.to_string(),
            message: format!("failed to run regsvr32: {e}"),
        })?;

    if !output.status.success() {
        return Err(InstallerError::ComRegistration {
            file: dll_path.to_string(),
            message: format!("regsvr32 failed with exit code {}", output.status),
        });
    }

    Ok(())
}

#[cfg(windows)]
fn register_typelib(tlb_path: &str) -> InstallerResult<()> {
    // Use regsvr32 for type libraries too (it handles .tlb files)
    let output = std::process::Command::new("regsvr32")
        .args(["/s", tlb_path])
        .output()
        .map_err(|e| InstallerError::ComRegistration {
            file: tlb_path.to_string(),
            message: format!("failed to run regsvr32 for typelib: {e}"),
        })?;

    if !output.status.success() {
        return Err(InstallerError::ComRegistration {
            file: tlb_path.to_string(),
            message: format!("regsvr32 typelib failed with exit code {}", output.status),
        });
    }

    Ok(())
}

// Rollback helper
#[cfg(windows)]
pub fn unregister(file: &Path, action: &str) -> InstallerResult<()> {
    let file_str = file.to_string_lossy();
    match action {
        "regserver" => {
            let output = std::process::Command::new("regsvr32")
                .args(["/s", "/u", &file_str])
                .output()
                .map_err(|e| InstallerError::ComRegistration {
                    file: file_str.to_string(),
                    message: format!("failed to run regsvr32 /u: {e}"),
                })?;
            if !output.status.success() {
                return Err(InstallerError::ComRegistration {
                    file: file_str.to_string(),
                    message: "regsvr32 /u failed".into(),
                });
            }
        }
        "typelib" => {
            let output = std::process::Command::new("regsvr32")
                .args(["/s", "/u", &file_str])
                .output()
                .map_err(|e| InstallerError::ComRegistration {
                    file: file_str.to_string(),
                    message: format!("regsvr32 /u typelib failed: {e}"),
                })?;
            if !output.status.success() {
                return Err(InstallerError::ComRegistration {
                    file: file_str.to_string(),
                    message: "regsvr32 /u typelib failed".into(),
                });
            }
        }
        _ => {}
    }
    Ok(())
}
