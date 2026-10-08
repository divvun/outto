use std::process::Command;

use crate::callbacks::{InstallerCallbacks, LogLevel};
use crate::config::{RunEntry, RunPhase, ShowWindow, VariableResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{CoreAction, InstallManifest, UninstallHook};

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

fn phase_name(phase: &RunPhase) -> &'static str {
    match phase {
        RunPhase::BeforeInstall => "before_install",
        RunPhase::AfterInstall => "after_install",
        RunPhase::BeforeUninstall => "before_uninstall",
        RunPhase::AfterUninstall => "after_uninstall",
    }
}

/// Resolve a `[[run]]` entry into a ready-to-run command.
///
/// The program and working directory are paths, so they go through
/// `resolve_path`, which on Windows turns `/` into `\`. That matters beyond
/// tidiness: `#{sys}/cmd.exe` resolved to `C:\Windows\System32/cmd.exe`, and
/// cmd.exe started under that name fails every command with "The system
/// cannot find the path specified".
pub fn resolve_command(
    entry: &RunEntry,
    resolver: &VariableResolver,
) -> InstallerResult<UninstallHook> {
    let command = resolver
        .resolve_path(&entry.command)?
        .to_string_lossy()
        .into_owned();
    let arguments = entry
        .arguments
        .as_deref()
        .map(|a| resolver.resolve(a))
        .transpose()?
        .map(|a| split_args(&a))
        .unwrap_or_default();
    let working_dir = entry
        .working_dir
        .as_deref()
        .map(|wd| resolver.resolve_path(wd))
        .transpose()?
        .map(|wd| wd.to_string_lossy().into_owned());
    Ok(UninstallHook {
        phase: entry.phase.clone(),
        command,
        arguments,
        working_dir,
        wait: entry.wait,
        show: entry.show.clone(),
        run_as_original_user: entry.run_as_original_user,
    })
}

/// The `before_uninstall` and `after_uninstall` entries of `entries`, in
/// config order, resolved for recording in the manifest.
pub fn resolve_uninstall_hooks(
    entries: &[RunEntry],
    resolver: &VariableResolver,
) -> InstallerResult<Vec<UninstallHook>> {
    entries
        .iter()
        .filter(|e| {
            matches!(
                e.phase,
                RunPhase::BeforeUninstall | RunPhase::AfterUninstall
            )
        })
        .map(|e| resolve_command(e, resolver))
        .collect()
}

/// Run the recorded uninstall hooks of `phase`, in order. A hook that can't be
/// started or exits non-zero is logged and the uninstall carries on, as with
/// install-time commands. `None` means the manifest predates recorded hooks.
pub fn run_uninstall_hooks(
    hooks: Option<&[UninstallHook]>,
    phase: &RunPhase,
    callbacks: &dyn InstallerCallbacks,
) {
    let phase_str = phase_name(phase);
    let Some(hooks) = hooks else {
        callbacks.on_log(
            LogLevel::Warn,
            &format!(
                "Run: this install's manifest does not record its uninstall commands; \
                 any {phase_str} commands it had cannot be run"
            ),
        );
        return;
    };
    for hook in hooks.iter().filter(|h| &h.phase == phase) {
        log_executing(phase_str, hook, callbacks);
        match run_program(hook, callbacks) {
            Ok(Ran::Exited { code, stderr }) => {
                if code == 0 {
                    callbacks.on_log(
                        LogLevel::Info,
                        &format!("Run: {} exited with 0", hook.command),
                    );
                } else {
                    callbacks.on_log(
                        LogLevel::Warn,
                        &format!("Run: {} exited with {code}: {stderr}", hook.command),
                    );
                }
            }
            Ok(Ran::Spawned) => log_spawned(hook, callbacks),
            Ok(Ran::Unavailable(_)) => unreachable!("run_program falls back itself"),
            Err(e) => callbacks.on_log(LogLevel::Warn, &format!("Run: {e}")),
        }
    }
}

fn log_executing(phase_str: &str, cmd: &UninstallHook, callbacks: &dyn InstallerCallbacks) {
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Run: executing ({phase_str}): {} {}",
            cmd.command,
            cmd.arguments.join(" ")
        ),
    );
}

fn log_spawned(cmd: &UninstallHook, callbacks: &dyn InstallerCallbacks) {
    let who = if cmd.run_as_original_user {
        " as the signed-in user"
    } else {
        ""
    };
    callbacks.on_log(
        LogLevel::Info,
        &format!("Run: started {}{who} (not waiting)", cmd.command),
    );
}

