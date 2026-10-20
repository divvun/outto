use crate::config::Config;
use crate::config::types::{Privileges, ShortcutScope};
use crate::error::{InstallerError, InstallerResult};

pub fn validate_config(config: &Config) -> InstallerResult<()> {
    validate_package(config)?;
    validate_components(config)?;
    validate_files(config)?;
    validate_registry(config)?;
    validate_shortcuts(config)?;
    validate_services(config)?;
    validate_associations(config)?;
    validate_upgrade(config)?;
    validate_uninstall(config)?;
    validate_run(config)?;
    Ok(())
}

fn validate_run(config: &Config) -> InstallerResult<()> {
    for (i, r) in config.run.iter().enumerate() {
        crate::actions::run::validate_log_args(
            &r.command,
            r.arguments.as_deref(),
            r.working_dir.as_deref(),
        )
        .map_err(|e| InstallerError::Validation(format!("run[{i}]: {e}")))?;
    }
    Ok(())
}

/// Split a registry path such as `HKLM\SOFTWARE\Foo` into its root, as
/// `HKLM`, `HKCU` or `HKCR`, and the key under it. The long root names
/// (`HKEY_LOCAL_MACHINE`, ...) are accepted too; `None` for anything else.
pub fn split_registry_key(path: &str) -> Option<(&'static str, &str)> {
    let (root, key) = path.split_once('\\')?;
    let root = match root.to_ascii_uppercase().as_str() {
        "HKLM" | "HKEY_LOCAL_MACHINE" => "HKLM",
        "HKCU" | "HKEY_CURRENT_USER" => "HKCU",
        "HKCR" | "HKEY_CLASSES_ROOT" => "HKCR",
        _ => return None,
    };
    (!key.is_empty()).then_some((root, key))
}

fn check_registry_key(path: &str, context: &str) -> InstallerResult<()> {
    if split_registry_key(path).is_none() {
        return Err(InstallerError::Validation(format!(
            "{context} '{path}' must be a registry key under HKLM, HKCU or HKCR, \
             e.g. HKLM\\SOFTWARE\\Example"
        )));
    }
    Ok(())
}

fn validate_upgrade(config: &Config) -> InstallerResult<()> {
    if let Some(probe) = &config.upgrade.installed_version {
        check_registry_key(
            &probe.registry_key,
            "upgrade.installed_version.registry_key",
        )?;
    }
    for (i, dir) in config.upgrade.prune.iter().enumerate() {
        if dir.trim().is_empty() {
            return Err(InstallerError::Validation(format!(
                "upgrade.prune[{i}] must not be empty"
            )));
        }
    }
    Ok(())
}

fn validate_uninstall(config: &Config) -> InstallerResult<()> {
    for (i, refusal) in config.uninstall.refuse_while.iter().enumerate() {
        check_registry_key(
            &refusal.registry_key,
            &format!("uninstall.refuse_while[{i}].registry_key"),
        )?;
    }
    Ok(())
}

fn validate_package(config: &Config) -> InstallerResult<()> {
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
    if let Some(guid) = &config.package.legacy_inno_app_id {
        if !is_bare_guid(guid) {
            return Err(InstallerError::Validation(format!(
                "package.legacy_inno_app_id '{guid}' must be a GUID without braces, \
                 e.g. 8A3F2C1D-0B4E-4F6A-9C7D-1E2F3A4B5C6D"
            )));
        }
    }
    Ok(())
}

fn is_bare_guid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, len)| g.len() == len && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn validate_components(config: &Config) -> InstallerResult<()> {
    let component_names: Vec<&str> = config.components.iter().map(|c| c.name.as_str()).collect();

    let mut seen = std::collections::HashSet::new();
    for name in &component_names {
        if !seen.insert(name) {
            return Err(InstallerError::Validation(format!(
                "duplicate component name: '{name}'"
            )));
        }
    }

    let check_component = |comp: &Option<String>, context: &str| -> InstallerResult<()> {
        if let Some(name) = comp {
            if !component_names.contains(&name.as_str()) && !component_names.is_empty() {
                return Err(InstallerError::Validation(format!(
                    "{context} references unknown component '{name}'"
                )));
            }
        }
        Ok(())
    };

    for (i, f) in config.files.iter().enumerate() {
        check_component(&f.component, &format!("files[{i}]"))?;
    }
    for (i, d) in config.dirs.iter().enumerate() {
        check_component(&d.component, &format!("dirs[{i}]"))?;
    }
    for (i, r) in config.registry.iter().enumerate() {
        check_component(&r.component, &format!("registry[{i}]"))?;
    }
    for (i, s) in config.shortcuts.iter().enumerate() {
        check_component(&s.component, &format!("shortcuts[{i}]"))?;
    }

    Ok(())
}

