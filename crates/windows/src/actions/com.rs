use std::path::Path;

use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{ComAction, ComEntry, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

pub fn register_com(
    entry: &ComEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let file = resolver.resolve(&entry.file)?;
    let action_str = match entry.action {
        ComAction::Regserver => "regserver",
        ComAction::Typelib => "typelib",
    };

    callbacks.on_log(
        LogLevel::Info,
        &format!("COM: registering {action_str} {file}"),
    );

    match entry.action {
        ComAction::Regserver => register_dll(&file)?,
        ComAction::Typelib => register_typelib(&file)?,
    }

    manifest.record(Action::ComRegistered {
        file: file.into(),
        action: action_str.to_string(),
    });

    Ok(())
}

fn register_dll(dll_path: &str) -> InstallerResult<()> {
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

fn register_typelib(tlb_path: &str) -> InstallerResult<()> {
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

pub fn unregister(file: &Path, action: &str) -> InstallerResult<()> {
    let file_str = file.to_string_lossy();
    match action {
        "regserver" | "typelib" => {
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
        _ => {}
    }
    Ok(())
}
