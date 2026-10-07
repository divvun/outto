//! Removal of the temporary directories the installer extracts its payload
//! into.
//!
//! The GUI leaves through `std::process::exit` on almost every path, which
//! skips destructors, so a `tempfile::TempDir` held for the session would
//! never be deleted. Instead the directories are registered here and removed
//! by [`exit`], which every exit path goes through, and by [`run`] when `main`
//! returns normally.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use outto_core::LogLevel;

static TEMP_DIRS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Remove `dir` and everything in it when the process exits.
pub fn register(dir: PathBuf) {
    if let Ok(mut dirs) = TEMP_DIRS.lock() {
        dirs.push(dir);
    }
}

/// Remove the registered directories, then exit with `code`.
pub fn exit(code: i32) -> ! {
    run();
    std::process::exit(code);
}

/// Remove the registered directories. Best effort: whatever can't be deleted
/// now (a file still in use) is scheduled for deletion at the next restart on
/// Windows, or left in place, and logged either way.
pub fn run() {
    let dirs = match TEMP_DIRS.lock() {
        Ok(mut dirs) => std::mem::take(&mut *dirs),
        Err(_) => return,
    };
    for dir in dirs {
        remove_dir(&dir);
    }
}

fn remove_dir(dir: &Path) {
    if !dir.exists() {
        return;
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => log(
            LogLevel::Debug,
            &format!("Removed temporary directory {}", dir.display()),
        ),
        Err(e) => {
            log(
                LogLevel::Warn,
                &format!("Could not remove temporary directory {}: {e}", dir.display()),
            );
            schedule_leftovers(dir);
        }
    }
}

#[cfg(windows)]
fn schedule_leftovers(dir: &Path) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};

    // Children before their parents, so the directories are empty by the time
    // the session manager reaches them.
    let mut scheduled = 0usize;
    for entry in walkdir_post_order(dir) {
        let wide: Vec<u16> = OsStr::new(&entry)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let ok = unsafe {
            MoveFileExW(
                wide.as_ptr(),
                std::ptr::null(),
                MOVEFILE_DELAY_UNTIL_REBOOT,
            )
        } != 0;
        if ok {
            scheduled += 1;
        } else {
            log(
                LogLevel::Warn,
                &format!(
                    "Could not schedule {} for deletion: {}",
                    entry.display(),
                    std::io::Error::last_os_error()
                ),
            );
        }
    }
    if scheduled > 0 {
        log(
            LogLevel::Info,
            &format!(
                "Scheduled {scheduled} leftover path(s) under {} for deletion at restart",
                dir.display()
            ),
        );
    }
}

#[cfg(not(windows))]
fn schedule_leftovers(dir: &Path) {
    log(
        LogLevel::Info,
        &format!("Leaving {} in place", dir.display()),
    );
}

/// Every path under `dir` that still exists, deepest first, then `dir`.
#[cfg(windows)]
fn walkdir_post_order(dir: &Path) -> Vec<PathBuf> {
    fn visit(path: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if entry.file_type().is_ok_and(|t| t.is_dir()) {
                    visit(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
        out.push(path.to_path_buf());
    }
    let mut out = Vec::new();
    visit(dir, &mut out);
    out
}

fn log(level: LogLevel, message: &str) {
    if level != LogLevel::Debug {
        eprintln!("[{level:?}] {message}");
    }
    outto_core::logfile::write(level, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_removes_registered_dirs() {
        let dir = tempfile::Builder::new()
            .prefix("outto-cleanup-test")
            .tempdir()
            .unwrap()
            .keep();
        std::fs::create_dir_all(dir.join("contents/source")).unwrap();
        std::fs::write(dir.join("payload.box"), "x").unwrap();
        std::fs::write(dir.join("contents/source/a.txt"), "a").unwrap();

        register(dir.clone());
        run();

        assert!(!dir.exists());
        // Already-removed directories are not revisited.
        run();
    }
}