enum Ran {
    Exited { code: i32, stderr: String },
    Spawned,
    Unavailable(&'static str),
}

/// Start `cmd`, waiting for it if asked to. With `run_as_original_user` on
/// Windows it runs with the desktop user's token when possible, otherwise as
/// the current user.
fn run_program(cmd: &UninstallHook, callbacks: &dyn InstallerCallbacks) -> InstallerResult<Ran> {
    if !cmd.run_as_original_user {
        return run_as_current_user(cmd);
    }
    match run_as_original_user(cmd)? {
        Ran::Unavailable(reason) => {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Run: {reason}; running as the current user"),
            );
            run_as_current_user(cmd)
        }
        ran => Ok(ran),
    }
}

#[cfg(windows)]
fn run_as_original_user(cmd: &UninstallHook) -> InstallerResult<Ran> {
    use super::original_user::{self, Outcome};
    Ok(
        match original_user::run(
            &cmd.command,
            &cmd.arguments,
            cmd.working_dir.as_deref(),
            &cmd.show,
            cmd.wait,
        )? {
            Outcome::Unavailable(reason) => Ran::Unavailable(reason),
            Outcome::Spawned => Ran::Spawned,
            Outcome::Exited(code) => Ran::Exited {
                code: code as i32,
                stderr: String::new(),
            },
        },
    )
}

#[cfg(not(windows))]
fn run_as_original_user(_cmd: &UninstallHook) -> InstallerResult<Ran> {
    Ok(Ran::Unavailable(
        "running as the original user is Windows-only",
    ))
}

