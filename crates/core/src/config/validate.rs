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
    validate_retire(config)?;
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
        crate::actions::run::validate_required(&r.phase, r.wait, r.required)
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

fn validate_retire(config: &Config) -> InstallerResult<()> {
    let invalid =
        |i: usize, what: String| Err(InstallerError::Validation(format!("retire[{i}]{what}")));
    for (i, entry) in config.retire.iter().enumerate() {
        if entry.name.trim().is_empty() {
            return invalid(i, ".name must not be empty".into());
        }
        if entry.steps().is_empty() {
            return invalid(
                i,
                format!(
                    " ({}) has nothing to remove: give it an inno_app_id, processes, \
                     services, shortcuts or registry_keys",
                    entry.name
                ),
            );
        }
        if let Some(guid) = &entry.inno_app_id {
            if !is_bare_guid(guid) {
                return invalid(
                    i,
                    format!(
                        ".inno_app_id '{guid}' must be a GUID without braces, \
                         e.g. 8A3F2C1D-0B4E-4F6A-9C7D-1E2F3A4B5C6D"
                    ),
                );
            }
            if config
                .package
                .legacy_inno_app_id
                .as_ref()
                .is_some_and(|own| own.eq_ignore_ascii_case(guid))
            {
                return invalid(
                    i,
                    format!(
                        ".inno_app_id '{guid}' is package.legacy_inno_app_id, the install \
                         this package takes the place of before it installs"
                    ),
                );
            }
        }
        for (j, image) in entry.processes.iter().enumerate() {
            if !is_image_name(image) {
                return invalid(
                    i,
                    format!(
                        ".processes[{j}] '{image}' must be the file name of a program, \
                         e.g. DivvunManager.exe"
                    ),
                );
            }
        }
        for (j, service) in entry.services.iter().enumerate() {
            if !is_service_name(service) {
                return invalid(
                    i,
                    format!(
                        ".services[{j}] '{service}' is not a service name: 1 to 256 \
                         characters, without / or \\ or spaces at either end"
                    ),
                );
            }
        }
        if !entry.services.is_empty() && config.package.privileges == Privileges::User {
            return invalid(
                i,
                ".services needs administrator privileges, and the package has \
                 privileges = \"user\""
                    .into(),
            );
        }
        for (j, shortcut) in entry.shortcuts.iter().enumerate() {
            let lower = shortcut.to_ascii_lowercase();
            if !(lower.ends_with(".lnk") || lower.ends_with(".url")) {
                return invalid(
                    i,
                    format!(".shortcuts[{j}] '{shortcut}' must be a .lnk or .url file"),
                );
            }
        }
        for (j, key) in entry.registry_keys.iter().enumerate() {
            let context = format!("retire[{i}].registry_keys[{j}]");
            check_registry_key(key, &context)?;
            let segments: Vec<&str> = split_registry_key(key)
                .map(|(_, path)| path.split('\\').collect())
                .unwrap_or_default();
            if segments.iter().any(|s| s.is_empty() || s.contains('*')) {
                return Err(InstallerError::Validation(format!(
                    "{context} '{key}' must name one key, without empty or `*` parts"
                )));
            }
            if segments.len() < 2 {
                return Err(InstallerError::Validation(format!(
                    "{context} '{key}' is at the top of its hive; name a key at least two \
                     levels down, e.g. HKLM\\SOFTWARE\\Example"
                )));
            }
        }
    }
    Ok(())
}

/// A program's file name, as a process list shows it: `name.exe`, with no
/// directory and no wildcard.
fn is_image_name(s: &str) -> bool {
    s.len() > 4
        && s.to_ascii_lowercase().ends_with(".exe")
        && s.trim() == s
        && !s.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|'])
}

