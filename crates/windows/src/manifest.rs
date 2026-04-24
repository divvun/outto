//! Windows action enum and rollback logic.
//!
//! `Action` is the Windows backend's top-level manifest record type — every
//! mutation the Windows installer makes gets recorded as a variant of this.
//! `impl From<CoreAction>` lets the neutral action primitives in `outto-core`
//! (file copy, directory create, command-exec) record into this enum via
//! `manifest.record(CoreAction::X)`. `impl RollbackAction` owns the reverse
//! semantics, dispatching registry/service/COM/etc. rollbacks into the
//! relevant sub-module.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::types::UninstallBehavior;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::rollback::{
    rollback_directory_created, rollback_file_copied, RollbackAction,
};
use outto_core::manifest::CoreAction;

use crate::actions::{associations, com, environment, fonts, registry, services};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    FileCopied {
        dest: PathBuf,
        backup: Option<PathBuf>,
        #[serde(default)]
        preserve_on_uninstall: bool,
        #[serde(default)]
        uninst_remove_readonly: bool,
        #[serde(default)]
        uninst_restart_delete: bool,
        #[serde(default)]
        restart_replace: bool,
    },
    DirectoryCreated {
        path: PathBuf,
    },
    CommandExecuted {
        command: String,
        phase: String,
    },
    RegistryKeyCreated {
        root: String,
        key: String,
        on_uninstall: UninstallBehavior,
    },
    RegistryValueSet {
        root: String,
        key: String,
        value_name: String,
        previous_data: Option<String>,
        on_uninstall: UninstallBehavior,
    },
    ShortcutCreated {
        path: PathBuf,
    },
    EnvironmentVariableSet {
        name: String,
        scope: String,
        action: String,
        value: String,
        previous_value: Option<String>,
    },
    ServiceInstalled {
        name: String,
    },
    ServiceStarted {
        name: String,
    },
    AssociationCreated {
        extension: String,
        prog_id: String,
    },
    ComRegistered {
        file: PathBuf,
        action: String,
    },
    FontInstalled {
        file: PathBuf,
        font_name: String,
    },
    PermissionsSet {
        path: PathBuf,
        identity: String,
        access: String,
    },
}

impl From<CoreAction> for Action {
    fn from(core: CoreAction) -> Self {
        match core {
            CoreAction::FileCopied {
                dest,
                backup,
                preserve_on_uninstall,
                uninst_remove_readonly,
                uninst_restart_delete,
                restart_replace,
            } => Action::FileCopied {
                dest,
                backup,
                preserve_on_uninstall,
                uninst_remove_readonly,
                uninst_restart_delete,
                restart_replace,
            },
            CoreAction::DirectoryCreated { path } => Action::DirectoryCreated { path },
            CoreAction::CommandExecuted { command, phase } => {
                Action::CommandExecuted { command, phase }
            }
        }
    }
}

impl RollbackAction for Action {
    fn rollback(
        &self,
        restore_backups: bool,
        _callbacks: &dyn InstallerCallbacks,
    ) -> InstallerResult<()> {
        match self {
            Action::FileCopied {
                dest,
                backup,
                preserve_on_uninstall,
                uninst_remove_readonly,
                uninst_restart_delete,
                restart_replace: _,
            } => {
                if !restore_backups && *preserve_on_uninstall {
                    return Ok(());
                }

                if !restore_backups && *uninst_remove_readonly {
                    if let Ok(metadata) = std::fs::metadata(dest) {
                        let mut perms = metadata.permissions();
                        if perms.readonly() {
                            perms.set_readonly(false);
                            let _ = std::fs::set_permissions(dest, perms);
                        }
                    }
                }

                if !restore_backups && *uninst_restart_delete {
                    let _ = schedule_delete_on_reboot(dest);
                    if let Some(backup_path) = backup {
                        if backup_path.exists() {
                            let _ = std::fs::remove_file(backup_path);
                        }
                    }
                    return Ok(());
                }

                // Try straightforward remove first; fall back to delete-on-reboot if locked.
                if dest.exists() {
                    if let Err(e) = std::fs::remove_file(dest) {
                        if schedule_delete_on_reboot(dest).is_err() {
                            return Err(InstallerError::FileOp {
                                path: dest.clone(),
                                source: e,
                            });
                        }
                    }
                }

                if let Some(backup_path) = backup {
                    if restore_backups {
                        if backup_path.exists() {
                            std::fs::rename(backup_path, dest).map_err(|e| {
                                InstallerError::FileOp {
                                    path: dest.clone(),
                                    source: e,
                                }
                            })?;
                        }
                    } else if backup_path.exists() {
                        let _ = std::fs::remove_file(backup_path);
                    }
                }
                Ok(())
            }
            Action::DirectoryCreated { path } => rollback_directory_created(path),
            Action::CommandExecuted { .. } | Action::PermissionsSet { .. } => Ok(()),
            Action::ShortcutCreated { path } => {
                if path.exists() {
                    std::fs::remove_file(path).map_err(|e| InstallerError::FileOp {
                        path: path.clone(),
                        source: e,
                    })?;
                }
                Ok(())
            }
            Action::RegistryKeyCreated {
                root,
                key,
                on_uninstall,
            } => {
                if !restore_backups && *on_uninstall == UninstallBehavior::Nothing {
                    return Ok(());
                }
                registry::delete_key(root, key)
            }
            Action::RegistryValueSet {
                root,
                key,
                value_name,
                previous_data,
                on_uninstall,
            } => {
                if !restore_backups && *on_uninstall == UninstallBehavior::Nothing {
                    return Ok(());
                }
                if restore_backups {
                    if let Some(prev) = previous_data {
                        registry::set_string_value(root, key, value_name, prev)?;
                    } else {
                        registry::delete_value(root, key, value_name)?;
                    }
                } else {
                    registry::delete_value(root, key, value_name)?;
                }
                Ok(())
            }
            Action::EnvironmentVariableSet {
                name,
                scope,
                action,
                value,
                previous_value,
            } => {
                environment::rollback_env_var(name, scope, action, value, previous_value.as_deref())
            }
            Action::ServiceInstalled { name } => {
                let _ = services::stop_service(name);
                services::delete_service(name)?;
                Ok(())
            }
            Action::ServiceStarted { name } => {
                let _ = services::stop_service(name);
                Ok(())
            }
            Action::AssociationCreated { extension, prog_id } => {
                associations::remove_association(extension, prog_id)
            }
            Action::ComRegistered { file, action } => com::unregister(file, action),
            Action::FontInstalled { file, font_name } => fonts::uninstall_font(file, font_name),
        }
    }
}

// Unused-import guard — keep LogLevel in the import set so future log-heavy
// rollback impls don't need to re-add it.
#[allow(dead_code)]
const _LOG_LEVEL_KEEP: Option<LogLevel> = None;
#[allow(dead_code)]
fn _rollback_file_copied_keep() {
    let _ = rollback_file_copied;
}

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
