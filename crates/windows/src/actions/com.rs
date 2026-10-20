use std::path::{Path, PathBuf};

use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{ComAction, ComEntry, ComHost, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

/// Register `entry` with the `regsvr32.exe` of its host. A failure fails the
/// install, which unregisters whatever earlier entries registered.
pub fn register_com(
    entry: &ComEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let file = resolver.resolve_path(&entry.file)?;
    let action_str = match entry.action {
        ComAction::Regserver => "regserver",
        ComAction::Typelib => "typelib",
    };
    let regsvr32 = regsvr32(&entry.host)?;

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "COM: registering {action_str} {} with {}",
            file.display(),
            regsvr32.display()
        ),
    );

    run_regsvr32(&regsvr32, &[], &file)?;

    manifest.record(Action::ComRegistered {
        file,
        action: action_str.to_string(),
        host: entry.host.clone(),
    });

    Ok(())
}

pub fn unregister(file: &Path, action: &str, host: &ComHost) -> InstallerResult<()> {
    match action {
        "regserver" | "typelib" => run_regsvr32(&regsvr32(host)?, &["/u"], file),
        _ => Ok(()),
    }
}

/// The `regsvr32.exe` of `host`, by its full path rather than through
/// `PATH`, which an elevated installer must not trust.
pub fn regsvr32(host: &ComHost) -> InstallerResult<PathBuf> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or_else(|| InstallerError::Other("SystemRoot is not set".into()))?;
    let dir = match host {
        ComHost::X86 if crate::elevation::get_system_architecture() != "x86" => "SysWOW64",
        ComHost::X86 | ComHost::Native => "System32",
    };
    Ok(root.join(dir).join("regsvr32.exe"))
}

fn run_regsvr32(regsvr32: &Path, args: &[&str], file: &Path) -> InstallerResult<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let status = std::process::Command::new(regsvr32)
        .arg("/s")
        .args(args)
        .arg(file)
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|e| InstallerError::ComRegistration {
            file: file.display().to_string(),
            message: format!("failed to run {}: {e}", regsvr32.display()),
        })?;

    if !status.success() {
        return Err(InstallerError::ComRegistration {
            file: file.display().to_string(),
            message: format!(
                "{} /s {}exited with code {}",
                regsvr32.display(),
                args.iter().map(|a| format!("{a} ")).collect::<String>(),
                status.code().unwrap_or(-1)
            ),
        });
    }

    Ok(())
}
