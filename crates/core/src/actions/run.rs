use std::process::Command;

use crate::callbacks::{InstallerCallbacks, LogLevel};
#[cfg(windows)]
use crate::config::ShowWindow;
use crate::config::{RunEntry, RunPhase, VariableResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{CoreAction, InstallManifest};

pub fn execute_phase_commands<A>(
    entries: &[RunEntry],
    phase: &RunPhase,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<A>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()>
where
    A: From<CoreAction>,
{
    for entry in entries.iter().filter(|e| &e.phase == phase) {
        execute_command(entry, resolver, manifest, callbacks)?;
    }
    Ok(())
}

fn execute_command<A>(
    entry: &RunEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<A>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()>
where
    A: From<CoreAction>,
{
    let command = resolver.resolve(&entry.command)?;
    let arguments = entry
        .arguments
        .as_deref()
        .map(|a| resolver.resolve(a))
        .transpose()?;

    let phase_str = match entry.phase {
        RunPhase::BeforeInstall => "before_install",
        RunPhase::AfterInstall => "after_install",
        RunPhase::BeforeUninstall => "before_uninstall",
        RunPhase::AfterUninstall => "after_uninstall",
    };

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Run: executing ({phase_str}): {} {}",
            command,
            arguments.as_deref().unwrap_or("")
        ),
    );

    let mut cmd = Command::new(&command);

    if let Some(ref args) = arguments {
        cmd.args(split_args(args));
    }

    if let Some(ref wd) = entry.working_dir {
        let resolved_wd = resolver.resolve(wd)?;
        cmd.current_dir(&resolved_wd);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let creation_flags = match entry.show {
            ShowWindow::Hidden => 0x08000000,
            _ => 0,
        };
        cmd.creation_flags(creation_flags);
    }

    if entry.wait {
        let output = cmd.output().map_err(|e| InstallerError::CommandExec {
            command: command.clone(),
            message: format!("failed to execute: {e}"),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Run: command exited with {}: {stderr}", output.status),
            );
        }
    } else {
        cmd.spawn().map_err(|e| InstallerError::CommandExec {
            command: command.clone(),
            message: format!("failed to spawn: {e}"),
        })?;
    }

    manifest.record(CoreAction::CommandExecuted {
        command: command.clone(),
        phase: phase_str.to_string(),
    });

    Ok(())
}

pub fn split_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_quote = false;
    let chars = input.chars();

    for ch in chars {
        match ch {
            '"' => in_quote = !in_quote,
            ' ' if !in_quote => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_args() {
        assert_eq!(split_args("--init"), vec!["--init"]);
        assert_eq!(
            split_args("/install /quiet /norestart"),
            vec!["/install", "/quiet", "/norestart"]
        );
        assert_eq!(
            split_args("\"hello world\" test"),
            vec!["hello world", "test"]
        );
        assert_eq!(split_args(""), Vec::<String>::new());
    }
}
