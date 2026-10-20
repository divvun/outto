use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::error::{InstallerError, InstallerResult};

/// Resolves `#{...}` variable references in strings.
///
/// Core provides only platform-neutral variables — package metadata and the
/// chosen install directory:
///
/// | Variable | Source |
/// |---|---|
/// | `#{package.name}` | Config `[package] name` |
/// | `#{package.version}` | Config `[package] version` |
/// | `#{app}` | The chosen install directory |
///
/// `[[run]]` arguments may also use `#{log_arg:NAME}`, which is expanded when
/// the command runs; see [`parse_log_arg`].
///
/// Platform crates add their own variable tables via `with_platform_env()`.
/// The Windows backend populates `#{pf}`, `#{windir}`, `#{sys}`, `#{userappdata}`,
/// etc. See `outto-windows::paths::populate_windows_env`.
///
/// Using a variable that hasn't been added returns an error.
pub struct VariableResolver {
    variables: HashMap<String, String>,
    /// When true, `resolve_path` converts `/` to `\` (Windows-native).
    /// When false, paths are left as-is (POSIX).
    windows_path_separators: bool,
}

impl Default for VariableResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl VariableResolver {
    /// Create an empty resolver with no variables. Use builder methods to populate.
    pub fn new() -> Self {
        VariableResolver {
            variables: HashMap::new(),
            windows_path_separators: cfg!(windows),
        }
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

    /// Force Windows-style path separators in `resolve_path`. Defaults to
    /// the host's convention, so platform backends usually don't need this.
    pub fn with_windows_paths(mut self, enabled: bool) -> Self {
        self.windows_path_separators = enabled;
        self
    }

    pub fn resolve(&self, input: &str) -> InstallerResult<String> {
        self.resolve_inner(input, false)
    }

    /// [`resolve`](Self::resolve) for `[[run]]` arguments, which may also hold
    /// `#{log_arg:NAME}`. That one is left as it is, to be expanded when the
    /// command runs (see [`parse_log_arg`]).
    pub fn resolve_arguments(&self, input: &str) -> InstallerResult<String> {
        self.resolve_inner(input, true)
    }

    fn resolve_inner(&self, input: &str, keep_log_args: bool) -> InstallerResult<String> {
        let mut result = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '#' && chars.peek() == Some(&'{') {
                chars.next();
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
                if let Some(name) = var_name.strip_prefix(LOG_ARG) {
                    if !keep_log_args {
                        return Err(InstallerError::Config(format!(
                            "#{{{var_name}}} can only be used in [[run]] arguments"
                        )));
                    }
                    if !is_log_arg_name(name) {
                        return Err(InstallerError::Config(format!(
                            "#{{{var_name}}}: the name after `{LOG_ARG}` has to be \
                             letters, digits, `-`, `_` and `.`"
                        )));
                    }
                    result.push_str(&format!("#{{{var_name}}}"));
                } else if let Some(value) = self.variables.get(&var_name) {
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
        let normalized = if self.windows_path_separators {
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

const LOG_ARG: &str = "log_arg:";

fn is_log_arg_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `NAME` when `arg` is exactly `#{log_arg:NAME}`.
///
/// In a `[[run]]` command's arguments, `#{log_arg:NAME}` passes the running
/// installer's or uninstaller's `/LOG` on to the program it runs, another
/// installer: it becomes the single argument `/LOG=<log>` with the log file
/// next to the current one, `NAME` added to its name before the extension
/// (`install.log` gives `install.NAME.log`). Without a log it becomes no
/// argument at all. It is expanded when the command runs, so an uninstall
/// command passes on the uninstaller's log, not the installer's.
pub fn parse_log_arg(arg: &str) -> Option<&str> {
    arg.strip_prefix("#{")?
        .strip_suffix('}')?
        .strip_prefix(LOG_ARG)
        .filter(|name| is_log_arg_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
        let resolver = VariableResolver::new();
        assert_eq!(resolver.resolve("plain text").unwrap(), "plain text");
    }

    #[test]
    fn test_resolve_unknown_variable() {
        let resolver = VariableResolver::new();
        assert!(resolver.resolve("#{nonexistent}").is_err());
    }

    #[test]
    fn test_resolve_unclosed_variable() {
        let resolver = VariableResolver::new();
        assert!(resolver.resolve("#{app").is_err());
    }

    #[test]
    fn test_resolve_literal_hash() {
        let resolver = VariableResolver::new();
        assert_eq!(resolver.resolve("color #red").unwrap(), "color #red");
    }

    #[test]
    fn test_custom_variable() {
        let mut resolver = VariableResolver::new();
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
    fn test_resolve_empty_string() {
        let resolver = VariableResolver::new();
        assert_eq!(resolver.resolve("").unwrap(), "");
    }

    #[test]
    fn log_arg_is_kept_for_run_arguments_only() {
        let resolver = VariableResolver::new().with_install_dir(Path::new("/opt/myapp"));
        assert_eq!(
            resolver
                .resolve_arguments("/VERYSILENT #{log_arg:text-service} \"#{app}\"")
                .unwrap(),
            "/VERYSILENT #{log_arg:text-service} \"/opt/myapp\""
        );
        let err = resolver.resolve("#{app}/#{log_arg:x}").unwrap_err();
        assert!(err.to_string().contains("[[run]] arguments"), "{err}");
        for bad in [
            "#{log_arg:}",
            "#{log_arg:a b}",
            "#{log_arg:a/b}",
            "#{log_arg:\"x\"}",
        ] {
            assert!(resolver.resolve_arguments(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_log_args() {
        assert_eq!(
            parse_log_arg("#{log_arg:text-service}"),
            Some("text-service")
        );
        assert_eq!(parse_log_arg("#{log_arg:a_b.c}"), Some("a_b.c"));
        assert_eq!(parse_log_arg("#{log_arg:}"), None);
        assert_eq!(parse_log_arg("#{log_arg:a\\b}"), None);
        assert_eq!(parse_log_arg("x#{log_arg:a}"), None);
        assert_eq!(parse_log_arg("#{app}"), None);
        assert_eq!(parse_log_arg("/LOG"), None);
    }
}
