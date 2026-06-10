pub mod rollback;

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{InstallerError, InstallerResult};

pub use rollback::{RollbackAction, rollback_actions};

/// Platform-neutral action variants that every platform backend must be able to
/// construct and record. Each platform defines its own top-level `Action` enum
/// that implements `From<CoreAction>` (so core action primitives can record into
/// it) and `RollbackAction` (so the platform owns the reverse semantics).
///
/// Kept minimal on purpose — more OS-specific records (registry, plists,
/// launchd, services, etc.) live in the platform crates' enums.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoreAction {
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
}

/// Generic install manifest parameterised over the platform's action enum.
///
/// The on-disk JSON stores `actions` as a flat array of whatever `A` serialises
/// to (typically `#[serde(tag = "type")]` tagged variants — the same wire format
/// the Windows backend has always used).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstallManifest<A> {
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
    pub install_dir: PathBuf,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub actions: Vec<A>,
}

impl<A> InstallManifest<A> {
    pub fn new(
        package_id: &str,
        package_name: &str,
        package_version: &str,
        install_dir: &Path,
        depends_on: Vec<String>,
    ) -> Self {
        Self {
            package_id: package_id.to_string(),
            package_name: package_name.to_string(),
            package_version: package_version.to_string(),
            install_dir: install_dir.to_path_buf(),
            depends_on,
            actions: Vec::new(),
        }
    }

    /// Record an action. Accepts anything convertible into `A`, so core's
    /// action modules can `manifest.record(CoreAction::FileCopied { ... })`
    /// and each platform's `From<CoreAction>` impl handles the conversion.
    pub fn record<R: Into<A>>(&mut self, action: R) {
        self.actions.push(action.into());
    }

    /// Default receipt location: `<install_dir>/.outto/<package_id>/`.
    /// Platforms that keep their receipt out-of-tree override this by calling
    /// `package_dir_with_base` instead.
    pub fn package_dir(install_dir: &Path, package_id: &str) -> PathBuf {
        install_dir.join(".outto").join(package_id)
    }

    /// Receipt location under a custom base directory — used by the macOS
    /// backend to store receipts at `~/Library/no.divvun.install/packages/<pkg-id>/`
    /// instead of inside the installed app bundle.
    pub fn package_dir_with_base(base: &Path, package_id: &str) -> PathBuf {
        base.join(package_id)
    }

    pub fn manifest_path(install_dir: &Path, package_id: &str) -> PathBuf {
        Self::package_dir(install_dir, package_id).join("manifest.json")
    }

    pub fn manifest_path_with_base(base: &Path, package_id: &str) -> PathBuf {
        Self::package_dir_with_base(base, package_id).join("manifest.json")
    }
}

impl<A: Serialize> InstallManifest<A> {
    pub fn save(&self) -> InstallerResult<()> {
        let dir = Self::package_dir(&self.install_dir, &self.package_id);
        fs::create_dir_all(&dir).map_err(|e| InstallerError::DirOp {
            path: dir.clone(),
            source: e,
        })?;

        let path = Self::manifest_path(&self.install_dir, &self.package_id);
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).map_err(|e| InstallerError::FileOp { path, source: e })?;

        Ok(())
    }

    /// Save the manifest to a custom base directory (used by macOS for
    /// `~/Library/no.divvun.install/packages/<pkg-id>/`).
    pub fn save_to(&self, base: &Path) -> InstallerResult<()> {
        let dir = Self::package_dir_with_base(base, &self.package_id);
        fs::create_dir_all(&dir).map_err(|e| InstallerError::DirOp {
            path: dir.clone(),
            source: e,
        })?;

        let path = Self::manifest_path_with_base(base, &self.package_id);
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).map_err(|e| InstallerError::FileOp { path, source: e })?;

        Ok(())
    }
}

impl<A: DeserializeOwned> InstallManifest<A> {
    pub fn load(install_dir: &Path, package_id: &str) -> InstallerResult<Self> {
        let path = Self::manifest_path(install_dir, package_id);
        Self::load_from_path(&path)
    }

    pub fn load_from_base(base: &Path, package_id: &str) -> InstallerResult<Self> {
        let path = Self::manifest_path_with_base(base, package_id);
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> InstallerResult<Self> {
        let content = fs::read_to_string(path).map_err(|e| InstallerError::FileOp {
            path: path.to_path_buf(),
            source: e,
        })?;
        let manifest: Self = serde_json::from_str(&content)?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_roundtrip_core_actions() {
        let dir = std::env::temp_dir().join("outto_test_manifest_core");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut manifest =
            InstallManifest::<CoreAction>::new("com.test", "Test", "1.0.0", &dir, vec![]);
        manifest.record(CoreAction::DirectoryCreated {
            path: dir.join("subdir"),
        });
        manifest.record(CoreAction::FileCopied {
            dest: dir.join("file.txt"),
            backup: None,
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        });

        manifest.save().unwrap();

        let loaded = InstallManifest::<CoreAction>::load(&dir, "com.test").unwrap();
        assert_eq!(loaded.package_id, "com.test");
        assert_eq!(loaded.actions.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }
}