fn validate_files(config: &Config) -> InstallerResult<()> {
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
    Ok(())
}

fn validate_registry(config: &Config) -> InstallerResult<()> {
    for (i, r) in config.registry.iter().enumerate() {
        if r.key.is_empty() {
            return Err(InstallerError::Validation(format!(
                "registry[{i}].key must not be empty"
            )));
        }
    }
    Ok(())
}

fn validate_shortcuts(config: &Config) -> InstallerResult<()> {
    for (i, s) in config.shortcuts.iter().enumerate() {
        if s.name.is_empty() {
            return Err(InstallerError::Validation(format!(
                "shortcuts[{i}].name must not be empty"
            )));
        }
        if s.target.is_empty() {
            return Err(InstallerError::Validation(format!(
                "shortcuts[{i}].target must not be empty"
            )));
        }
        // A package that never elevates can't write to the all-users folders.
        if s.scope == Some(ShortcutScope::AllUsers) && config.package.privileges == Privileges::User
        {
            return Err(InstallerError::Validation(format!(
                "shortcuts[{i}]: scope = \"all_users\" needs administrator privileges, \
                 and the package has privileges = \"user\""
            )));
        }
        if let Some(subfolder) = &s.subfolder {
            let relative = !subfolder.contains(':')
                && subfolder
                    .split(['/', '\\'])
                    .all(|part| !part.is_empty() && part != "." && part != "..");
            if !relative {
                return Err(InstallerError::Validation(format!(
                    "shortcuts[{i}].subfolder must be a relative path without `.` or `..`, \
                     not {subfolder:?}"
                )));
            }
        }
    }
    Ok(())
}

fn validate_services(config: &Config) -> InstallerResult<()> {
    for (i, s) in config.services.iter().enumerate() {
        if s.name.is_empty() {
            return Err(InstallerError::Validation(format!(
                "services[{i}].name must not be empty"
            )));
        }
        if s.executable.is_empty() {
            return Err(InstallerError::Validation(format!(
                "services[{i}].executable must not be empty"
            )));
        }
    }
    Ok(())
}

