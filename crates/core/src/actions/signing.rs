use std::path::Path;
use std::process::Command;

use crate::callbacks::{InstallerCallbacks, LogLevel};
use crate::error::{InstallerError, InstallerResult};

const MAX_RETRIES: u32 = 3;
const RETRY_DELAY_SECS: u64 = 2;

fn build_command(sign_command: &str, file: &Path) -> String {
    let abs_path = dunce::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let file_str = abs_path.to_string_lossy();

    if sign_command.contains("#{file}") {
        sign_command.replace("#{file}", &file_str)
    } else {
        format!("{sign_command} {file_str}")
    }
}

fn run_shell(command: &str) -> Command {
    #[cfg(windows)]
    {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    }
}

/// Sign a file using the provided command template.
///
/// If the command contains `#{file}`, it is replaced with the absolute path.
/// Otherwise, the absolute path is appended as the last argument.
///
/// Retries up to 3 times with a 2-second delay on failure (timestamp servers
/// are flaky). Fails hard if all retries are exhausted — nothing ships unsigned.
pub fn sign_file(
    sign_command: &str,
    file: &Path,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let full_command = build_command(sign_command, file);

    callbacks.on_log(
        LogLevel::Info,
        &format!("Signing: signing {}", file.display()),
    );

    let mut last_error = None;

    for attempt in 1..=MAX_RETRIES {
        let output =
            run_shell(&full_command)
                .output()
                .map_err(|e| InstallerError::CommandExec {
                    command: full_command.clone(),
                    message: format!("failed to execute sign command: {e}"),
                })?;

        if output.status.success() {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Signing: signed {}", file.display()),
            );
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        let msg = format!(
            "Signing: attempt {attempt}/{MAX_RETRIES} failed for {}: exit code {}, stderr: {}",
            file.display(),
            output.status,
            stderr.trim()
        );

        callbacks.on_log(LogLevel::Warn, &msg);
        last_error = Some(msg);

        if attempt < MAX_RETRIES {
            std::thread::sleep(std::time::Duration::from_secs(RETRY_DELAY_SECS));
        }
    }

    Err(InstallerError::CommandExec {
        command: full_command,
        message: last_error.unwrap_or_else(|| "signing failed".into()),
    })
}

pub fn sign_all(
    sign_command: &str,
    files: &[&Path],
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for file in files {
        sign_file(sign_command, file, callbacks)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callbacks::NoOpCallbacks;

    #[test]
    fn test_build_command_with_placeholder() {
        let dir = std::env::temp_dir();
        let file = dir.join("test_sign.exe");
        std::fs::write(&file, "fake").unwrap();

        let result = build_command("echo signing #{file} done", &file);
        assert!(result.starts_with("echo signing "));
        assert!(result.ends_with(" done"));
        assert!(result.contains("test_sign.exe"));

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn test_build_command_without_placeholder() {
        let dir = std::env::temp_dir();
        let file = dir.join("test_sign2.exe");
        std::fs::write(&file, "fake").unwrap();

        let result = build_command("sign.bat", &file);
        assert!(result.starts_with("sign.bat "));
        assert!(result.contains("test_sign2.exe"));

        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn test_sign_file_success() {
        let dir = std::env::temp_dir().join("outto_test_sign");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("app.exe");
        std::fs::write(&file, "fake exe").unwrap();

        let callbacks = NoOpCallbacks;
        let result = sign_file("echo signed", &file, &callbacks);
        assert!(result.is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
