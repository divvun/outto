use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{InstallerError, InstallerResult};

pub struct PathResolver {
    variables: HashMap<String, String>,
}

impl PathResolver {
    pub fn new(app_dir: &Path, package_name: &str, package_version: &str) -> Self {
        let mut variables = HashMap::new();

        variables.insert("app".into(), app_dir.to_string_lossy().into_owned());
        variables.insert("package.name".into(), package_name.into());
        variables.insert("package.version".into(), package_version.into());

        // Platform-specific paths
        #[cfg(windows)]
        Self::populate_windows_paths(&mut variables);

        #[cfg(not(windows))]
        Self::populate_fallback_paths(&mut variables);

        PathResolver { variables }
    }

    #[cfg(windows)]
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
                up.join(
                    "AppData\\Roaming\\Microsoft\\Windows\\Start Menu\\Programs\\Startup",
                )
                .to_string_lossy()
                .into_owned(),
            );
        }
    }

    #[cfg(not(windows))]
    fn populate_fallback_paths(variables: &mut HashMap<String, String>) {
        // Provide sensible defaults for non-Windows (testing)
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        variables.insert("pf".into(), format!("{home}/opt"));
        variables.insert("pf32".into(), format!("{home}/opt"));
        variables.insert("win".into(), "/usr".into());
        variables.insert("sys".into(), "/usr/lib".into());
        variables.insert("userappdata".into(), format!("{home}/.config"));
        variables.insert("localappdata".into(), format!("{home}/.local/share"));
        variables.insert("commonappdata".into(), "/etc".into());
        variables.insert("tmp".into(), "/tmp".into());
        variables.insert("desktop".into(), format!("{home}/Desktop"));
        variables.insert("startmenu".into(), format!("{home}/.local/share/applications"));
        variables.insert("startup".into(), format!("{home}/.config/autostart"));
    }

    pub fn resolve(&self, input: &str) -> InstallerResult<String> {
        let mut result = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '$' {
                // Collect variable name (alphanumeric, underscore, dot)
                let mut var_name = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_alphanumeric() || c == '_' || c == '.' {
                        var_name.push(c);
                        chars.next();
                    } else {
                        break;
                    }
                }
                if var_name.is_empty() {
                    result.push('$');
                } else if let Some(value) = self.variables.get(&var_name) {
                    result.push_str(value);
                } else {
                    return Err(InstallerError::Config(format!(
                        "unknown path variable: ${var_name}"
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
        // Normalize path separators to the platform native separator
        let normalized = if cfg!(windows) {
            resolved.replace('/', "\\")
        } else {
            resolved
        };
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
        let resolver = PathResolver::new(
            Path::new("C:\\Program Files\\MyApp"),
            "MyApp",
            "1.0.0",
        );
        assert_eq!(
            resolver.resolve("$app/bin").unwrap(),
            "C:\\Program Files\\MyApp/bin"
        );
    }

    #[test]
    fn test_resolve_package_variables() {
        let resolver = PathResolver::new(Path::new("/opt/myapp"), "MyApp", "2.1.0");
        assert_eq!(
            resolver.resolve("$package.name v$package.version").unwrap(),
            "MyApp v2.1.0"
        );
    }

    #[test]
    fn test_resolve_no_variables() {
        let resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        assert_eq!(resolver.resolve("plain text").unwrap(), "plain text");
    }

    #[test]
    fn test_resolve_unknown_variable() {
        let resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        assert!(resolver.resolve("$nonexistent").is_err());
    }

    #[test]
    fn test_resolve_dollar_at_end() {
        let resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        assert_eq!(resolver.resolve("price$").unwrap(), "price$");
    }

    #[test]
    fn test_custom_variable() {
        let mut resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        resolver.set_variable("custom", "/custom/path");
        assert_eq!(resolver.resolve("$custom/file").unwrap(), "/custom/path/file");
    }

    #[test]
    fn test_resolve_multiple_variables() {
        let resolver = PathResolver::new(Path::new("/opt/myapp"), "MyApp", "2.0.0");
        assert_eq!(
            resolver.resolve("$app/$package.name/$package.version").unwrap(),
            "/opt/myapp/MyApp/2.0.0"
        );
    }

    #[test]
    fn test_resolve_adjacent_variables() {
        let resolver = PathResolver::new(Path::new("/opt"), "App", "1.0.0");
        assert_eq!(
            resolver.resolve("$app$package.name").unwrap(),
            "/optApp"
        );
    }

    #[test]
    fn test_resolve_variable_alone() {
        let resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        assert_eq!(resolver.resolve("$app").unwrap(), "/opt");
    }

    #[test]
    fn test_resolve_with_backslashes() {
        let resolver = PathResolver::new(
            Path::new("C:\\Program Files\\MyApp"),
            "MyApp",
            "1.0.0",
        );
        assert_eq!(
            resolver.resolve("$app\\bin\\sub").unwrap(),
            "C:\\Program Files\\MyApp\\bin\\sub"
        );
    }

    #[test]
    fn test_resolve_empty_string() {
        let resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        assert_eq!(resolver.resolve("").unwrap(), "");
    }

    #[test]
    fn test_resolve_only_dollar_sign() {
        let resolver = PathResolver::new(Path::new("/opt"), "x", "1.0.0");
        assert_eq!(resolver.resolve("$").unwrap(), "$");
    }
}