fn run_as_current_user(cmd: &UninstallHook) -> InstallerResult<Ran> {
    let mut command = Command::new(&cmd.command);
    command.args(&cmd.arguments);
    if let Some(ref wd) = cmd.working_dir {
        command.current_dir(wd);
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let creation_flags = match cmd.show {
            ShowWindow::Hidden => 0x08000000,
            _ => 0,
        };
        command.creation_flags(creation_flags);
    }
    #[cfg(not(windows))]
    let _: &ShowWindow = &cmd.show;

    if cmd.wait {
        let output = command.output().map_err(|e| InstallerError::CommandExec {
            command: cmd.command.clone(),
            message: format!("failed to execute: {e}"),
        })?;
        // A process killed by a signal has no code; treat it as a failure.
        Ok(Ran::Exited {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    } else {
        command.spawn().map_err(|e| InstallerError::CommandExec {
            command: cmd.command.clone(),
            message: format!("failed to spawn: {e}"),
        })?;
        Ok(Ran::Spawned)
    }
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
    let phase_str = phase_name(&entry.phase);
    let cmd = resolve_command(entry, resolver)?;
    log_executing(phase_str, &cmd, callbacks);

    match run_program(&cmd, callbacks)? {
        Ran::Exited { code, stderr } => {
            handle_exit(&cmd.command, code, &stderr, manifest, callbacks)
        }
        Ran::Spawned => log_spawned(&cmd, callbacks),
        Ran::Unavailable(_) => unreachable!("run_program falls back itself"),
    }

    record(manifest, cmd.command, phase_str);
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

    fn entry(phase: RunPhase, command: &str, arguments: Option<&str>) -> RunEntry {
        RunEntry {
            phase,
            command: command.into(),
            arguments: arguments.map(Into::into),
            wait: true,
            show: ShowWindow::Hidden,
            component: None,
            working_dir: Some("#{app}/bin".into()),
            arch: None,
            run_as_original_user: false,
        }
    }

    fn windows_resolver() -> VariableResolver {
        let mut r = VariableResolver::new()
            .with_windows_paths(true)
            .with_install_dir(std::path::Path::new(r"C:\Program Files\Võro Keyboards"));
        r.set_variable("sys", r"C:\WINDOWS\System32");
        r.set_variable("pf", r"C:\Program Files");
        r
    }

    #[test]
    fn program_path_gets_native_separators() {
        let cmd = resolve_command(
            &entry(RunPhase::AfterInstall, "#{sys}/cmd.exe", Some("/c exit 3")),
            &windows_resolver(),
        )
        .unwrap();
        assert_eq!(cmd.command, r"C:\WINDOWS\System32\cmd.exe");
        // Arguments are not paths: their slashes are switches.
        assert_eq!(cmd.arguments, vec!["/c", "exit", "3"]);
        assert_eq!(
            cmd.working_dir.as_deref(),
            Some(r"C:\Program Files\Võro Keyboards\bin")
        );
    }

    #[test]
    fn uninstall_hooks_are_resolved_in_order() {
        let entries = [
            entry(
                RunPhase::AfterInstall,
                "#{app}/kbdi.exe",
                Some("keyboard_install"),
            ),
            entry(
                RunPhase::BeforeUninstall,
                "#{app}/kbdi.exe",
                Some(r#"keyboard_uninstall "{2cc0c11a-efb4-5241-a262-330a9475c109}""#),
            ),
            entry(
                RunPhase::BeforeUninstall,
                "#{pf}/Divvun/Text Service/unins000.exe",
                Some("/VERYSILENT /SUPPRESSMSGBOXES /NORESTART"),
            ),
            entry(
                RunPhase::AfterUninstall,
                "#{sys}/cmd.exe",
                Some("/c exit 0"),
            ),
        ];
        let hooks = resolve_uninstall_hooks(&entries, &windows_resolver()).unwrap();
        let summary: Vec<(RunPhase, &str, Vec<&str>)> = hooks
            .iter()
            .map(|h| {
                (
                    h.phase.clone(),
                    h.command.as_str(),
                    h.arguments.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    RunPhase::BeforeUninstall,
                    r"C:\Program Files\Võro Keyboards\kbdi.exe",
                    vec![
                        "keyboard_uninstall",
                        "{2cc0c11a-efb4-5241-a262-330a9475c109}"
                    ],
                ),
                (
                    RunPhase::BeforeUninstall,
                    r"C:\Program Files\Divvun\Text Service\unins000.exe",
                    vec!["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"],
                ),
                (
                    RunPhase::AfterUninstall,
                    r"C:\WINDOWS\System32\cmd.exe",
                    vec!["/c", "exit", "0"],
                ),
            ]
        );
    }

    #[test]
    fn unknown_variable_in_uninstall_hook_is_an_error() {
        let entries = [entry(RunPhase::BeforeUninstall, "#{nope}/x.exe", None)];
        assert!(resolve_uninstall_hooks(&entries, &windows_resolver()).is_err());
    }

    #[derive(Default)]
    struct Logs(std::sync::Mutex<Vec<(LogLevel, String)>>);

    impl InstallerCallbacks for Logs {
        fn on_progress(&self, _: &str, _: u64, _: u64) {}
        fn on_prompt(&self, _: crate::callbacks::Prompt) -> crate::callbacks::PromptResponse {
            crate::callbacks::PromptResponse::Yes
        }
        fn on_log(&self, level: LogLevel, message: &str) {
            self.0.lock().unwrap().push((level, message.to_string()));
        }
        fn on_error(&self, _: &InstallerError) -> crate::error::ErrorAction {
            crate::error::ErrorAction::Abort
        }
    }

    #[test]
    fn missing_hooks_are_reported_as_unknown() {
        let logs = Logs::default();
        run_uninstall_hooks(None, &RunPhase::BeforeUninstall, &logs);
        let logs = logs.0.into_inner().unwrap();
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].0, LogLevel::Warn);
        assert!(logs[0].1.contains("before_uninstall"), "{}", logs[0].1);
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_hooks_run_in_order_and_log_exit_codes() {
        let tmp = tempfile::tempdir().unwrap();
        let out = tmp.path().join("out.txt");
        let sh = |phase: RunPhase, script: String| UninstallHook {
            phase,
            command: "/bin/sh".into(),
            arguments: vec!["-c".into(), script],
            working_dir: None,
            wait: true,
            show: ShowWindow::Hidden,
            run_as_original_user: false,
        };
        let hooks = vec![
            sh(
                RunPhase::BeforeUninstall,
                format!("echo one >> '{}'", out.display()),
            ),
            sh(
                RunPhase::AfterUninstall,
                format!("echo after >> '{}'", out.display()),
            ),
            sh(RunPhase::BeforeUninstall, "echo oops >&2; exit 4".into()),
            sh(
                RunPhase::BeforeUninstall,
                format!("echo two >> '{}'", out.display()),
            ),
            UninstallHook {
                command: tmp.path().join("missing").to_string_lossy().into_owned(),
                ..sh(RunPhase::BeforeUninstall, String::new())
            },
        ];
        let logs = Logs::default();
        run_uninstall_hooks(Some(&hooks), &RunPhase::BeforeUninstall, &logs);

        assert_eq!(std::fs::read_to_string(&out).unwrap(), "one\ntwo\n");
        let logs = logs.0.into_inner().unwrap();
        let text: Vec<&str> = logs.iter().map(|(_, m)| m.as_str()).collect();
        assert!(
            text.iter().any(|m| m.ends_with("exited with 0")),
            "{text:?}"
        );
        assert!(
            logs.iter()
                .any(|(l, m)| *l == LogLevel::Warn && m.contains("exited with 4: oops")),
            "{text:?}"
        );
        assert!(
            logs.iter()
                .any(|(l, m)| *l == LogLevel::Warn && m.contains("failed to execute")),
            "{text:?}"
        );
        assert!(!text.iter().any(|m| m.contains("after")), "{text:?}");
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
