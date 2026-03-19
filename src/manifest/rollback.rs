use std::fs;

use crate::error::{InstallerError, InstallerResult};
use crate::manifest::ActionRecord;
use crate::{InstallerCallbacks, LogLevel};

pub fn rollback_actions(
    actions: &[ActionRecord],
    callbacks: &dyn InstallerCallbacks,
    restore_backups: bool,
) -> InstallerResult<()> {
    let total = actions.len() as u64;
    let mut errors = Vec::new();

    for (i, action) in actions.iter().rev().enumerate() {
        callbacks.on_progress("rollback", i as u64, total);
        callbacks.on_log(
            LogLevel::Info,
            &format!("Rolling back action: {action:?}"),
        );

        if let Err(e) = rollback_single(action, restore_backups) {
            let msg = format!("Failed to rollback {action:?}: {e}");
            callbacks.on_log(LogLevel::Warn, &msg);
            errors.push(msg);
        }
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

fn try_remove_file(path: &std::path::Path) -> InstallerResult<()> {
    if path.exists() {
        if let Err(e) = fs::remove_file(path) {
            #[cfg(windows)]
            if schedule_delete_on_reboot(path).is_ok() {
                return Ok(());
            }

            return Err(InstallerError::FileOp {
                path: path.to_path_buf(),
                source: e,
            });
        }
    }
    Ok(())
}

fn rollback_single(action: &ActionRecord, restore_backups: bool) -> InstallerResult<()> {
    match action {
        ActionRecord::FileCopied {
            dest,
            backup,
            preserve_on_uninstall,
            uninst_remove_readonly,
            uninst_restart_delete,
            restart_replace,
        } => {
            // During uninstall, respect preserve flag
            if !restore_backups && *preserve_on_uninstall {
                return Ok(());
            }

            // Clear read-only before deleting if requested
            if !restore_backups && *uninst_remove_readonly {
                if let Ok(metadata) = fs::metadata(dest) {
                    let mut perms = metadata.permissions();
                    if perms.readonly() {
                        perms.set_readonly(false);
                        let _ = fs::set_permissions(dest, perms);
                    }
                }
            }

            // Use MoveFileEx delete-on-reboot if requested
            if !restore_backups && *uninst_restart_delete {
                #[cfg(windows)]
                {
                    let _ = schedule_delete_on_reboot(dest);
                }
                // Also delete backup
                if let Some(backup_path) = backup {
                    try_remove_file(backup_path)?;
                }
                return Ok(());
            }

            try_remove_file(dest)?;

            if let Some(backup_path) = backup {
                if restore_backups {
                    if backup_path.exists() {
                        fs::rename(backup_path, dest).map_err(|e| InstallerError::FileOp {
                            path: dest.clone(),
                            source: e,
                        })?;
                    }
                } else {
                    try_remove_file(backup_path)?;
                }
            }
            Ok(())
        }
        ActionRecord::DirectoryCreated { path } => {
            // Only remove if empty (don't delete user data)
            if path.exists() {
                let _ = fs::remove_dir(path); // ignore error if not empty
            }
            Ok(())
        }
        ActionRecord::ShortcutCreated { path } => {
            if path.exists() {
                fs::remove_file(path).map_err(|e| InstallerError::FileOp {
                    path: path.clone(),
                    source: e,
                })?;
            }
            Ok(())
        }
        // Registry, env vars, services, etc. require platform-specific rollback
        // which is implemented in their respective action modules
        ActionRecord::RegistryKeyCreated { root, key, on_uninstall } => {
            if !restore_backups && *on_uninstall == crate::config::types::UninstallBehavior::Nothing {
                return Ok(()); // Uninstall: leave key alone
            }
            #[cfg(windows)]
            {
                crate::actions::registry::delete_key(root, key)?;
            }
            Ok(())
        }
        ActionRecord::RegistryValueSet {
            root,
            key,
            value_name,
            previous_data,
            on_uninstall,
        } => {
            if !restore_backups && *on_uninstall == crate::config::types::UninstallBehavior::Nothing {
                return Ok(()); // Uninstall: leave value alone
            }
            #[cfg(windows)]
            {
                if restore_backups {
                    // Install rollback: restore previous value
                    if let Some(prev) = previous_data {
                        crate::actions::registry::set_string_value(root, key, value_name, prev)?;
                    } else {
                        crate::actions::registry::delete_value(root, key, value_name)?;
                    }
                } else {
                    // Uninstall: delete the value (RemoveKey or RemoveValues)
                    crate::actions::registry::delete_value(root, key, value_name)?;
                }
            }
            Ok(())
        }
        ActionRecord::EnvironmentVariableSet {
            name,
            scope,
            previous_value,
            ..
        } => {
            #[cfg(windows)]
            {
                crate::actions::environment::rollback_env_var(name, scope, previous_value.as_deref())?;
            }
            Ok(())
        }
        ActionRecord::ServiceInstalled { name } => {
            #[cfg(windows)]
            {
                let _ = crate::actions::services::stop_service(name);
                crate::actions::services::delete_service(name)?;
            }
            Ok(())
        }
        ActionRecord::ServiceStarted { name } => {
            #[cfg(windows)]
            {
                let _ = crate::actions::services::stop_service(name);
            }
            Ok(())
        }
        ActionRecord::AssociationCreated { extension, prog_id } => {
            #[cfg(windows)]
            {
                crate::actions::associations::remove_association(extension, prog_id)?;
            }
            Ok(())
        }
        ActionRecord::ComRegistered { file, action } => {
            #[cfg(windows)]
            {
                crate::actions::com::unregister(file, action)?;
            }
            Ok(())
        }
        ActionRecord::FontInstalled { file, font_name } => {
            #[cfg(windows)]
            {
                crate::actions::fonts::uninstall_font(file, font_name)?;
            }
            Ok(())
        }
        ActionRecord::CommandExecuted { .. } | ActionRecord::PermissionsSet { .. } => {
            // Commands can't be rolled back; permissions are best-effort
            Ok(())
        }
    }
}

#[cfg(windows)]
fn schedule_delete_on_reboot(path: &std::path::Path) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            wide.as_ptr(),
            std::ptr::null(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };

    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorAction;
    use crate::{InstallerError, Prompt, PromptResponse};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    struct TestCallbacks {
        progress_phases: Arc<Mutex<Vec<String>>>,
    }

    impl TestCallbacks {
        fn new() -> Self {
            Self {
                progress_phases: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl InstallerCallbacks for TestCallbacks {
        fn on_progress(&self, phase: &str, _current: u64, _total: u64) {
            self.progress_phases
                .lock()
                .unwrap()
                .push(phase.to_string());
        }
        fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
            PromptResponse::Yes
        }
        fn on_log(&self, _level: LogLevel, _message: &str) {}
        fn on_error(&self, _error: &InstallerError) -> ErrorAction {
            ErrorAction::Abort
        }
    }

    #[test]
    fn test_rollback_empty_actions() {
        let callbacks = TestCallbacks::new();
        let result = rollback_actions(&[], &callbacks, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_rollback_file_copied_removes_file() {
        let dir = std::env::temp_dir().join("outto_test_rollback_file");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let file = dir.join("test.txt");
        std::fs::write(&file, "content").unwrap();
        assert!(file.exists());

        let actions = vec![ActionRecord::FileCopied {
            dest: file.clone(),
            backup: None,
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        }];

        let callbacks = TestCallbacks::new();
        rollback_actions(&actions, &callbacks, true).unwrap();

        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_file_copied_restores_backup() {
        let dir = std::env::temp_dir().join("outto_test_rollback_backup");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let dest = dir.join("app.exe");
        let backup = dir.join("app.exe.bak");
        std::fs::write(&dest, "new content").unwrap();
        std::fs::write(&backup, "original content").unwrap();

        let actions = vec![ActionRecord::FileCopied {
            dest: dest.clone(),
            backup: Some(backup.clone()),
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        }];

        let callbacks = TestCallbacks::new();
        rollback_actions(&actions, &callbacks, true).unwrap();

        assert!(dest.exists());
        assert!(!backup.exists());
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "original content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_file_nonexistent_is_ok() {
        let actions = vec![ActionRecord::FileCopied {
            dest: PathBuf::from("C:\\nonexistent\\path\\file.txt"),
            backup: None,
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        }];

        let callbacks = TestCallbacks::new();
        let result = rollback_actions(&actions, &callbacks, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_rollback_directory_removes_empty_dir() {
        let dir = std::env::temp_dir().join("outto_test_rollback_emptydir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let actions = vec![ActionRecord::DirectoryCreated { path: dir.clone() }];

        let callbacks = TestCallbacks::new();
        rollback_actions(&actions, &callbacks, true).unwrap();

        assert!(!dir.exists());
    }

    #[test]
    fn test_rollback_directory_skips_nonempty() {
        let dir = std::env::temp_dir().join("outto_test_rollback_nonemptydir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), "keep me").unwrap();

        let actions = vec![ActionRecord::DirectoryCreated { path: dir.clone() }];

        let callbacks = TestCallbacks::new();
        rollback_actions(&actions, &callbacks, true).unwrap();

        // Directory still exists because it's not empty
        assert!(dir.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_shortcut_removes_file() {
        let dir = std::env::temp_dir().join("outto_test_rollback_shortcut");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let lnk = dir.join("Test.lnk");
        std::fs::write(&lnk, "fake shortcut").unwrap();

        let actions = vec![ActionRecord::ShortcutCreated { path: lnk.clone() }];

        let callbacks = TestCallbacks::new();
        rollback_actions(&actions, &callbacks, true).unwrap();

        assert!(!lnk.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_rollback_command_executed_is_noop() {
        let actions = vec![ActionRecord::CommandExecuted {
            command: "echo hello".to_string(),
            phase: "after_install".to_string(),
        }];

        let callbacks = TestCallbacks::new();
        let result = rollback_actions(&actions, &callbacks, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_rollback_permissions_set_is_noop() {
        let actions = vec![ActionRecord::PermissionsSet {
            path: PathBuf::from("C:\\test"),
            identity: "Users".to_string(),
            access: "read".to_string(),
        }];

        let callbacks = TestCallbacks::new();
        let result = rollback_actions(&actions, &callbacks, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_rollback_reverses_order() {
        let dir = std::env::temp_dir().join("outto_test_rollback_order");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create file inside dir — if rollback processes dir before file, it would
        // fail to remove the dir (not empty). Correct reverse order: file first, then dir.
        let file = dir.join("file.txt");
        std::fs::write(&file, "content").unwrap();

        let actions = vec![
            ActionRecord::DirectoryCreated { path: dir.clone() },
            ActionRecord::FileCopied {
                dest: file.clone(),
                backup: None,
                preserve_on_uninstall: false,
                uninst_remove_readonly: false,
                uninst_restart_delete: false,
                restart_replace: false,
            },
        ];

        let callbacks = TestCallbacks::new();
        let result = rollback_actions(&actions, &callbacks, true);
        assert!(result.is_ok());

        // File should be removed, and then dir should be removed (now empty)
        assert!(!file.exists());
        assert!(!dir.exists());
    }
}
