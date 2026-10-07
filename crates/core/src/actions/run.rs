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

    let args = arguments.as_deref().map(split_args).unwrap_or_default();
    let working_dir = entry
        .working_dir
        .as_deref()
        .map(|wd| resolver.resolve(wd))
        .transpose()?;

    #[cfg(windows)]
    if entry.run_as_original_user {
        use super::original_user::{self, Outcome};
        match original_user::run(
            &command,
            &args,
            working_dir.as_deref(),
            &entry.show,
            entry.wait,
        )? {
            Outcome::Unavailable(reason) => callbacks.on_log(
                LogLevel::Info,
                &format!("Run: {reason}; running as the current user"),
            ),
            Outcome::Spawned => {
                callbacks.on_log(
                    LogLevel::Info,
                    &format!("Run: started {command} as the signed-in user (not waiting)"),
                );
                record(manifest, command, phase_str);
                return Ok(());
            }
            Outcome::Exited(code) => {
                handle_exit(&command, code as i32, "", manifest, callbacks);
                record(manifest, command, phase_str);
                return Ok(());
            }
        }
    }

    let mut cmd = Command::new(&command);
    cmd.args(&args);
    if let Some(ref wd) = working_dir {
        cmd.current_dir(wd);
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

        // A process killed by a signal has no code; treat it as a failure.
        let code = output.status.code().unwrap_or(-1);
        handle_exit(
            &command,
            code,
            &String::from_utf8_lossy(&output.stderr),
            manifest,
            callbacks,
        );
    } else {
        cmd.spawn().map_err(|e| InstallerError::CommandExec {
            command: command.clone(),
            message: format!("failed to spawn: {e}"),
        })?;
        callbacks.on_log(
            LogLevel::Info,
            &format!("Run: started {command} (not waiting)"),
        );
    }

    record(manifest, command, phase_str);
    Ok(())
}

fn record<A>(manifest: &mut InstallManifest<A>, command: String, phase: &str)
where
    A: From<CoreAction>,
{
    manifest.record(CoreAction::CommandExecuted {
        command,
        phase: phase.to_string(),
    });
}

/// A command's exit code is never fatal: a reboot request is noted, any other
/// failure is logged with its stderr. Every exit code is logged.
fn handle_exit<A>(
    command: &str,
    code: i32,
    stderr: &str,
    manifest: &mut InstallManifest<A>,
    callbacks: &dyn InstallerCallbacks,
) {
    if is_reboot_exit_code(code) {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Run: {command} exited with {code}, requesting a system restart"),
        );
        manifest.reboot_needed = true;
    } else if code != 0 {
        callbacks.on_log(
            LogLevel::Warn,
            &format!("Run: {command} exited with {code}: {stderr}"),
        );
    } else {
        callbacks.on_log(LogLevel::Info, &format!("Run: {command} exited with 0"));
    }
}

/// Windows convention (MSI / many redistributables): 3010 =
/// `ERROR_SUCCESS_REBOOT_REQUIRED`, 1641 = `ERROR_SUCCESS_REBOOT_INITIATED`.
/// Both mean the command succeeded but a restart is needed to finish. (On
/// Unix, exit codes are 0–255, so these never occur there.)
fn is_reboot_exit_code(code: i32) -> bool {
    code == 3010 || code == 1641
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

    #[test]
    fn test_is_reboot_exit_code() {
        assert!(is_reboot_exit_code(3010));
        assert!(is_reboot_exit_code(1641));
        assert!(!is_reboot_exit_code(0));
        assert!(!is_reboot_exit_code(1));
        assert!(!is_reboot_exit_code(3));
    }
}
