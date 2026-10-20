//! Removing files that a running process may have loaded.
//!
//! A DLL or exe that a process has mapped cannot be deleted, but it can be
//! renamed. Scheduling its own path for deletion at the next restart is a
//! trap: if the same file is installed there again before then, the restart
//! deletes the new one. So a file in use is moved out of the way first, to a
//! temporary directory on the same volume, which frees its path at once, and
//! the moved copy is what gets deleted at the restart. Only when it cannot be
//! moved is its own path scheduled.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW};

/// What [`discard_file`] did with a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discarded {
    /// Deleted, or it was not there.
    Deleted,
    /// In use: moved to this path, to be deleted there at the next restart.
    MovedAside(PathBuf),
    /// In use and immovable: its own path is deleted at the next restart.
    DeletedAtRestart,
}

/// Delete the file at `path`, moving it aside if it is in use (see the
/// module docs). Fails only if the file stays where it is, undeleted and
/// unscheduled.
pub fn discard_file(path: &Path) -> std::io::Result<Discarded> {
    let error = match std::fs::remove_file(path) {
        Ok(()) => return Ok(Discarded::Deleted),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Discarded::Deleted),
        Err(e) => e,
    };
    for dir in trash_dirs() {
        let trash = dir.join(trash_name(path));
        if move_file(path, Some(&trash), 0).is_ok() {
            let _ = move_file(&trash, None, MOVEFILE_DELAY_UNTIL_REBOOT);
            return Ok(Discarded::MovedAside(trash));
        }
    }
    if move_file(path, None, MOVEFILE_DELAY_UNTIL_REBOOT).is_ok() {
        return Ok(Discarded::DeletedAtRestart);
    }
    Err(error)
}

/// [`discard_file`], logging what happened to a file in use.
pub fn discard_file_logged(
    path: &Path,
    callbacks: &dyn InstallerCallbacks,
) -> std::io::Result<Discarded> {
    let outcome = discard_file(path)?;
    match &outcome {
        Discarded::Deleted => {}
        Discarded::MovedAside(to) => callbacks.on_log(
            LogLevel::Info,
            &format!(
                "{} is in use; moved it to {}, which is deleted at the next restart",
                path.display(),
                to.display()
            ),
        ),
        Discarded::DeletedAtRestart => callbacks.on_log(
            LogLevel::Warn,
            &format!(
                "{} is in use and could not be moved; it is deleted at the next restart",
                path.display()
            ),
        ),
    }
    Ok(outcome)
}

/// Delete the directory `dir` and everything in it, discarding each file
/// with [`discard_file`]. A directory still holding a file that could only
/// be scheduled is itself deleted at the next restart. Returns the number
/// of entries that could not be removed at all; they are logged.
pub fn discard_tree(dir: &Path, callbacks: &dyn InstallerCallbacks) -> usize {
    let mut failures = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                failures += discard_tree(&path, callbacks);
            } else if let Err(e) = discard_file_logged(&path, callbacks) {
                callbacks.on_log(
                    LogLevel::Warn,
                    &format!("Could not remove {}: {e}", path.display()),
                );
                failures += 1;
            }
        }
    }
    if std::fs::remove_dir(dir).is_err() && dir.exists() {
        if move_file(dir, None, MOVEFILE_DELAY_UNTIL_REBOOT).is_ok() {
            callbacks.on_log(
                LogLevel::Warn,
                &format!(
                    "{} is not empty yet; it is deleted at the next restart",
                    dir.display()
                ),
            );
        } else {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Could not remove {}", dir.display()),
            );
            failures += 1;
        }
    }
    failures
}

/// Where files in use are moved: the Windows temporary directory, then the
/// current user's. A move only works within one volume, so the first one on
/// the file's volume takes it.
fn trash_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(root) = std::env::var_os("SystemRoot") {
        dirs.push(PathBuf::from(root).join("Temp"));
    }
    dirs.push(std::env::temp_dir());
    dirs
}

fn trash_name(path: &Path) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!(
        "outto-{}-{nanos:08x}-{}-{name}.deleted",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

fn move_file(from: &Path, to: Option<&Path>, flags: u32) -> std::io::Result<()> {
    let wide = |p: &Path| -> Vec<u16> {
        OsStr::new(p)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    };
    let from = wide(from);
    let to = to.map(wide);
    let ok = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()),
            flags,
        )
    };
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::NoOpCallbacks;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("outto_test_discard_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A loaded DLL cannot be deleted but is moved aside, freeing its path
    /// while the process keeps it loaded.
    #[test]
    fn a_loaded_dll_is_moved_aside() {
        use windows_sys::Win32::Foundation::FreeLibrary;
        use windows_sys::Win32::System::LibraryLoader::LoadLibraryW;
        let tmp = scratch("loaded");
        let dll = tmp.join("outto_discard_test.dll");
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        std::fs::copy(system.join("msimg32.dll"), &dll).unwrap();
        let wide: Vec<u16> = OsStr::new(&dll)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let module = unsafe { LoadLibraryW(wide.as_ptr()) };
        assert!(!module.is_null(), "{}", std::io::Error::last_os_error());
        assert!(std::fs::remove_file(&dll).is_err());

        let outcome = discard_file(&dll).unwrap();
        let Discarded::MovedAside(to) = &outcome else {
            panic!("expected the loaded DLL to be moved aside, got {outcome:?}");
        };
        assert!(!dll.exists());
        assert!(to.exists());
        std::fs::write(&dll, b"a new file in its place").unwrap();

        unsafe { FreeLibrary(module) };
        std::fs::remove_file(to).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_missing_file_counts_as_deleted() {
        let tmp = scratch("absent");
        assert_eq!(
            discard_file(&tmp.join("absent")).unwrap(),
            Discarded::Deleted
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn discard_tree_removes_nested_directories() {
        let tmp = scratch("tree");
        let root = tmp.join("1.0.0");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("x.dll"), b"x").unwrap();
        std::fs::write(root.join("a/b/y.dll"), b"y").unwrap();
        assert_eq!(discard_tree(&root, &NoOpCallbacks), 0);
        assert!(!root.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
