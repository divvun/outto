pub mod paths;
pub mod types;
pub mod validate;

use serde::Deserialize;

use crate::error::{InstallerError, InstallerResult};
pub use paths::PathResolver;
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
    fn test_parse_full_config() {
        let toml = r#"
[package]
id = "com.example.myapp"
name = "My Application"
version = "2.4.1"
publisher = "Example Corp"
url = "https://example.com"
support_url = "https://example.com/support"
license_file = "LICENSE.txt"
architecture = "x64"
privileges = "admin"
default_dir = "$pf/$package.name"

[logging]
enabled = true
path = "$app/install.log"

[reboot]
policy = "if_needed"
restart_manager = true

[uninstall]
display_icon = "$app/myapp.exe"
remove_app_dir = true
extra_dirs = ["$userappdata/MyApp"]

[upgrade]
policy = "overwrite"
preserve = ["config/*.toml", "data/**"]

[[components]]
name = "core"
display_name = "Core Application"
required = true
default = true

[[components]]
name = "extras"
display_name = "Extra Features"
required = false
default = false

[[files]]
source = "build/release/**/*"
dest = "$app"
overwrite = "if_newer"
component = "core"

[[files]]
source = "extras/*"
dest = "$app/extras"
overwrite = "always"
component = "extras"

[[dirs]]
path = "$app/logs"
permissions = [{ identity = "Users", access = "modify" }]

[[registry]]
root = "hklm"
key = "Software\\ExampleCorp\\MyApp"
values = [
    { name = "InstallPath", type = "string", data = "$app" },
    { name = "Version", type = "string", data = "$package.version" },
]
uninstall = "remove_key"

[[shortcuts]]
name = "My Application"
target = "$app/myapp.exe"
location = "start_menu"
icon = "$app/myapp.exe,0"
working_dir = "$app"

[[environment]]
name = "PATH"
value = "$app/bin"
scope = "system"
action = "append"

[[services]]
name = "myapp-daemon"
display_name = "MyApp Background Service"
executable = "$app/myapp-svc.exe"
start_type = "delayed_auto"
account = "LocalService"
on_install = "start"
on_uninstall = "stop_and_delete"

[[associations]]
extension = ".myf"
prog_id = "MyApp.Document"
description = "MyApp Document"
icon = "$app/myapp.exe,1"
command = "\"$app/myapp.exe\" \"%1\""

[[prerequisites]]
name = "VC++ 2022 Redistributable"
check = { registry = "HKLM\\SOFTWARE\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\X64", value = "Installed", equals = 1 }
download_url = "https://aka.ms/vs/17/release/vc_redist.x64.exe"
installer = "vc_redist.x64.exe"
arguments = "/install /quiet /norestart"
required = true

[[run]]
phase = "after_install"
command = "$app/myapp.exe"
arguments = "--init"
wait = true
show = "hidden"

[[fonts]]
source = "assets/CustomFont.ttf"

[[com]]
file = "$app/mylib.dll"
action = "regserver"
"#;
        let config = Config::from_toml(toml).unwrap();
        assert_eq!(config.package.id, "com.example.myapp");
        assert_eq!(config.package.name, "My Application");
        assert_eq!(config.package.version, "2.4.1");
        assert_eq!(config.package.architecture, Architecture::X64);
        assert_eq!(config.package.privileges, Privileges::Admin);
        assert_eq!(config.files.len(), 2);
        assert_eq!(config.components.len(), 2);
        assert_eq!(config.registry.len(), 1);
        assert_eq!(config.registry[0].values.len(), 2);
        assert_eq!(config.shortcuts.len(), 1);
        assert_eq!(config.environment.len(), 1);
        assert_eq!(config.services.len(), 1);
        assert_eq!(config.associations.len(), 1);
        assert_eq!(config.prerequisites.len(), 1);
        assert_eq!(config.run.len(), 1);
        assert_eq!(config.fonts.len(), 1);
        assert_eq!(config.com.len(), 1);
        assert!(config.logging.enabled);
        assert_eq!(config.reboot.policy, RebootPolicy::IfNeeded);
        assert!(config.uninstall.remove_app_dir);
        assert_eq!(config.upgrade.policy, UpgradePolicy::Overwrite);
    }

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