/// A name the Service Control Manager accepts for a service: at most 256
/// characters, none of them `/` or `\`.
fn is_service_name(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= 256 && s.trim() == s && !s.contains(['/', '\\'])
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
    fn test_architecture_selectors() {
        use crate::config::types::Architecture;
        let parse = |arch: &str| {
            Config::from_toml(&format!(
                "{}architecture = \"{arch}\"\n\n[[files]]\nsource = \"a\"\ndest = \"#{{app}}\"\n\
                 arch = \"{arch}\"\n",
                minimal_toml()
            ))
        };
        for (name, arch) in [
            ("x86", Architecture::X86),
            ("x64", Architecture::X64),
            ("x64compatible", Architecture::X64Compatible),
            ("arm64", Architecture::Arm64),
            ("any", Architecture::Any),
        ] {
            let config = parse(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(config.package.architecture, arch, "{name}");
            assert_eq!(config.files[0].arch, Some(arch), "{name}");
        }
        for bad in ["x64_compatible", "X64Compatible", "amd64", "arm"] {
            assert!(parse(bad).is_err(), "{bad} accepted");
        }
        assert_eq!(
            Config::from_toml(minimal_toml())
                .unwrap()
                .package
                .architecture,
            Architecture::Any
        );
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

    const RETIRE: &str = r##"
[[retire]]
name = "Pahkat Service"
inno_app_id = "6B3A048B-BB81-4865-86CA-61A0DF038CFE"
services = ["pahkat-server"]
registry_keys = ['HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\Pahkat Service']

[[retire]]
name = "Divvun Manager"
inno_app_id = "4CF2F367-82A8-5E60-8334-34619CBA8347"
processes = ["DivvunManager.exe"]
shortcuts = [
    "#{commonstartmenu}/Divvun Manager.lnk",
    "#{commonstartup}/Divvun Manager.lnk",
]
"##;

    #[test]
    fn test_retire_section() {
        let config = with_privileges("admin", RETIRE).unwrap();
        assert_eq!(config.retire.len(), 2);
        let service = &config.retire[0];
        assert_eq!(service.name, "Pahkat Service");
        assert_eq!(
            service.inno_app_id.as_deref(),
            Some("6B3A048B-BB81-4865-86CA-61A0DF038CFE")
        );
        assert_eq!(service.services, ["pahkat-server"]);
        assert!(service.processes.is_empty() && service.shortcuts.is_empty());
        let manager = &config.retire[1];
        assert_eq!(manager.processes, ["DivvunManager.exe"]);
        assert_eq!(manager.shortcuts.len(), 2);
        assert!(manager.services.is_empty() && manager.registry_keys.is_empty());

        assert!(Config::from_toml(minimal_toml()).unwrap().retire.is_empty());
    }

    /// Entries go in the order listed; within one, programs end and
    /// services stop before files go, and the Add/Remove Programs entry
    /// goes last.
    #[test]
    fn test_retire_order() {
        use crate::config::types::RetireStep::{self, *};
        use crate::config::types::retire_steps;
        let config = with_privileges(
            "admin",
            &format!(
                "{RETIRE}registry_keys = ['HKLM\\SOFTWARE\\Divvun\\Manager']\n\
                 services = [\"divvun-a\", \"divvun-b\"]\n"
            ),
        )
        .unwrap();
        let steps: Vec<(&str, RetireStep)> = retire_steps(&config.retire)
            .into_iter()
            .map(|(entry, step)| (entry.name.as_str(), step))
            .collect();
        let service = "6B3A048B-BB81-4865-86CA-61A0DF038CFE";
        let manager = "4CF2F367-82A8-5E60-8334-34619CBA8347";
        assert_eq!(
            steps,
            [
                ("Pahkat Service", DeleteService("pahkat-server")),
                ("Pahkat Service", RemoveInnoFiles(service)),
                (
                    "Pahkat Service",
                    DeleteRegistryKey(
                        r"HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\Pahkat Service"
                    )
                ),
                ("Pahkat Service", RemoveInnoEntry(service)),
                ("Divvun Manager", EndProcess("DivvunManager.exe")),
                ("Divvun Manager", DeleteService("divvun-a")),
                ("Divvun Manager", DeleteService("divvun-b")),
                ("Divvun Manager", RemoveInnoFiles(manager)),
                (
                    "Divvun Manager",
                    RemoveShortcut("#{commonstartmenu}/Divvun Manager.lnk")
                ),
                (
                    "Divvun Manager",
                    RemoveShortcut("#{commonstartup}/Divvun Manager.lnk")
                ),
                (
                    "Divvun Manager",
                    DeleteRegistryKey(r"HKLM\SOFTWARE\Divvun\Manager")
                ),
                ("Divvun Manager", RemoveInnoEntry(manager)),
            ]
        );
    }

    #[test]
    fn test_retire_validation() {
        let entry = |body: &str| format!("[[retire]]\nname = \"Old\"\n{body}\n");
        let check = |privileges: &str, body: &str| with_privileges(privileges, &entry(body));
        for ok in [
            r#"inno_app_id = "4cf2f367-82a8-5e60-8334-34619cba8347""#,
            r#"processes = ["DivvunManager.exe", "helper.EXE"]"#,
            r#"services = ["pahkat-server", "Pahkat Service"]"#,
            r##"shortcuts = ["#{commonstartup}/Old.lnk", "#{commondesktop}/Old site.url"]"##,
            r"registry_keys = ['HKCU\Software\Old', 'HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Old']",
        ] {
            check("admin", ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for (privileges, bad, field) in [
            ("admin", "", "nothing to remove"),
            ("admin", "processes = []", "nothing to remove"),
            (
                "admin",
                r#"inno_app_id = "{4CF2F367-82A8-5E60-8334-34619CBA8347}""#,
                "inno_app_id",
            ),
            ("admin", r#"inno_app_id = "Divvun Manager""#, "inno_app_id"),
            ("admin", r#"processes = ["DivvunManager"]"#, "processes[0]"),
            (
                "admin",
                r#"processes = ["ok.exe", "C:\\x\\a.exe"]"#,
                "processes[1]",
            ),
            ("admin", r#"processes = ["*.exe"]"#, "processes[0]"),
            ("admin", r#"services = [""]"#, "services[0]"),
            ("admin", r#"services = ["a\\b"]"#, "services[0]"),
            ("admin", r#"services = [" pahkat-server"]"#, "services[0]"),
            ("user", r#"services = ["pahkat-server"]"#, "administrator"),
            (
                "admin",
                r##"shortcuts = ["#{commonstartup}/Old"]"##,
                "shortcuts[0]",
            ),
            (
                "admin",
                r"registry_keys = ['SOFTWARE\Old']",
                "registry_keys[0]",
            ),
            (
                "admin",
                r"registry_keys = ['HKLM\SOFTWARE']",
                "registry_keys[0]",
            ),
            (
                "admin",
                r"registry_keys = ['HKLM\SOFTWARE\*']",
                "registry_keys[0]",
            ),
            (
                "admin",
                r"registry_keys = ['HKLM\SOFTWARE\\Old']",
                "registry_keys[0]",
            ),
        ] {
            let err = check(privileges, bad)
                .err()
                .unwrap_or_else(|| panic!("{bad:?} accepted"));
            assert!(err.to_string().contains("retire[0]"), "{err}");
            assert!(err.to_string().contains(field), "{bad}: {err}");
        }
        let err =
            with_privileges("admin", "[[retire]]\nname = \"\"\nservices = [\"a\"]\n").unwrap_err();
        assert!(err.to_string().contains("retire[0].name"), "{err}");

        let guid = "8A3F2C1D-0B4E-4F6A-9C7D-1E2F3A4B5C6D";
        let own = Config::from_toml(&format!(
            "{}legacy_inno_app_id = \"{guid}\"\n{}",
            minimal_toml(),
            entry(&format!("inno_app_id = \"{}\"", guid.to_lowercase()))
        ))
        .unwrap_err();
        assert!(own.to_string().contains("legacy_inno_app_id"), "{own}");
    }

    #[test]
    fn test_no_files_is_valid() {
        let config: Config = toml::from_str(minimal_toml()).unwrap();
        assert_eq!(config.files.len(), 0);
        assert!(validate_config(&config).is_ok());
    }
}
