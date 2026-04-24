pub mod types;

use serde::Deserialize;

use outto_core::error::{InstallerError, InstallerResult};

pub use types::*;

/// The macOS-schema config. Parsed from `outto.macos.toml`.
///
/// Intentionally separate from the Windows `outto_core::Config` — the two
/// platforms have very different installer primitives (registry/COM/services
/// vs. launchd/plist/symlinks/bundles) and trying to merge them into one
/// schema made every section feel awkward.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub package: PackageConfig,

    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub upgrade: UpgradeConfig,
    #[serde(default)]
    pub uninstall: UninstallConfig,
    #[serde(default)]
    pub privileges: PrivilegesConfig,
    #[serde(default)]
    pub install_cleanup: InstallCleanup,

    #[serde(default)]
    pub components: Vec<ComponentEntry>,
    #[serde(default)]
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub dirs: Vec<DirEntry>,
    #[serde(default)]
    pub symlinks: Vec<SymlinkEntry>,
    #[serde(default)]
    pub plist: Vec<PlistEntry>,
    #[serde(default)]
    pub launchd: Vec<LaunchdEntry>,
    #[serde(default)]
    pub associations: Vec<AssociationEntry>,
    #[serde(default)]
    pub fonts: Vec<FontEntry>,
    #[serde(default)]
    pub environment: Vec<EnvironmentEntry>,
    #[serde(default)]
    pub prerequisites: Vec<PrerequisiteEntry>,
    #[serde(default)]
    pub run: Vec<RunEntry>,
}

impl Config {
    pub fn from_toml(toml_str: &str) -> InstallerResult<Self> {
        let config: Config = toml::from_str(toml_str)?;
        validate(&config)?;
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

fn validate(config: &Config) -> InstallerResult<()> {
    if config.package.id.is_empty() {
        return Err(InstallerError::Validation(
            "package.id must not be empty".into(),
        ));
    }
    if config.package.name.is_empty() {
        return Err(InstallerError::Validation(
            "package.name must not be empty".into(),
        ));
    }
    if config.package.version.is_empty() {
        return Err(InstallerError::Validation(
            "package.version must not be empty".into(),
        ));
    }
    if semver::Version::parse(&config.package.version).is_err() {
        return Err(InstallerError::Validation(format!(
            "package.version '{}' is not valid semver",
            config.package.version
        )));
    }

    // Bundle id should look like a reverse-DNS string; enforce a non-empty dot.
    if !config.package.id.contains('.') {
        return Err(InstallerError::Validation(format!(
            "package.id '{}' must be a reverse-DNS identifier (e.g. \"no.divvun.myapp\")",
            config.package.id
        )));
    }

    // Duplicate component names.
    let mut seen = std::collections::HashSet::new();
    for c in &config.components {
        if !seen.insert(&c.name) {
            return Err(InstallerError::Validation(format!(
                "duplicate component name: '{}'",
                c.name
            )));
        }
    }

    for (i, f) in config.files.iter().enumerate() {
        if f.source.is_empty() {
            return Err(InstallerError::Validation(format!(
                "files[{i}].source must not be empty"
            )));
        }
        if f.dest.is_empty() {
            return Err(InstallerError::Validation(format!(
                "files[{i}].dest must not be empty"
            )));
        }
    }

    for (i, s) in config.symlinks.iter().enumerate() {
        if s.target.is_empty() {
            return Err(InstallerError::Validation(format!(
                "symlinks[{i}].target must not be empty"
            )));
        }
        if s.link.is_empty() {
            return Err(InstallerError::Validation(format!(
                "symlinks[{i}].link must not be empty"
            )));
        }
    }

    for (i, l) in config.launchd.iter().enumerate() {
        if l.label.is_empty() {
            return Err(InstallerError::Validation(format!(
                "launchd[{i}].label must not be empty"
            )));
        }
        if !l.label.contains('.') {
            return Err(InstallerError::Validation(format!(
                "launchd[{i}].label '{}' must be a reverse-DNS identifier",
                l.label
            )));
        }
        if l.program.is_empty() {
            return Err(InstallerError::Validation(format!(
                "launchd[{i}].program must not be empty"
            )));
        }
    }

    for (i, p) in config.plist.iter().enumerate() {
        if p.path.is_empty() {
            return Err(InstallerError::Validation(format!(
                "plist[{i}].path must not be empty"
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> &'static str {
        r#"
[package]
id = "no.divvun.test"
name = "Test"
version = "1.0.0"
"#
    }

    #[test]
    fn test_parse_minimal() {
        let c = Config::from_toml(minimal()).unwrap();
        assert_eq!(c.package.id, "no.divvun.test");
        assert!(c.files.is_empty());
    }

    #[test]
    fn test_rejects_non_reverse_dns_id() {
        let toml = r##"
[package]
id = "test"
name = "Test"
version = "1.0.0"
"##;
        assert!(Config::from_toml(toml).is_err());
    }

    #[test]
    fn test_parse_files_with_bundle_flag() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[files]]
source = "MyApp.app"
dest = "#{applications}"
bundle = true
overwrite = "always"
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.files.len(), 1);
        assert!(c.files[0].bundle);
        assert_eq!(c.files[0].overwrite, OverwritePolicy::Always);
    }

    #[test]
    fn test_parse_symlinks() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[symlinks]]
target = "#{app}/Contents/MacOS/myapp"
link = "#{local_bin}/myapp"
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.symlinks.len(), 1);
        assert_eq!(c.symlinks[0].link, "#{local_bin}/myapp");
    }

