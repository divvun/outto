use std::fmt::Debug;
use std::fs;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::callbacks::{InstallerCallbacks, LogLevel};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::CoreAction;

/// Every platform's top-level manifest action enum implements this.
///
/// The trait owns both the *semantics* of undoing a recorded action
/// (`rollback`) and the serde bounds needed to persist/restore manifests.
/// Each platform can therefore put whatever OS-specific calls it needs
/// inside `rollback` — the core `rollback_actions` helper just iterates
/// in reverse and calls this method per action.
///
/// `restore_backups = true` is the "install failed, undo partial work
/// and restore any backed-up files" mode. `false` is the "uninstall" mode
/// (respect `preserve_on_uninstall`, delete backups rather than restore).
pub trait RollbackAction: Serialize + DeserializeOwned + Clone + Debug {
    fn rollback(
        &self,
        restore_backups: bool,
        callbacks: &dyn InstallerCallbacks,
    ) -> InstallerResult<()>;
}

/// Reverse each recorded action in `actions` (tail-first), logging progress
/// through `callbacks`. Collects individual failures and reports a combined
/// error at the end rather than stopping at the first one — best-effort
/// rollback is almost always better than aborting halfway through.
pub fn rollback_actions<A: RollbackAction>(
    actions: &[A],
    callbacks: &dyn InstallerCallbacks,
    restore_backups: bool,
) -> InstallerResult<()> {
    let total = actions.len() as u64;
    let mut errors = Vec::new();

    for (i, action) in actions.iter().rev().enumerate() {
        callbacks.on_log(LogLevel::Info, &format!("Rollback: {action:?}"));

        if let Err(e) = action.rollback(restore_backups, callbacks) {
            let msg = format!("Rollback: failed {action:?}: {e}");
            callbacks.on_log(LogLevel::Warn, &msg);
            errors.push(msg);
        }
        callbacks.on_progress("rollback", (i + 1) as u64, total);
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(InstallerError::Other(format!(
            "Rollback completed with {} errors: {}",
            errors.len(),
            errors.join("; ")
        )))
    }
}

// --- Helpers reusable by platform action enums ---

