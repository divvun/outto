pub mod rollback;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{InstallerError, InstallerResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallManifest {
    pub package_id: String,
    pub package_name: String,
    pub package_version: String,
    pub install_dir: PathBuf,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub actions: Vec<ActionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ActionRecord {
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
    RegistryKeyCreated {
        root: String,
        key: String,
        on_uninstall: crate::config::types::UninstallBehavior,
    },
    RegistryValueSet {
        root: String,
        key: String,
        value_name: String,
        previous_data: Option<String>,
        on_uninstall: crate::config::types::UninstallBehavior,
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
    CommandExecuted {
        command: String,
        phase: String,
    },
    PermissionsSet {
        path: PathBuf,
        identity: String,
        access: String,
    },
}

impl InstallManifest {
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

    pub fn record(&mut self, action: ActionRecord) {
        self.actions.push(action);
    }

    pub fn package_dir(install_dir: &Path, package_id: &str) -> PathBuf {
        install_dir.join(".outto").join(package_id)
    }

    pub fn manifest_path(install_dir: &Path, package_id: &str) -> PathBuf {
        Self::package_dir(install_dir, package_id).join("manifest.json")
    }

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

    pub fn load(install_dir: &Path, package_id: &str) -> InstallerResult<Self> {
        let path = Self::manifest_path(install_dir, package_id);
        let content = fs::read_to_string(&path).map_err(|e| InstallerError::FileOp {
            path: path.clone(),
            source: e,
        })?;
        let manifest: Self = serde_json::from_str(&content)?;
        Ok(manifest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_manifest_roundtrip() {
        let dir = std::env::temp_dir().join("outto_test_manifest");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut manifest = InstallManifest::new("com.test", "Test", "1.0.0", &dir, vec![]);
        manifest.record(ActionRecord::DirectoryCreated {
            path: dir.join("subdir"),
        });
        manifest.record(ActionRecord::FileCopied {
            dest: dir.join("file.txt"),
            backup: None,
            preserve_on_uninstall: false,
            uninst_remove_readonly: false,
            uninst_restart_delete: false,
            restart_replace: false,
        });

        manifest.save().unwrap();

        let loaded = InstallManifest::load(&dir, "com.test").unwrap();
        assert_eq!(loaded.package_id, "com.test");
        assert_eq!(loaded.actions.len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_manifest_paths() {
        let dir = Path::new("C:\\Program Files\\MyApp");
        assert_eq!(
            InstallManifest::package_dir(dir, "com.example"),
            PathBuf::from("C:\\Program Files\\MyApp\\.outto\\com.example")
        );
        assert_eq!(
            InstallManifest::manifest_path(dir, "com.example"),
            PathBuf::from("C:\\Program Files\\MyApp\\.outto\\com.example\\manifest.json")
        );
    }
}