    #[test]
    fn test_parse_plist() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[plist]]
path = "#{prefs_user}/no.divvun.myapp.plist"
uninstall = "remove_file"

[[plist.values]]
key = "AppVersion"
type = "string"
data = "1.0.0"

[[plist.values]]
key = "FirstLaunchDate"
type = "integer"
data = 0
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.plist.len(), 1);
        assert_eq!(c.plist[0].values.len(), 2);
        assert_eq!(c.plist[0].values[0].key, "AppVersion");
        assert_eq!(c.plist[0].values[0].value_type, PlistValueType::String);
        assert_eq!(c.plist[0].uninstall, PlistUninstall::RemoveFile);
    }

    #[test]
    fn test_parse_launchd_agent() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[launchd]]
label = "no.divvun.myapp.updater"
scope = "agent"
program = "#{app}/Contents/MacOS/myapp-updater"
program_arguments = ["--daemon"]
run_at_load = true
keep_alive = false
on_install = "load"
on_uninstall = "unload_and_remove"
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.launchd.len(), 1);
        assert_eq!(c.launchd[0].scope, LaunchdScope::Agent);
        assert_eq!(c.launchd[0].program_arguments, vec!["--daemon".to_string()]);
    }

    #[test]
    fn test_parse_launchd_daemon_with_user() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[launchd]]
label = "no.divvun.myapp.daemon"
scope = "daemon"
program = "/usr/local/bin/myappd"
user_name = "_myapp"
group_name = "_myapp"
keep_alive = true
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.launchd[0].scope, LaunchdScope::Daemon);
        assert_eq!(c.launchd[0].user_name.as_deref(), Some("_myapp"));
        assert!(c.launchd[0].keep_alive);
    }

    #[test]
    fn test_parse_fonts() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[fonts]]
source = "assets/DivvunSans.ttf"
scope = "user"
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.fonts.len(), 1);
        assert_eq!(c.fonts[0].scope, FontScope::User);
    }

    #[test]
    fn test_parse_environment() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[environment]]
name = "MYAPP_HOME"
value = "#{app}/Contents/Resources"
action = "set"
shells = ["zsh", "bash"]
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.environment[0].name, "MYAPP_HOME");
        assert_eq!(c.environment[0].action, EnvAction::Set);
        assert_eq!(c.environment[0].shells, vec![Shell::Zsh, Shell::Bash]);
    }

    #[test]
    fn test_parse_privileges_default() {
        let c = Config::from_toml(minimal()).unwrap();
        assert_eq!(c.privileges.required, RequiredPrivileges::User);
        assert!(c.privileges.auto_elevate);
    }

    #[test]
    fn test_parse_privileges_admin() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[privileges]
required = "admin"
auto_elevate = true
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.privileges.required, RequiredPrivileges::Admin);
    }

    #[test]
    fn test_parse_prerequisites_command() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.0.0"

[[prerequisites]]
name = "Xcode CLI Tools"
check = { command = "xcode-select -p" }
required = true
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.prerequisites.len(), 1);
        assert_eq!(
            c.prerequisites[0].check.command.as_deref(),
            Some("xcode-select -p")
        );
    }

    #[test]
    fn test_parse_full_example() {
        let toml = r##"
[package]
id = "no.divvun.myapp"
name = "MyApp"
version = "1.2.3"
publisher = "Divvun"
url = "https://divvun.no"
license_file = "LICENSE.txt"
default_dir = "#{applications}/MyApp.app"
min_macos_version = "12.0"
depends_on = ["no.divvun.runtime"]

[privileges]
required = "user"
auto_elevate = true

[upgrade]
policy = "overwrite"
preserve = ["Contents/Resources/prefs.plist"]

[[components]]
name = "core"
required = true
default = true

[[components]]
name = "cli"
display_name = "Command-line tool"
default = true

[[files]]
source = "MyApp.app"
dest = "#{applications}"
bundle = true
overwrite = "always"
component = "core"

[[symlinks]]
target = "#{app}/Contents/MacOS/myapp"
link = "#{local_bin}/myapp"
component = "cli"

[[plist]]
path = "#{prefs_user}/no.divvun.myapp.plist"

[[plist.values]]
key = "AppVersion"
type = "string"
data = "#{package.version}"

[[launchd]]
label = "no.divvun.myapp.updater"
scope = "agent"
program = "#{app}/Contents/MacOS/myapp-updater"
run_at_load = true

[[associations]]
app_path = "#{app}"
lsregister = true

[[fonts]]
source = "assets/DivvunSans.ttf"
scope = "user"

[[run]]
phase = "after_install"
command = "echo"
arguments = "installed"
wait = true

[[prerequisites]]
name = "Xcode CLI Tools"
check = { command = "xcode-select -p" }
required = true

[uninstall]
display_icon = "#{app}/Contents/Resources/AppIcon.icns"
remove_app_dir = true
"##;
        let c = Config::from_toml(toml).unwrap();
        assert_eq!(c.package.name, "MyApp");
        assert_eq!(c.components.len(), 2);
        assert_eq!(c.files.len(), 1);
        assert!(c.files[0].bundle);
        assert_eq!(c.symlinks.len(), 1);
        assert_eq!(c.plist.len(), 1);
        assert_eq!(c.launchd.len(), 1);
        assert_eq!(c.associations.len(), 1);
        assert_eq!(c.fonts.len(), 1);
        assert_eq!(c.run.len(), 1);
        assert_eq!(c.prerequisites.len(), 1);
    }
}
