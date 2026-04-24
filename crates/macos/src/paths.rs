//! macOS-specific variables for `outto_core::VariableResolver`.
//!
//! Call [`with_macos_env`] after `VariableResolver::new()` to get the
//! `#{applications}`, `#{library}`, `#{launch_agents_user}`, etc. variable
//! table that macOS manifests use.

use std::path::PathBuf;

use outto_core::config::VariableResolver;

/// Populate macOS shell-folder variables onto the given resolver.
pub fn with_macos_env(mut resolver: VariableResolver) -> VariableResolver {
    let home = home_dir();

    resolver.set_variable("applications", "/Applications");
    resolver.set_variable("library", "/Library");
    resolver.set_variable("local", "/usr/local");
    resolver.set_variable("local_bin", "/usr/local/bin");
    resolver.set_variable("launch_daemons", "/Library/LaunchDaemons");
    resolver.set_variable("launch_agents_system", "/Library/LaunchAgents");
    resolver.set_variable("fonts_system", "/Library/Fonts");
    resolver.set_variable("prefs_system", "/Library/Preferences");
    resolver.set_variable("app_support_system", "/Library/Application Support");

    resolver.set_variable(
        "tmp",
        std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into()),
    );

    if let Some(home) = home {
        let home_str = home.to_string_lossy().into_owned();
        resolver.set_variable("home", home_str.clone());
        resolver.set_variable("user_applications", format!("{home_str}/Applications"));
        resolver.set_variable("user_library", format!("{home_str}/Library"));
        resolver.set_variable(
            "launch_agents_user",
            format!("{home_str}/Library/LaunchAgents"),
        );
        resolver.set_variable("fonts_user", format!("{home_str}/Library/Fonts"));
        resolver.set_variable("prefs_user", format!("{home_str}/Library/Preferences"));
        resolver.set_variable(
            "app_support_user",
            format!("{home_str}/Library/Application Support"),
        );
    }

    resolver
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Construct a fully-configured resolver: builder pattern on top of
/// [`VariableResolver::new`], with macOS env + forward-slash path separators.
pub fn make_resolver(
    package_name: &str,
    package_version: &str,
    install_dir: Option<&std::path::Path>,
) -> VariableResolver {
    let mut resolver = VariableResolver::new()
        .with_windows_paths(false)
        .with_package(package_name, package_version);
    resolver = with_macos_env(resolver);
    if let Some(dir) = install_dir {
        resolver = resolver.with_install_dir(dir);
    }
    resolver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_macos_variables_present() {
        let r = with_macos_env(VariableResolver::new());
        assert_eq!(r.get_variable("applications"), Some("/Applications"));
        assert_eq!(r.get_variable("library"), Some("/Library"));
        assert_eq!(r.get_variable("local"), Some("/usr/local"));
        assert_eq!(r.get_variable("local_bin"), Some("/usr/local/bin"));
        assert_eq!(
            r.get_variable("launch_daemons"),
            Some("/Library/LaunchDaemons")
        );
        assert_eq!(r.get_variable("fonts_system"), Some("/Library/Fonts"));
    }

    #[test]
    fn test_home_based_variables() {
        // Only check that HOME-based expansion runs; exact path depends on the host.
        let r = with_macos_env(VariableResolver::new());
        if std::env::var_os("HOME").is_some() {
            assert!(r.get_variable("home").is_some());
            assert!(r.get_variable("user_library").is_some());
            assert!(r.get_variable("launch_agents_user").is_some());
            assert!(r.get_variable("fonts_user").is_some());
        }
    }

    #[test]
    fn test_resolve_macos_path_pattern() {
        let r = make_resolver("MyApp", "1.0.0", None);
        let result = r.resolve("#{applications}/MyApp.app").unwrap();
        assert_eq!(result, "/Applications/MyApp.app");
    }

    #[test]
    fn test_macos_paths_keep_forward_slashes() {
        let r = make_resolver("MyApp", "1.0.0", None);
        let path = r.resolve_path("#{library}/MyApp/config.json").unwrap();
        // On macOS we want /Library/MyApp/config.json, not backslash-separated
        assert_eq!(
            path.to_string_lossy().as_ref(),
            "/Library/MyApp/config.json"
        );
    }
}
