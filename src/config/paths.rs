use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{InstallerError, InstallerResult};

/// Resolves `#{...}` variable references in strings.
///
/// Variables are grouped by lifecycle:
///
/// | Variable | Category | Source |
/// |---|---|---|
/// | `#{pf}` | System | `%ProgramFiles%` |
/// | `#{pf32}` | System | `%ProgramFiles(x86)%` |
/// | `#{win}` | System | `%SystemRoot%` |
/// | `#{sys}` | System | `%SystemRoot%\System32` |
/// | `#{userappdata}` | System | `%APPDATA%` |
/// | `#{localappdata}` | System | `%LOCALAPPDATA%` |
/// | `#{commonappdata}` | System | `%ProgramData%` |
/// | `#{tmp}` | System | `%TEMP%` |
/// | `#{desktop}` | System | `%USERPROFILE%\Desktop` |
/// | `#{startmenu}` | System | Start Menu Programs folder |
/// | `#{startup}` | System | Startup folder |
/// | `#{package.name}` | Package | Config `[package] name` |
/// | `#{package.version}` | Package | Config `[package] version` |
/// | `#{app}` | Install | The chosen install directory |
///
/// Build with `new()` (system vars), then chain `with_package()` and
/// `with_install_dir()` as those values become known. Using a variable
/// that hasn't been added yet returns an error.
pub struct VariableResolver {
    variables: HashMap<String, String>,
}

impl VariableResolver {
    /// Create a resolver with system variables populated from the environment.
    pub fn new() -> Self {
        let mut variables = HashMap::new();

        Self::populate_windows_paths(&mut variables);

        VariableResolver { variables }
    }

    /// Add package metadata variables (`#{package.name}`, `#{package.version}`).
    pub fn with_package(mut self, name: &str, version: &str) -> Self {
        self.variables.insert("package.name".into(), name.into());
        self.variables
            .insert("package.version".into(), version.into());
        self
    }

    /// Add the install directory variable (`#{app}`).
    pub fn with_install_dir(mut self, dir: &Path) -> Self {
        self.variables
            .insert("app".into(), dir.to_string_lossy().into_owned());
        self
    }

    fn populate_windows_paths(variables: &mut HashMap<String, String>) {
        fn env(name: &str) -> Option<String> {
            std::env::var(name).ok()
        }

        // Program Files
        if let Some(pf) = env("ProgramFiles") {
            variables.insert("pf".into(), pf);
        }
        if let Some(pf86) = env("ProgramFiles(x86)") {
            variables.insert("pf32".into(), pf86);
        }

        // System directories
        if let Some(sys) = env("SystemRoot") {
            let sys_path = PathBuf::from(&sys);
            variables.insert("win".into(), sys.clone());
            variables.insert(
                "sys".into(),
                sys_path.join("System32").to_string_lossy().into_owned(),
            );
        }

        // User directories
        if let Some(appdata) = env("APPDATA") {
            variables.insert("userappdata".into(), appdata);
        }
        if let Some(localappdata) = env("LOCALAPPDATA") {
            variables.insert("localappdata".into(), localappdata);
        }
        if let Some(programdata) = env("ProgramData") {
            variables.insert("commonappdata".into(), programdata);
        }
        if let Some(temp) = env("TEMP") {
            variables.insert("tmp".into(), temp);
        }

        // Desktop and Start Menu - get from shell folders
        if let Some(userprofile) = env("USERPROFILE") {
            let up = PathBuf::from(&userprofile);
            variables.insert(
                "desktop".into(),
                up.join("Desktop").to_string_lossy().into_owned(),
            );
            variables.insert(
                "startmenu".into(),
                up.join("AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs")
                    .to_string_lossy()
                    .into_owned(),
            );
            variables.insert(
                "startup".into(),
                up.join("AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    pub fn resolve(&self, input: &str) -> InstallerResult<String> {
        let mut result = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '#' && chars.peek() == Some(&'{') {
                chars.next(); // consume '{'
                let mut var_name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => var_name.push(c),
                        None => {
                            return Err(InstallerError::Config(format!(
                                "unclosed variable: #{{{}",
                                var_name
                            )));
                        }
                    }
                }
                if let Some(value) = self.variables.get(&var_name) {
                    result.push_str(value);
                } else {
                    return Err(InstallerError::Config(format!(
                        "unknown path variable: #{{{}}}",
                        var_name
                    )));
                }
            } else {
                result.push(ch);
            }
        }

        Ok(result)
    }

