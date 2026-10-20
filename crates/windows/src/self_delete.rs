//! Deleting the uninstaller's own copy in the temporary directory.
//!
//! So that its install directory can be removed, the uninstaller moves its
//! running exe from `.outto\<package id>\` to
//! `%TEMP%\outto-uninstall-<pid>.exe`. A running exe can't be deleted, so it
//! starts a hidden `cmd.exe` that deletes that one file as soon as the
//! uninstaller has exited, retrying for about a minute. The helper inherits
//! none of the uninstaller's handles and gets a console of its own, so a
//! caller that waits for the uninstaller and reads its output to the end does
//! not wait for the helper too. Only when the helper can't be started is the
//! file left for deletion at the next restart.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Component, Path, Prefix};

use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
};

/// The name the uninstaller gives its copy in the temporary directory.
pub fn relocated_name(pid: u32) -> String {
    format!("outto-uninstall-{pid}.exe")
}

/// How [`delete_after_exit`] arranged for the file to go.
#[derive(Debug)]
pub enum Removal {
    /// A helper process with this id deletes it once this process has exited.
    AfterExit { helper_pid: u32 },
    /// The helper could not be started, for this reason, so it is deleted at
    /// the next restart.
    AtRestart(io::Error),
}

/// Arrange for `path`, this process's relocated exe, to be deleted once this
/// process has exited. Refuses anything but a file named like
/// [`relocated_name`] in a directory on a drive (the helper runs there and
/// names only the file).
pub fn delete_after_exit(path: &Path) -> io::Result<Removal> {
    let (dir, name) = check_target(path)?;
    match start_helper(dir, name) {
        Ok(helper_pid) => Ok(Removal::AfterExit { helper_pid }),
        Err(e) => {
            delete_at_restart(path)?;
            Ok(Removal::AtRestart(e))
        }
    }
}

fn check_target(path: &Path) -> io::Result<(&Path, &str)> {
    let refuse = |why: &str| {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not deleting {}: {why}", path.display()),
        ))
    };
    let Some(name) = path
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|n| is_relocated_name(n))
    else {
        return refuse("not an uninstaller's temporary copy");
    };
    let Some(dir) = path.parent() else {
        return refuse("no directory");
    };
    if !on_drive(dir) {
        return refuse("not in a directory on a drive");
    }
    if !std::fs::symlink_metadata(path)?.is_file() {
        return refuse("not a file");
    }
    Ok((dir, name))
}

fn is_relocated_name(name: &str) -> bool {
    name.strip_prefix("outto-uninstall-")
        .and_then(|rest| rest.strip_suffix(".exe"))
        .is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit()))
}

/// `C:\...`: cmd.exe can't make a UNC path its current directory.
fn on_drive(dir: &Path) -> bool {
    let mut components = dir.components();
    matches!(
        components.next(),
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_))
    ) && matches!(components.next(), Some(Component::RootDir))
}

/// Start the helper in `dir`, where it deletes `name` and nothing else. The
/// name is checked to be only ASCII letters, digits, `-` and `.`, so the
/// command line needs no quoting.
fn start_helper(dir: &Path, name: &str) -> io::Result<u32> {
    let cmd = system_dir()?.join("cmd.exe");
    // `/d` skips AutoRun commands; `ping` waits a second between attempts.
    let command_line = format!(
        "\"{}\" /d /q /c for /l %i in (1,1,60) do @(del /f /q {name} >nul 2>&1 & \
         if exist {name} (ping -n 2 127.0.0.1 >nul) else exit)",
        cmd.display()
    );
    let mut command_line = wide(OsStr::new(&command_line));
    let application = wide(cmd.as_os_str());
    let dir = wide(dir.as_os_str());

    let startup_info = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..unsafe { std::mem::zeroed() }
    };
    // Out of a job the caller may wait on or end, where the job allows it.
    let mut last_error = None;
    for flags in [
        CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB,
        CREATE_NO_WINDOW,
    ] {
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags,
                std::ptr::null(),
                dir.as_ptr(),
                &startup_info,
                &mut info,
            )
        };
        if ok != 0 {
            unsafe {
                CloseHandle(info.hThread);
                CloseHandle(info.hProcess);
            }
            return Ok(info.dwProcessId);
        }
        last_error = Some(io::Error::last_os_error());
    }
    Err(last_error.unwrap())
}

fn system_dir() -> io::Result<std::path::PathBuf> {
    let mut buf = [0u16; 260];
    let len = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    if len == 0 || len >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(&buf[..len]).into())
}

fn delete_at_restart(path: &Path) -> io::Result<()> {
    let path = wide(path.as_os_str());
    if unsafe { MoveFileExW(path.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "outto_test_self_delete_{name}_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn names() {
        assert!(is_relocated_name(&relocated_name(7480)));
        assert!(!is_relocated_name("outto-uninstall-.exe"));
        assert!(!is_relocated_name("outto-uninstall-12 & x.exe"));
        assert!(!is_relocated_name("uninstall.exe"));
        assert!(!is_relocated_name("outto-uninstall-12.exe.bak"));
    }

    #[test]
    fn refuses_other_files() {
        let dir = scratch("refuse");
        let other = dir.join("uninstall.exe");
        std::fs::write(&other, "x").unwrap();
        assert!(delete_after_exit(&other).is_err());
        let as_dir = dir.join(relocated_name(1));
        std::fs::create_dir(&as_dir).unwrap();
        assert!(delete_after_exit(&as_dir).is_err());
        assert!(!on_drive(Path::new(r"\\server\share\temp")));
        assert!(!on_drive(Path::new(r"temp")));
        assert!(on_drive(Path::new(r"C:\Users\admin\AppData\Local\Temp")));
        assert!(other.exists() && as_dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The helper deletes the file and leaves its neighbours alone.
    #[test]
    fn helper_deletes_only_the_file() {
        let dir = scratch("helper");
        let target = dir.join(relocated_name(std::process::id()));
        let neighbour = dir.join("keep.exe");
        std::fs::write(&target, "x").unwrap();
        std::fs::write(&neighbour, "x").unwrap();

        match delete_after_exit(&target).unwrap() {
            Removal::AfterExit { .. } => {}
            Removal::AtRestart(e) => panic!("helper not started: {e}"),
        }
        let start = Instant::now();
        while target.exists() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(!target.exists());
        assert!(neighbour.exists());
        // The directory is the helper's current directory until it exits.
        while std::fs::remove_dir_all(&dir).is_err() && start.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
