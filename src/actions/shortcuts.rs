use std::path::PathBuf;

use crate::config::{PathResolver, ShortcutEntry, ShortcutLocation};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel};

pub fn create_shortcut(
    entry: &ShortcutEntry,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let target = resolver.resolve(&entry.target)?;
    let name = resolver.resolve(&entry.name)?;

    let location_dir = match entry.location {
        ShortcutLocation::StartMenu => resolver
            .get_variable("startmenu")
            .ok_or_else(|| InstallerError::Shortcut {
                name: name.clone(),
                message: "startmenu path variable not set".into(),
            })?
            .to_string(),
        ShortcutLocation::Desktop => resolver
            .get_variable("desktop")
            .ok_or_else(|| InstallerError::Shortcut {
                name: name.clone(),
                message: "desktop path variable not set".into(),
            })?
            .to_string(),
        ShortcutLocation::Startup => resolver
            .get_variable("startup")
            .ok_or_else(|| InstallerError::Shortcut {
                name: name.clone(),
                message: "startup path variable not set".into(),
            })?
            .to_string(),
    };

    let shortcut_path = PathBuf::from(&location_dir).join(format!("{name}.lnk"));

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Creating shortcut: {} -> {}",
            shortcut_path.display(),
            target
        ),
    );

    // Ensure directory exists
    if let Some(parent) = shortcut_path.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| InstallerError::DirOp {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
    }

    let arguments = entry
        .arguments
        .as_deref()
        .map(|a| resolver.resolve(a))
        .transpose()?;
    let working_dir = entry
        .working_dir
        .as_deref()
        .map(|w| resolver.resolve(w))
        .transpose()?;
    let icon = entry
        .icon
        .as_deref()
        .map(|i| resolver.resolve(i))
        .transpose()?;

    #[cfg(windows)]
    {
        create_shortcut_powershell(
            &shortcut_path,
            &target,
            arguments.as_deref(),
            working_dir.as_deref(),
            icon.as_deref(),
            entry.description.as_deref(),
        )?;
    }

    #[cfg(not(windows))]
    {
        // Create a placeholder .lnk file for testing on non-Windows
        std::fs::write(&shortcut_path, format!("target={target}")).map_err(|e| {
            InstallerError::FileOp {
                path: shortcut_path.clone(),
                source: e,
            }
        })?;
    }

    manifest.record(ActionRecord::ShortcutCreated {
        path: shortcut_path,
    });

    Ok(())
}

#[cfg(windows)]
fn create_shortcut_powershell(
    shortcut_path: &std::path::Path,
    target: &str,
    arguments: Option<&str>,
    working_dir: Option<&str>,
    icon: Option<&str>,
    description: Option<&str>,
) -> InstallerResult<()> {
    let mut script = format!(
        "$ws = New-Object -ComObject WScript.Shell; \
         $s = $ws.CreateShortcut('{}'); \
         $s.TargetPath = '{}';",
        shortcut_path.to_string_lossy().replace('\'', "''"),
        target.replace('\'', "''"),
    );

    if let Some(args) = arguments {
        script.push_str(&format!(" $s.Arguments = '{}';", args.replace('\'', "''")));
    }
    if let Some(wd) = working_dir {
        script.push_str(&format!(
            " $s.WorkingDirectory = '{}';",
            wd.replace('\'', "''")
        ));
    }
    if let Some(ico) = icon {
        script.push_str(&format!(
            " $s.IconLocation = '{}';",
            ico.replace('\'', "''")
        ));
    }
    if let Some(desc) = description {
        script.push_str(&format!(
            " $s.Description = '{}';",
            desc.replace('\'', "''")
        ));
    }

    script.push_str(" $s.Save()");

    let output = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| InstallerError::Shortcut {
            name: shortcut_path.to_string_lossy().into(),
            message: format!("failed to run powershell: {e}"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(InstallerError::Shortcut {
            name: shortcut_path.to_string_lossy().into(),
            message: format!("powershell shortcut creation failed: {stderr}"),
        });
    }

    Ok(())
}
