use crate::config::Config;
use crate::error::{InstallerError, InstallerResult};

pub fn validate_config(config: &Config) -> InstallerResult<()> {
    validate_package(config)?;
    validate_components(config)?;
    validate_files(config)?;
    validate_registry(config)?;
    validate_shortcuts(config)?;
    validate_services(config)?;
    validate_associations(config)?;
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
    // Validate version is valid semver
    if semver::Version::parse(&config.package.version).is_err() {
        return Err(InstallerError::Validation(format!(
            "package.version '{}' is not valid semver",
            config.package.version
        )));
    }
    Ok(())
}

fn validate_components(config: &Config) -> InstallerResult<()> {
    let component_names: Vec<&str> = config.components.iter().map(|c| c.name.as_str()).collect();

    // Check for duplicate component names
    let mut seen = std::collections::HashSet::new();
    for name in &component_names {
        if !seen.insert(name) {
            return Err(InstallerError::Validation(format!(
                "duplicate component name: '{name}'"
            )));
        }
    }

    // Validate component references in entries
    let check_component = |comp: &Option<String>, context: &str| -> InstallerResult<()> {
        if let Some(ref name) = comp {
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
    fn test_unknown_component_reference() {
        let config: Config = toml::from_str(
            r##"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[components]]
name = "core"
required = true

[[files]]
source = "build/*"
dest = "#{app}"
component = "nonexistent"
"##,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_association_missing_dot() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[associations]]
extension = "myf"
prog_id = "MyApp.Doc"
command = "myapp \"%1\""
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_package_name() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = ""
version = "1.0.0"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_package_version() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = ""
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_version_missing_patch() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0"
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
    fn test_version_with_build_metadata() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0+build.123"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_duplicate_component_names() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[components]]
name = "core"

[[components]]
name = "core"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_component_ref_with_no_components_defined() {
        // When no components are defined, references should be accepted (components are optional)
        let config: Config = toml::from_str(
            r##"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[files]]
source = "build/*"
dest = "#{app}"
component = "anything"
"##,
        )
        .unwrap();
        // This should pass because component_names is empty, so the check is skipped
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn test_empty_file_source() {
        let config: Config = toml::from_str(
            r##"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[files]]
source = ""
dest = "#{app}"
"##,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_file_dest() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[files]]
source = "build/*"
dest = ""
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_registry_key() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[registry]]
root = "hkcu"
key = ""
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_shortcut_name() {
        let config: Config = toml::from_str(
            r##"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[shortcuts]]
name = ""
target = "#{app}/test.exe"
location = "desktop"
"##,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_shortcut_target() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[shortcuts]]
name = "Test"
target = ""
location = "desktop"
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_service_name() {
        let config: Config = toml::from_str(
            r##"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[services]]
name = ""
executable = "#{app}/svc.exe"
"##,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_empty_service_executable() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[services]]
name = "myservice"
executable = ""
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_association_empty_prog_id() {
        let config: Config = toml::from_str(
            r#"
[package]
id = "com.test"
name = "TestApp"
version = "1.0.0"

[[associations]]
extension = ".myf"
prog_id = ""
command = "myapp \"%1\""
"#,
        )
        .unwrap();
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn test_no_files_is_valid() {
        let config: Config = toml::from_str(minimal_toml()).unwrap();
        assert_eq!(config.files.len(), 0);
        assert!(validate_config(&config).is_ok());
    }
}