/// Remove `path` if it exists. On Windows, locked files can be handled by the
/// caller by catching the returned error and using `MoveFileEx(..., DELAY_UNTIL_REBOOT)`.
pub fn try_remove_file(path: &std::path::Path) -> InstallerResult<()> {
    if path.exists() {
        fs::remove_file(path).map_err(|e| InstallerError::FileOp {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    Ok(())
}

/// Shared roll-back of a file-copy record. Windows variants wrap this and add
/// delete-on-reboot scheduling for locked files; macOS uses it verbatim.
pub fn rollback_file_copied(
    dest: &std::path::Path,
    backup: Option<&std::path::Path>,
    preserve_on_uninstall: bool,
    uninst_remove_readonly: bool,
    restore_backups: bool,
) -> InstallerResult<()> {
    if !restore_backups && preserve_on_uninstall {
        return Ok(());
    }

    if !restore_backups && uninst_remove_readonly {
        if let Ok(metadata) = fs::metadata(dest) {
            let mut perms = metadata.permissions();
            if perms.readonly() {
                perms.set_readonly(false);
                let _ = fs::set_permissions(dest, perms);
            }
        }
    }

    try_remove_file(dest)?;

    if let Some(backup_path) = backup {
        if restore_backups {
            if backup_path.exists() {
                fs::rename(backup_path, dest).map_err(|e| InstallerError::FileOp {
                    path: dest.to_path_buf(),
                    source: e,
                })?;
            }
        } else {
            try_remove_file(backup_path)?;
        }
    }
    Ok(())
}

/// Remove `path` if it exists and is empty — the standard "undo DirectoryCreated"
/// semantics that both platforms share. A protected directory (see
/// [`is_protected_dir`]) is never removed, whatever a manifest says.
pub fn rollback_directory_created(path: &std::path::Path) -> InstallerResult<()> {
    if path.exists() && !is_protected_dir(path, &protected_dirs()) {
        let _ = fs::remove_dir(path); // silently ignore non-empty dirs
    }
    Ok(())
}

/// Well-known system and user directories on this machine: the Windows
/// folders (Windows, System32, SysWOW64, Program Files, ProgramData, the
/// profile and AppData folders, Temp) and the macOS ones (home, Library,
/// /Applications, /usr/local).
pub fn protected_dirs() -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = [
        "SystemRoot",
        "windir",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "CommonProgramFiles",
        "CommonProgramFiles(x86)",
        "ProgramData",
        "ALLUSERSPROFILE",
        "PUBLIC",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "HOME",
        "TMPDIR",
    ]
    .iter()
    .filter_map(|v| env(v))
    .collect();
    if let Some(root) = env("SystemRoot") {
        for sub in ["System32", "SysWOW64", "Fonts", "Sysnative"] {
            dirs.push(root.join(sub));
        }
    }
    if let Some(home) = env("HOME") {
        for sub in [
            "Library",
            "Applications",
            "Library/Fonts",
            "Library/LaunchAgents",
        ] {
            dirs.push(home.join(sub));
        }
    }
    for fixed in [
        "/Applications",
        "/Library",
        "/Library/Fonts",
        "/Library/LaunchAgents",
        "/Library/LaunchDaemons",
        "/usr",
        "/usr/local",
        "/usr/local/bin",
        "/opt",
    ] {
        dirs.push(PathBuf::from(fixed));
    }
    dirs
}

/// Whether `path` is a filesystem root, a drive root, or one of `protected`
/// (compared after normalisation, so `C:/Windows/SysWOW64` matches
/// `C:\WINDOWS\SysWOW64`).
pub fn is_protected_dir(path: &std::path::Path, protected: &[std::path::PathBuf]) -> bool {
    use crate::manifest::orphans::normalize_lexical;
    let raw = path.to_string_lossy();
    let windows = cfg!(windows)
        || raw.contains('\\')
        || (raw.len() >= 2 && raw.as_bytes()[1] == b':' && raw.as_bytes()[0].is_ascii_alphabetic());
    let key = normalize_lexical(&path.to_string_lossy(), windows);
    let is_root = key.is_empty()
        || key == "/"
        || key == "\\"
        || (key.len() <= 3 && key.as_bytes().get(1) == Some(&b':'));
    is_root
        || protected
            .iter()
            .any(|p| normalize_lexical(&p.to_string_lossy(), windows) == key)
}

impl RollbackAction for CoreAction {
    fn rollback(
        &self,
        restore_backups: bool,
        _callbacks: &dyn InstallerCallbacks,
    ) -> InstallerResult<()> {
        match self {
            CoreAction::FileCopied {
                dest,
                backup,
                preserve_on_uninstall,
                uninst_remove_readonly,
                uninst_restart_delete: _,
                restart_replace: _,
            } => rollback_file_copied(
                dest,
                backup.as_deref(),
                *preserve_on_uninstall,
                *uninst_remove_readonly,
                restore_backups,
            ),
            CoreAction::DirectoryCreated { path } => rollback_directory_created(path),
            CoreAction::CommandExecuted { .. } => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::callbacks::{LogLevel, NoOpCallbacks};
    use crate::error::ErrorAction;
    use crate::manifest::CoreAction;
    use std::path::PathBuf;

    #[test]
    fn test_rollback_empty_actions() {
        let actions: Vec<CoreAction> = vec![];
        let result = rollback_actions(&actions, &NoOpCallbacks, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_rollback_file_copied_removes_file() {
        let dir = std::env::temp_dir().join("outto_test_rollback_file_core");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("test.txt");
        std::fs::write(&file, "content").unwrap();
        assert!(file.exists());

        let actions = vec![CoreAction::FileCopied {
            dest: file.clone(),
            backup: None,
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        }];

        rollback_actions(&actions, &NoOpCallbacks, true).unwrap();
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_file_copied_restores_backup() {
        let dir = std::env::temp_dir().join("outto_test_rollback_backup_core");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let dest = dir.join("app.exe");
        let backup = dir.join("app.exe.bak");
        std::fs::write(&dest, "new content").unwrap();
        std::fs::write(&backup, "original content").unwrap();

        let actions = vec![CoreAction::FileCopied {
            dest: dest.clone(),
            backup: Some(backup.clone()),
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        }];

        rollback_actions(&actions, &NoOpCallbacks, true).unwrap();
        assert!(dest.exists());
        assert!(!backup.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_directory_removes_empty_dir() {
        let dir = std::env::temp_dir().join("outto_test_rollback_dir_core");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let actions = vec![CoreAction::DirectoryCreated { path: dir.clone() }];
        rollback_actions(&actions, &NoOpCallbacks, true).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn test_rollback_preserves_files_flagged_so() {
        let dir = std::env::temp_dir().join("outto_test_rollback_preserve");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("data.bin");
        std::fs::write(&file, "keep me").unwrap();

        let actions = vec![CoreAction::FileCopied {
            dest: file.clone(),
            backup: None,
            preserve_on_uninstall: true,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        }];

        // Uninstall mode (restore_backups=false) honours preserve_on_uninstall
        rollback_actions(&actions, &NoOpCallbacks, false).unwrap();
        assert!(file.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // A minimal test callbacks type for completeness.
    #[allow(dead_code)]
    struct TestCallbacks;
    impl InstallerCallbacks for TestCallbacks {
        fn on_progress(&self, _: &str, _: u64, _: u64) {}
        fn on_prompt(&self, _: crate::callbacks::Prompt) -> crate::callbacks::PromptResponse {
            crate::callbacks::PromptResponse::Yes
        }
        fn on_log(&self, _: LogLevel, _: &str) {}
        fn on_error(&self, _: &InstallerError) -> ErrorAction {
            ErrorAction::Abort
        }
    }
    const _: () = {
        let _: PathBuf;
    };
}

#[cfg(test)]
mod protected_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    #[test]
    fn protected_dirs_are_matched_in_any_spelling() {
        let protected = vec![
            PathBuf::from(r"C:\WINDOWS"),
            PathBuf::from(r"C:\WINDOWS\SysWOW64"),
            PathBuf::from(r"C:\Program Files"),
        ];
        for p in [
            r"C:\Windows\SysWOW64",
            "C:/Windows/SysWOW64/",
            r"c:\windows",
            r"C:\Program Files\",
            r"C:\",
            "C:",
            "D:/",
            "/",
        ] {
            assert!(is_protected_dir(Path::new(p), &protected), "{p}");
        }
        for p in [
            r"C:\Program Files\Outto E2E Test",
            r"C:\ProgramData\OuttoE2E-Shared",
            r"C:\Windows\SysWOW64\outto",
        ] {
            assert!(!is_protected_dir(Path::new(p), &protected), "{p}");
        }
    }

    #[test]
    fn rollback_removes_only_empty_unprotected_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let empty = tmp.path().join("empty");
        let full = tmp.path().join("full");
        fs::create_dir(&empty).unwrap();
        fs::create_dir(&full).unwrap();
        fs::write(full.join("keep.txt"), "x").unwrap();

        rollback_directory_created(&empty).unwrap();
        rollback_directory_created(&full).unwrap();
        assert!(!empty.exists());
        assert!(full.join("keep.txt").exists());
    }
}
