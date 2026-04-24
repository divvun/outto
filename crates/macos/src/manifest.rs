//! macOS action enum for the install manifest.
//!
//! Mirrors the Windows pattern: one top-level `Action` enum containing every
//! mutation the macOS installer might record. `From<CoreAction>` lets neutral
//! action primitives in `outto-core` (file copy, directory create, command exec)
//! record into this enum via the generic manifest's `record` method.
//!
//! `RollbackAction` owns the reverse semantics; each variant is handled inline
//! or delegates to a module in `crate::actions::*`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use outto_core::callbacks::InstallerCallbacks;
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::rollback::{
    rollback_directory_created, rollback_file_copied, RollbackAction,
};
use outto_core::manifest::CoreAction;

/// Serde-compatible mirror of a plist::Value, used to capture the previous
/// value when overwriting a plist key so uninstall can restore it.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum PlistValueJson {
    String(String),
    Integer(i64),
    Real(f64),
    Bool(bool),
    Data(Vec<u8>),
    Array(Vec<PlistValueJson>),
    Dict(std::collections::BTreeMap<String, PlistValueJson>),
    /// Marker for "this value didn't exist before" — on rollback we delete the key.
    Absent,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Action {
    // --- Shared / neutral (mirror CoreAction fields) ---
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
    PermissionsSet {
        path: PathBuf,
        /// Octal mode (e.g., "755"). Empty string if only owner was set.
        mode: String,
        owner: Option<String>,
    },

    // --- macOS-specific ---
    SymlinkCreated {
        link: PathBuf,
        target: PathBuf,
        /// If the link existed before and we overwrote it, remember its old target
        /// so uninstall can restore it.
        previous_target: Option<PathBuf>,
    },
    PlistValueSet {
        path: PathBuf,
        /// Dotted key path (e.g. "Window.Size.Width").
        key_path: String,
        previous_value: PlistValueJson,
    },
    PlistFileCreated {
        path: PathBuf,
    },
    LaunchdPlistInstalled {
        label: String,
        plist_path: PathBuf,
        /// "agent" | "daemon"
        scope: String,
    },
    LaunchdServiceLoaded {
        label: String,
        /// "agent" | "daemon"
        scope: String,
    },
    FontInstalled {
        path: PathBuf,
        /// "user" | "system"
        scope: String,
    },
    ShellRcModified {
        rc_file: PathBuf,
        package_id: String,
    },
    LsregisterRan {
        app_path: PathBuf,
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
        callbacks: &dyn InstallerCallbacks,
    ) -> InstallerResult<()> {
        match self {
            Action::FileCopied {
                dest,
                backup,
                preserve_on_uninstall,
                uninst_remove_readonly,
                ..
            } => rollback_file_copied(
                dest,
                backup.as_deref(),
                *preserve_on_uninstall,
                *uninst_remove_readonly,
                restore_backups,
            ),
            Action::DirectoryCreated { path } => rollback_directory_created(path),
            Action::CommandExecuted { .. } | Action::PermissionsSet { .. } => Ok(()),
            Action::SymlinkCreated {
                link,
                previous_target,
                ..
            } => crate::actions::symlinks::rollback_symlink(link, previous_target.as_deref()),
            Action::PlistValueSet {
                path,
                key_path,
                previous_value,
            } => crate::actions::plist::rollback_value(path, key_path, previous_value),
            Action::PlistFileCreated { path } => {
                if path.exists() {
                    std::fs::remove_file(path).map_err(|e| InstallerError::FileOp {
                        path: path.clone(),
                        source: e,
                    })?;
                }
                Ok(())
            }
            Action::LaunchdPlistInstalled {
                label,
                plist_path,
                scope,
            } => crate::actions::launchd::rollback_plist_installed(
                label, plist_path, scope, callbacks,
            ),
            Action::LaunchdServiceLoaded { label, scope } => {
                crate::actions::launchd::rollback_service_loaded(label, scope, callbacks)
            }
            Action::FontInstalled { path, .. } => {
                if path.exists() {
                    std::fs::remove_file(path).map_err(|e| InstallerError::FileOp {
                        path: path.clone(),
                        source: e,
                    })?;
                }
                Ok(())
            }
            Action::ShellRcModified {
                rc_file,
                package_id,
            } => crate::actions::environment::remove_guarded_block(rc_file, package_id),
            Action::LsregisterRan { .. } => Ok(()), // no-op — the reverse (unregister) is handled by the uninstaller removing the .app itself
        }
    }
}
