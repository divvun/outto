use std::os::windows::process::CommandExt;
use std::path::PathBuf;

use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{Privileges, ShortcutEntry, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

/// Create `entry`'s shortcut in the folder of its location for its scope,
/// all users' by default when the package has `privileges` = `admin`, and
/// record it with any folders it created.
pub fn create_shortcut(
    entry: &ShortcutEntry,
    privileges: &Privileges,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let target = resolver.resolve(&entry.target)?;
    let name = resolver.resolve(&entry.name)?;

    let scope = entry.scope(privileges);
    let variable = entry.location.folder_variable(scope);
    let mut dir =
        PathBuf::from(
            resolver
                .get_variable(variable)
                .ok_or_else(|| InstallerError::Shortcut {
                    name: name.clone(),
                    message: format!("#{{{variable}}} is not set"),
                })?,
        );
    if let Some(subfolder) = &entry.subfolder {
        dir.push(resolver.resolve_path(subfolder)?);
    }
    let shortcut_path = dir.join(format!("{name}.lnk"));

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Shortcuts: creating {} -> {target} ({scope:?})",
            shortcut_path.display(),
        ),
    );

    if let Some(parent) = shortcut_path.parent() {
        outto_core::actions::dirs::create_dir_all_recorded(parent, manifest)?;
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

    create_shortcut_powershell(
        &shortcut_path,
        &target,
        arguments.as_deref(),
        working_dir.as_deref(),
        icon.as_deref(),
        entry.description.as_deref(),
    )?;

    manifest.record(Action::ShortcutCreated {
        path: shortcut_path,
    });

    Ok(())
}

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
        .creation_flags(CREATE_NO_WINDOW)
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
