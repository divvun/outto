//! `run_as_original_user` on Windows: start a `[[run]]` command as the user
//! signed in to the desktop rather than with the installer's elevated token.
//!
//! The token is borrowed from the shell (Explorer) process, duplicated as a
//! primary token, and handed to `CreateProcessWithTokenW`. That call needs
//! SeImpersonatePrivilege, which an elevated administrator has.

#[cfg(windows)]
use crate::config::ShowWindow;
#[cfg(windows)]
use crate::error::{InstallerError, InstallerResult};

#[cfg(windows)]
pub enum Outcome {
    /// Not elevated, or no shell to borrow a token from: the caller should
    /// run the command normally, which already runs it as the current user.
    Unavailable(&'static str),
    /// Started without waiting.
    Spawned,
    /// Ran to completion with this exit code.
    Exited(u32),
}

#[cfg(windows)]
pub fn run(
    command: &str,
    args: &[String],
    working_dir: Option<&str>,
    show: &ShowWindow,
    wait: bool,
) -> InstallerResult<Outcome> {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    if !is_elevated() {
        return Ok(Outcome::Unavailable("installer is not elevated"));
    }
    let token = match shell_token()? {
        Some(token) => token,
        None => return Ok(Outcome::Unavailable("no desktop shell is running")),
    };

    let mut command_line = String::new();
    append_program(&mut command_line, command);
    for arg in args {
        command_line.push(' ');
        append_arg(&mut command_line, arg);
    }
    let mut command_line = wide(&command_line);
    let working_dir = working_dir.map(wide);

    let (creation_flags, show_window) = match show {
        ShowWindow::Hidden => (CREATE_NO_WINDOW, SW_HIDE),
        ShowWindow::Normal => (0, SW_SHOWNORMAL),
        ShowWindow::Minimized => (0, SW_SHOWMINIMIZED),
        ShowWindow::Maximized => (0, SW_SHOWMAXIMIZED),
    };
    let startup_info = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        dwFlags: STARTF_USESHOWWINDOW,
        wShowWindow: show_window as u16,
        ..Default::default()
    };
    let mut process_info = PROCESS_INFORMATION::default();

    unsafe {
        // A null environment gives the child one built from the user's profile.
        let created = CreateProcessWithTokenW(
            token,
            LOGON_WITH_PROFILE,
            std::ptr::null(),
            command_line.as_mut_ptr(),
            creation_flags,
            std::ptr::null(),
            working_dir
                .as_ref()
                .map_or(std::ptr::null(), |wd| wd.as_ptr()),
            &startup_info,
            &mut process_info,
        );
        let create_error = std::io::Error::last_os_error();
        CloseHandle(token);
        if created == 0 {
            return Err(InstallerError::CommandExec {
                command: command.to_string(),
                message: format!("failed to start as the signed-in user: {create_error}"),
            });
        }
        CloseHandle(process_info.hThread);

        if !wait {
            CloseHandle(process_info.hProcess);
            return Ok(Outcome::Spawned);
        }

        let mut exit_code = 0u32;
        let waited = WaitForSingleObject(process_info.hProcess, INFINITE) != WAIT_FAILED
            && GetExitCodeProcess(process_info.hProcess, &mut exit_code) != 0;
        let wait_error = std::io::Error::last_os_error();
        CloseHandle(process_info.hProcess);
        if !waited {
            return Err(InstallerError::CommandExec {
                command: command.to_string(),
                message: format!("failed to wait for the command: {wait_error}"),
            });
        }
        Ok(Outcome::Exited(exit_code))
    }
}

/// A primary token for the shell's user, or `None` when there's no shell.
#[cfg(windows)]
fn shell_token() -> InstallerResult<Option<windows_sys::Win32::Foundation::HANDLE>> {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    unsafe {
        let shell = GetShellWindow();
        if shell.is_null() {
            return Ok(None);
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(shell, &mut pid);
        if pid == 0 {
            return Ok(None);
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return Err(os_error("OpenProcess on the shell"));
        }
        let mut shell_token: HANDLE = std::ptr::null_mut();
        let opened = OpenProcessToken(process, TOKEN_DUPLICATE, &mut shell_token);
        let open_error = os_error("OpenProcessToken on the shell");
        CloseHandle(process);
        if opened == 0 {
            return Err(open_error);
        }

        let mut primary: HANDLE = std::ptr::null_mut();
        let duplicated = DuplicateTokenEx(
            shell_token,
            TOKEN_QUERY
                | TOKEN_DUPLICATE
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_DEFAULT
                | TOKEN_ADJUST_SESSIONID,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        );
        let duplicate_error = os_error("DuplicateTokenEx on the shell token");
        CloseHandle(shell_token);
        if duplicated == 0 {
            return Err(duplicate_error);
        }
        Ok(Some(primary))
    }
}

#[cfg(windows)]
fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Threading::*;

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size = std::mem::size_of::<TOKEN_ELEVATION>() as u32;
        let result = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut std::ffi::c_void,
            size,
            &mut size,
        );
        CloseHandle(token);
        result != 0 && elevation.TokenIsElevated != 0
    }
}

#[cfg(windows)]
fn os_error(what: &str) -> InstallerError {
    InstallerError::Other(format!(
        "{what} failed: {}",
        std::io::Error::last_os_error()
    ))
}

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The program token of a command line. Windows parses it without escapes
/// (a path can't contain `"`), so quoting is all it needs.
#[cfg(any(windows, test))]
fn append_program(command_line: &mut String, program: &str) {
    command_line.push('"');
    command_line.push_str(program);
    command_line.push('"');
}

/// Append one argument, quoted and escaped the way `CommandLineToArgvW` and
/// the MSVC runtime split it back out.
#[cfg(any(windows, test))]
fn append_arg(command_line: &mut String, arg: &str) {
    if !arg.is_empty() && !arg.contains([' ', '\t', '\n', '\x0b', '"']) {
        command_line.push_str(arg);
        return;
    }
    command_line.push('"');
    let mut backslashes = 0;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                command_line.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                command_line.push('"');
                backslashes = 0;
            }
            _ => {
                command_line.extend(std::iter::repeat_n('\\', backslashes));
                command_line.push(c);
                backslashes = 0;
            }
        }
    }
    command_line.extend(std::iter::repeat_n('\\', backslashes * 2));
    command_line.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quoted(arg: &str) -> String {
        let mut s = String::new();
        append_arg(&mut s, arg);
        s
    }

    #[test]
    fn plain_args_are_unquoted() {
        assert_eq!(quoted("/Run"), "/Run");
        assert_eq!(quoted(r"C:\path\file"), r"C:\path\file");
    }

    #[test]
    fn args_with_spaces_or_quotes_are_escaped() {
        assert_eq!(quoted(""), r#""""#);
        assert_eq!(quoted("hello world"), r#""hello world""#);
        assert_eq!(quoted(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quoted(r"C:\a dir\"), r#""C:\a dir\\""#);
        assert_eq!(quoted(r#"a\"b c"#), r#""a\\\"b c""#);
    }

    #[test]
    fn program_is_quoted_verbatim() {
        let mut s = String::new();
        append_program(&mut s, r"C:\Program Files\x.exe");
        assert_eq!(s, r#""C:\Program Files\x.exe""#);
    }
}