fn validate_associations(config: &Config) -> InstallerResult<()> {
    for (i, a) in config.associations.iter().enumerate() {
        if !a.extension.starts_with('.') {
            return Err(InstallerError::Validation(format!(
                "associations[{i}].extension '{}' must start with '.'",
                a.extension
            )));
        }
        if a.prog_id.is_empty() {
            return Err(InstallerError::Validation(format!(
                "associations[{i}].prog_id must not be empty"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn minimal_toml() -> &'static str {
        r#"
[package]
id = "com.test.app"
name = "TestApp"
version = "1.0.0"
"#
    }

    #[test]
    fn test_valid_minimal_config() {
        let config: Config = toml::from_str(minimal_toml()).unwrap();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_empty_package_id() {
        let config: Config = toml::from_str(
            r#"
[package]
id = ""
name = "TestApp"
version = "1.0.0"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_invalid_version() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "not-semver"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_version_with_prerelease() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0-beta.1"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_legacy_inno_app_id() {
        let parse = |guid: &str| -> Config {
            toml::from_str(&format!(
                "{}legacy_inno_app_id = \"{guid}\"\n",
                minimal_toml()
            ))
            .unwrap()
        };
        assert!(validate_config(&parse("8A3F2C1D-0B4E-4F6A-9C7D-1E2F3A4B5C6D")).is_ok());
        assert!(validate_config(&parse("8a3f2c1d-0b4e-4f6a-9c7d-1e2f3a4b5c6d")).is_ok());
        assert!(validate_config(&parse("{8A3F2C1D-0B4E-4F6A-9C7D-1E2F3A4B5C6D}")).is_err());
        assert!(validate_config(&parse("8A3F2C1D-0B4E-4F6A-9C7D")).is_err());
        assert!(validate_config(&parse("MyApp")).is_err());
    }

    #[test]
    fn test_run_log_args() {
        let parse = |run: &str| Config::from_toml(&format!("{}\n[[run]]\n{run}\n", minimal_toml()));
        assert!(
            parse(
                r##"phase = "after_install"
command = "#{app}/tip-setup.exe"
arguments = "/VERYSILENT #{log_arg:text-service}""##
            )
            .is_ok()
        );
        for run in [
            r##"phase = "after_install"
command = "#{app}/x.exe"
arguments = "/LOG=#{log_arg:x}""##,
            r##"phase = "after_install"
command = "#{app}/x.exe"
arguments = "#{log_arg:a b}""##,
            r##"phase = "after_install"
command = "#{log_arg:x}""##,
            r##"phase = "after_install"
command = "#{app}/x.exe"
working_dir = "#{log_arg:x}""##,
        ] {
            let err = parse(run).err().unwrap_or_else(|| panic!("{run}"));
            assert!(err.to_string().contains("run[0]"), "{err}");
        }
    }

    #[test]
    fn test_split_registry_key() {
        assert_eq!(
            split_registry_key(r"HKLM\SOFTWARE\Classes"),
            Some(("HKLM", r"SOFTWARE\Classes"))
        );
        assert_eq!(
            split_registry_key(r"hkey_current_user\Software"),
            Some(("HKCU", "Software"))
        );
        assert_eq!(split_registry_key(r"HKCR\.txt"), Some(("HKCR", ".txt")));
        assert_eq!(split_registry_key(r"HKLM\"), None);
        assert_eq!(split_registry_key("HKLM"), None);
        assert_eq!(split_registry_key(r"HKU\S-1-5-18"), None);
        assert_eq!(split_registry_key("SOFTWARE/Foo"), None);
    }

    #[test]
    fn test_upgrade_and_uninstall_sections() {
        let config = Config::from_toml(
            r##"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"
legacy_inno_app_id = "8A3F2C1D-0B4E-4F6A-9C7D-1E2F3A4B5C6D"
legacy_inno_mode = "adopt"

[upgrade]
downgrade = "refuse"
prune = ["#{app}"]

[upgrade.installed_version]
registry_key = 'HKLM\SOFTWARE\Classes\CLSID\{4F600265-E72D-4131-8FBC-D56D08E17227}\InprocServer32'
from = "parent_dir_name"

[[uninstall.refuse_while]]
registry_key = 'HKLM\SOFTWARE\Microsoft\CTF\TIP\{4F600265-E72D-4131-8FBC-D56D08E17227}\LanguageProfile\*\*'
message = "Keyboards still use it."

[[com]]
file = "#{app}/x86.dll"
action = "regserver"
host = "x86"

[[com]]
file = "#{app}/arm64.dll"
action = "regserver"
arch = "arm64"
"##,
        )
        .unwrap();
        use crate::config::types::*;
        assert_eq!(config.package.legacy_inno_mode, LegacyInnoMode::Adopt);
        assert_eq!(config.upgrade.downgrade, DowngradePolicy::Refuse);
        assert_eq!(config.upgrade.prune, ["#{app}"]);
        let probe = config.upgrade.installed_version.unwrap();
        assert_eq!(probe.from, InstalledVersionFrom::ParentDirName);
        assert_eq!(probe.value, None);
        assert_eq!(config.uninstall.refuse_while.len(), 1);
        assert!(
            config.uninstall.refuse_while[0]
                .registry_key
                .ends_with(r"\*\*")
        );
        assert_eq!(config.com[0].host, ComHost::X86);
        assert_eq!(config.com[0].arch, None);
        assert_eq!(config.com[1].host, ComHost::Native);
        assert_eq!(config.com[1].arch, Some(Architecture::Arm64));

        let defaults = Config::from_toml(minimal_toml()).unwrap();
        assert_eq!(defaults.package.legacy_inno_mode, LegacyInnoMode::Uninstall);
        assert_eq!(defaults.upgrade.downgrade, DowngradePolicy::Allow);
        assert!(defaults.upgrade.prune.is_empty());
        assert!(defaults.uninstall.refuse_while.is_empty());
    }

    #[test]
    fn test_registry_keys_need_a_root() {
        let parse = |section: &str| Config::from_toml(&format!("{}{section}", minimal_toml()));
        assert!(parse("[[uninstall.refuse_while]]\nregistry_key = 'SOFTWARE\\Foo'\n").is_err());
        assert!(parse("[upgrade.installed_version]\nregistry_key = 'HKXX\\Foo'\n").is_err());
        assert!(parse("[upgrade]\nprune = [\"\"]\n").is_err());
        assert!(parse("[[uninstall.refuse_while]]\nregistry_key = 'HKCU\\Foo\\*'\n").is_ok());
    }

    fn with_privileges(privileges: &str, section: &str) -> InstallerResult<Config> {
        Config::from_toml(&format!(
            "{}privileges = \"{privileges}\"\n{section}",
            minimal_toml()
        ))
    }

    #[test]
    fn test_shortcut_scope_defaults_to_the_package_privileges() {
        use crate::config::types::*;
        let shortcut = "[[shortcuts]]\nname = \"App\"\ntarget = \"#{app}/app.exe\"\n\
                        location = \"start_menu\"\n";
        for (privileges, want) in [
            ("admin", ShortcutScope::AllUsers),
            ("user", ShortcutScope::CurrentUser),
            ("auto", ShortcutScope::CurrentUser),
        ] {
            let config = with_privileges(privileges, shortcut).unwrap();
            let entry = &config.shortcuts[0];
            assert_eq!(entry.scope, None);
            assert_eq!(
                entry.scope(&config.package.privileges),
                want,
                "{privileges}"
            );
        }
        let unset = Config::from_toml(&format!("{}{shortcut}", minimal_toml())).unwrap();
        assert_eq!(unset.package.privileges, Privileges::Auto);
        assert_eq!(
            unset.shortcuts[0].scope(&unset.package.privileges),
            ShortcutScope::CurrentUser
        );
    }

    #[test]
    fn test_shortcut_scope_override() {
        use crate::config::types::*;
        let shortcut = |scope: &str| {
            format!(
                "[[shortcuts]]\nname = \"App\"\ntarget = \"#{{app}}/app.exe\"\n\
                 location = \"desktop\"\nscope = \"{scope}\"\n"
            )
        };
        let admin = with_privileges("admin", &shortcut("current_user")).unwrap();
        assert_eq!(admin.shortcuts[0].scope, Some(ShortcutScope::CurrentUser));
        assert_eq!(
            admin.shortcuts[0].scope(&admin.package.privileges),
            ShortcutScope::CurrentUser
        );
        let auto = with_privileges("auto", &shortcut("all_users")).unwrap();
        assert_eq!(
            auto.shortcuts[0].scope(&auto.package.privileges),
            ShortcutScope::AllUsers
        );
        assert!(with_privileges("user", &shortcut("current_user")).is_ok());

        let err = with_privileges("user", &shortcut("all_users")).unwrap_err();
        assert!(err.to_string().contains("shortcuts[0]"), "{err}");
        assert!(with_privileges("admin", &shortcut("everyone")).is_err());
    }

    #[test]
    fn test_shortcut_folder_variables() {
        use crate::config::types::*;
        let all = ShortcutScope::AllUsers;
        let user = ShortcutScope::CurrentUser;
        assert_eq!(
            ShortcutLocation::StartMenu.folder_variable(all),
            "commonstartmenu"
        );
        assert_eq!(
            ShortcutLocation::Desktop.folder_variable(all),
            "commondesktop"
        );
        assert_eq!(
            ShortcutLocation::Startup.folder_variable(all),
            "commonstartup"
        );
        assert_eq!(
            ShortcutLocation::StartMenu.folder_variable(user),
            "startmenu"
        );
        assert_eq!(ShortcutLocation::Desktop.folder_variable(user), "desktop");
        assert_eq!(ShortcutLocation::Startup.folder_variable(user), "startup");
    }

    #[test]
    fn test_shortcut_subfolder_is_relative() {
        let shortcut = |subfolder: &str| {
            format!(
                "[[shortcuts]]\nname = \"App\"\ntarget = \"#{{app}}/app.exe\"\n\
                 location = \"start_menu\"\nsubfolder = '{subfolder}'\n"
            )
        };
        for ok in ["#{package.name}", "Divvun/Keyboards", r"Divvun\Keyboards"] {
            let config = with_privileges("admin", &shortcut(ok)).unwrap();
            assert_eq!(config.shortcuts[0].subfolder.as_deref(), Some(ok));
        }
        for bad in [
            "",
            "/Divvun",
            r"\Divvun",
            "C:/Divvun",
            "Divvun/../..",
            "./Divvun",
            "a//b",
        ] {
            let err = with_privileges("admin", &shortcut(bad))
                .err()
                .unwrap_or_else(|| panic!("{bad:?} accepted"));
            assert!(err.to_string().contains("shortcuts[0].subfolder"), "{err}");
        }
    }

    #[test]
    fn test_no_files_is_valid() {
        let config: Config = toml::from_str(minimal_toml()).unwrap();
        assert_eq!(config.files.len(), 0);
        assert!(validate_config(&config).is_ok());
    }
}