    pub fn resolve_path(&self, input: &str) -> InstallerResult<PathBuf> {
        let resolved = self.resolve(input)?;
        // Normalize path separators to Windows native separator
        let normalized = resolved.replace('/', "\\");
        Ok(PathBuf::from(normalized))
    }

    pub fn set_variable(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.variables.insert(name.into(), value.into());
    }

    pub fn get_variable(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(|s| s.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_resolve_simple_variable() {
        let resolver = VariableResolver::new()
            .with_package("MyApp", "1.0.0")
            .with_install_dir(Path::new("C:\\Program Files\\MyApp"));
        assert_eq!(
            resolver.resolve("#{app}/bin").unwrap(),
            "C:\\Program Files\\MyApp/bin"
        );
    }

    #[test]
    fn test_resolve_package_variables() {
        let resolver = VariableResolver::new()
            .with_package("MyApp", "2.1.0")
            .with_install_dir(Path::new("/opt/myapp"));
        assert_eq!(
            resolver
                .resolve("#{package.name} v#{package.version}")
                .unwrap(),
            "MyApp v2.1.0"
        );
    }

    #[test]
    fn test_resolve_no_variables() {
        let resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert_eq!(resolver.resolve("plain text").unwrap(), "plain text");
    }

    #[test]
    fn test_resolve_unknown_variable() {
        let resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert!(resolver.resolve("#{nonexistent}").is_err());
    }

    #[test]
    fn test_resolve_unclosed_variable() {
        let resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert!(resolver.resolve("#{app").is_err());
    }

    #[test]
    fn test_resolve_literal_hash() {
        let resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert_eq!(resolver.resolve("color #red").unwrap(), "color #red");
    }

    #[test]
    fn test_custom_variable() {
        let mut resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        resolver.set_variable("custom", "/custom/path");
        assert_eq!(
            resolver.resolve("#{custom}/file").unwrap(),
            "/custom/path/file"
        );
    }

    #[test]
    fn test_resolve_multiple_variables() {
        let resolver = VariableResolver::new()
            .with_package("MyApp", "2.0.0")
            .with_install_dir(Path::new("/opt/myapp"));
        assert_eq!(
            resolver
                .resolve("#{app}/#{package.name}/#{package.version}")
                .unwrap(),
            "/opt/myapp/MyApp/2.0.0"
        );
    }

    #[test]
    fn test_resolve_adjacent_variables() {
        let resolver = VariableResolver::new()
            .with_package("App", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert_eq!(
            resolver.resolve("#{app}#{package.name}").unwrap(),
            "/optApp"
        );
    }

    #[test]
    fn test_resolve_variable_alone() {
        let resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert_eq!(resolver.resolve("#{app}").unwrap(), "/opt");
    }

    #[test]
    fn test_resolve_with_backslashes() {
        let resolver = VariableResolver::new()
            .with_package("MyApp", "1.0.0")
            .with_install_dir(Path::new("C:\\Program Files\\MyApp"));
        assert_eq!(
            resolver.resolve("#{app}\\bin\\sub").unwrap(),
            "C:\\Program Files\\MyApp\\bin\\sub"
        );
    }

    #[test]
    fn test_resolve_empty_string() {
        let resolver = VariableResolver::new()
            .with_package("x", "1.0.0")
            .with_install_dir(Path::new("/opt"));
        assert_eq!(resolver.resolve("").unwrap(), "");
    }
}
