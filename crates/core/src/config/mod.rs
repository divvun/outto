pub mod paths;
pub mod types;
pub mod validate;

use serde::Deserialize;

use crate::error::{InstallerError, InstallerResult};
pub use paths::VariableResolver;
pub use types::*;
pub use validate::validate_config;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub package: PackageConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub reboot: RebootConfig,
    #[serde(default)]
    pub uninstall: UninstallConfig,
    #[serde(default)]
    pub upgrade: UpgradeConfig,
    #[serde(default)]
    pub components: Vec<ComponentEntry>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub dirs: Vec<DirEntry>,
    #[serde(default)]
    pub registry: Vec<RegistryEntry>,
    #[serde(default)]
    pub shortcuts: Vec<ShortcutEntry>,
    #[serde(default)]
    pub environment: Vec<EnvironmentEntry>,
    #[serde(default)]
    pub services: Vec<ServiceEntry>,
    #[serde(default)]
    pub associations: Vec<AssociationEntry>,
    #[serde(default)]
    pub prerequisites: Vec<PrerequisiteEntry>,
    #[serde(default)]
    pub run: Vec<RunEntry>,
    #[serde(default)]
    pub fonts: Vec<FontEntry>,
    #[serde(default)]
    pub com: Vec<ComEntry>,
    #[serde(default)]
    pub install_cleanup: InstallCleanup,
}

impl Config {
    pub fn from_toml(toml_str: &str) -> InstallerResult<Self> {
        let config: Config = toml::from_str(toml_str)?;
        validate_config(&config)?;
        Ok(config)
    }

    pub fn from_file(path: &std::path::Path) -> InstallerResult<Self> {
        let content = std::fs::read_to_string(path).map_err(|e| InstallerError::FileOp {
            path: path.to_path_buf(),
            source: e,
        })?;
        Self::from_toml(&content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[package]
id = "com.test.minimal"
name = "Minimal"
version = "0.1.0"
"#;
        let config = Config::from_toml(toml).unwrap();
        assert_eq!(config.files.len(), 0);
        assert_eq!(config.registry.len(), 0);
        assert!(!config.logging.enabled);
    }

    #[test]
    fn test_parse_invalid_toml() {
        let result = Config::from_toml("not valid toml [[[");
        assert!(result.is_err());
    }
}
