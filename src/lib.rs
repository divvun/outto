pub mod actions;
pub mod config;
pub mod detect;
pub mod elevation;
pub mod error;
pub mod manifest;
pub mod uninstall;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use config::Config;
pub use error::{ErrorAction, InstallerError, InstallerResult};
pub use manifest::InstallManifest;

// Re-export key types
pub use config::types::{
    Architecture, ComponentEntry, OverwritePolicy, Privileges, UpgradePolicy,
};
pub use detect::ExistingInstall;

/// Log severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

/// Prompt types that the library may ask the host to resolve.
#[derive(Debug, Clone)]
pub enum Prompt {
    OverwriteFile { path: PathBuf },
    ExistingInstallDetected { existing: ExistingInstall },
}

/// Host response to a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptResponse {
    Yes,
    No,
    YesToAll,
    Cancel,
}

/// Trait for host programs to implement. Provides UI callbacks.
pub trait InstallerCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64);
    fn on_prompt(&self, prompt: Prompt) -> PromptResponse;
    fn on_log(&self, level: LogLevel, message: &str);
    fn on_error(&self, error: &InstallerError) -> ErrorAction;
}

/// No-op callbacks for silent installs.
pub struct NoOpCallbacks;

impl InstallerCallbacks for NoOpCallbacks {
    fn on_progress(&self, _phase: &str, _current: u64, _total: u64) {}
    fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
        PromptResponse::Yes
    }
    fn on_log(&self, _level: LogLevel, _message: &str) {}
    fn on_error(&self, _error: &InstallerError) -> ErrorAction {
        ErrorAction::Abort
    }
}

/// Options for installation.
pub struct InstallOptions {
    /// The directory containing the source files to install.
    pub source_dir: PathBuf,
    /// Override the default installation directory.
    pub install_dir: Option<PathBuf>,
    /// Selected component names. None = all components.
    pub selected_components: Option<HashSet<String>>,
    /// Path to the uninstaller executable. If set, it will be copied to
    /// `{install_dir}/.outto/uninstall.exe` and registered in Add/Remove Programs.
    pub uninstall_exe: Option<PathBuf>,
}

/// Main entry point: install from a parsed config.
pub fn install(
    config: &Config,
    options: &InstallOptions,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Starting installation: {} v{}",
            config.package.name, config.package.version
        ),
    );

    // Check architecture compatibility
    if !detect::arch_matches(&config.package.architecture) {
        return Err(InstallerError::Validation(format!(
            "architecture mismatch: package requires {:?} but system is {}",
            config.package.architecture,
            elevation::get_system_architecture()
        )));
    }

    // Check elevation
    if elevation::needs_elevation(&config.package.privileges) {
        return Err(InstallerError::ElevationRequired(
            "this installer requires administrator privileges".into(),
        ));
    }

    // Determine install directory (normalize separators)
    let install_dir = if let Some(ref dir) = options.install_dir {
        PathBuf::from(dir.to_string_lossy().replace('/', std::path::MAIN_SEPARATOR_STR))
    } else if let Some(ref default_dir) = config.package.default_dir {
        // Create a temporary resolver just for the default_dir
        let temp_resolver = config::PathResolver::new(
            Path::new(""),
            &config.package.name,
            &config.package.version,
        );
        temp_resolver.resolve_path(default_dir)?
    } else {
        return Err(InstallerError::Config(
            "no install directory specified (set install_dir in options or default_dir in config)"
                .into(),
        ));
    };

    // Create path resolver with actual install dir
    let resolver = config::PathResolver::new(
        &install_dir,
        &config.package.name,
        &config.package.version,
    );

    // Check for existing installation
    if let Some(existing) = detect::detect_existing_install(&config.package.id)? {
        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Existing installation found: {} v{}",
                existing.display_name.as_deref().unwrap_or("unknown"),
                existing.version.as_deref().unwrap_or("unknown")
            ),
        );

        match config.upgrade.policy {
            UpgradePolicy::Fail => {
                return Err(InstallerError::UpgradeConflict(format!(
                    "{} is already installed",
                    config.package.name
                )));
            }
            UpgradePolicy::SideBySide => {
                // Allow side-by-side (different dir)
            }
            UpgradePolicy::Overwrite => {
                // Continue and overwrite
            }
        }
    }

    // Check prerequisites
    actions::prerequisites::check_prerequisites(
        &config.prerequisites,
        callbacks,
    )?;

    // Create install directory
    std::fs::create_dir_all(&install_dir).map_err(|e| InstallerError::DirOp {
        path: install_dir.clone(),
        source: e,
    })?;

    // Initialize manifest and record directory creation
    let mut install_manifest = InstallManifest::new(
        &config.package.id,
        &config.package.name,
        &config.package.version,
        &install_dir,
    );
    install_manifest.record(manifest::ActionRecord::DirectoryCreated {
        path: install_dir.clone(),
    });

    // Execute all install actions
    let result = actions::execute_install(
        config,
        &options.source_dir,
        &options.selected_components,
        &resolver,
        &mut install_manifest,
        callbacks,
    );

    match result {
        Ok(()) => {
            // Save manifest for uninstallation
            install_manifest.save()?;

            // Copy uninstaller exe if provided
            let uninstall_string = if let Some(ref uninstall_exe_src) = options.uninstall_exe {
                let outto_dir = install_dir.join(".outto");
                let uninstall_dest = outto_dir.join("uninstall.exe");
                std::fs::copy(uninstall_exe_src, &uninstall_dest).map_err(|e| {
                    InstallerError::FileOp {
                        path: uninstall_dest.clone(),
                        source: e,
                    }
                })?;
                callbacks.on_log(
                    LogLevel::Info,
                    &format!("Copied uninstaller to {}", uninstall_dest.display()),
                );
                Some(format!(
                    "\"{}\" --dir \"{}\"",
                    uninstall_dest.display(),
                    install_dir.display()
                ))
            } else {
                None
            };

            // Write Add/Remove Programs registry entry
            let display_icon = config
                .uninstall
                .display_icon
                .as_deref()
                .map(|i| resolver.resolve(i))
                .transpose()?;

            detect::write_uninstall_registry(&detect::UninstallRegistryInfo {
                package_id: &config.package.id,
                display_name: &config.package.name,
                version: &config.package.version,
                publisher: config.package.publisher.as_deref(),
                install_dir: &install_dir,
                display_icon: display_icon.as_deref(),
                url: config.package.url.as_deref(),
                support_url: config.package.support_url.as_deref(),
                uninstall_string: uninstall_string.as_deref(),
            })?;

            callbacks.on_log(LogLevel::Info, "Installation complete");
            callbacks.on_progress("complete", 1, 1);
            Ok(())
        }
        Err(e) => {
            // Rollback on failure
            callbacks.on_log(
                LogLevel::Error,
                &format!("Installation failed: {e}. Rolling back..."),
            );

            let rollback_result =
                manifest::rollback::rollback_actions(&install_manifest.actions, callbacks, true);

            match rollback_result {
                Ok(()) => {
                    callbacks.on_log(LogLevel::Info, "Rollback completed successfully");
                    Err(e)
                }
                Err(rollback_err) => Err(InstallerError::RollbackFailed {
                    original_error: e.to_string(),
                    rollback_error: rollback_err.to_string(),
                }),
            }
        }
    }
}

/// Uninstall from a manifest stored at the given install directory.
pub fn uninstall_from_dir(
    install_dir: &Path,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    uninstall::uninstall(install_dir, callbacks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct TestCallbacks {
        logs: Arc<Mutex<Vec<(LogLevel, String)>>>,
    }

    impl InstallerCallbacks for TestCallbacks {
        fn on_progress(&self, _phase: &str, _current: u64, _total: u64) {}
        fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
            PromptResponse::Yes
        }
        fn on_log(&self, level: LogLevel, message: &str) {
            self.logs
                .lock()
                .unwrap()
                .push((level, message.to_string()));
        }
        fn on_error(&self, _error: &InstallerError) -> ErrorAction {
            ErrorAction::Abort
        }
    }

    #[test]
    fn test_install_basic_files() {
        let test_dir = std::env::temp_dir().join("outto_test_install");
        let source_dir = test_dir.join("source");
        let install_dir = test_dir.join("installed");

        // Clean up from previous runs
        let _ = fs::remove_dir_all(&test_dir);

        // Create source files
        fs::create_dir_all(source_dir.join("build")).unwrap();
        fs::write(source_dir.join("build/app.exe"), "fake exe").unwrap();
        fs::write(source_dir.join("build/readme.txt"), "readme").unwrap();

        let toml = r#"
[package]
id = "com.test.basic"
name = "BasicTest"
version = "1.0.0"

[[files]]
source = "build/*"
dest = "$app"
overwrite = "always"
"#;
        let config = Config::from_toml(toml).unwrap();
        let callbacks = TestCallbacks::default();

        let options = InstallOptions {
            source_dir,
            install_dir: Some(install_dir.clone()),
            selected_components: None,
            uninstall_exe: None,
        };

        let result = install(&config, &options, &callbacks);
        assert!(result.is_ok(), "Install failed: {result:?}");

        // Verify files were copied
        assert!(install_dir.join("app.exe").exists());
        assert!(install_dir.join("readme.txt").exists());

        // Verify manifest was created
        assert!(InstallManifest::manifest_path(&install_dir).exists());

        // Test uninstall
        let result = uninstall_from_dir(&install_dir, &callbacks);
        assert!(result.is_ok(), "Uninstall failed: {result:?}");

        // Verify files were removed
        assert!(!install_dir.join("app.exe").exists());
        assert!(!install_dir.join("readme.txt").exists());

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_install_with_components() {
        let test_dir = std::env::temp_dir().join("outto_test_components");
        let source_dir = test_dir.join("source");
        let install_dir = test_dir.join("installed");

        let _ = fs::remove_dir_all(&test_dir);

        fs::create_dir_all(source_dir.join("core")).unwrap();
        fs::create_dir_all(source_dir.join("extras")).unwrap();
        fs::write(source_dir.join("core/app.exe"), "core").unwrap();
        fs::write(source_dir.join("extras/plugin.dll"), "extras").unwrap();

        let toml = r#"
[package]
id = "com.test.comp"
name = "CompTest"
version = "1.0.0"

[[components]]
name = "core"
required = true

[[components]]
name = "extras"

[[files]]
source = "core/*"
dest = "$app"
component = "core"

[[files]]
source = "extras/*"
dest = "$app/extras"
component = "extras"
"#;
        let config = Config::from_toml(toml).unwrap();
        let callbacks = TestCallbacks::default();

        // Only select "core" component
        let mut selected = HashSet::new();
        selected.insert("core".to_string());

        let options = InstallOptions {
            source_dir,
            install_dir: Some(install_dir.clone()),
            selected_components: Some(selected),
            uninstall_exe: None,
        };

        let result = install(&config, &options, &callbacks);
        assert!(result.is_ok());

        assert!(install_dir.join("app.exe").exists());
        assert!(!install_dir.join("extras/plugin.dll").exists()); // extras not selected

        let _ = fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn test_noop_callbacks() {
        let callbacks = NoOpCallbacks;
        callbacks.on_progress("test", 0, 1);
        callbacks.on_log(LogLevel::Info, "test");
        assert_eq!(
            callbacks.on_prompt(Prompt::OverwriteFile {
                path: PathBuf::from("test")
            }),
            PromptResponse::Yes
        );
        assert_eq!(
            callbacks.on_error(&InstallerError::Other("test".into())),
            ErrorAction::Abort
        );
    }

    #[test]
    fn test_config_roundtrip() {
        let toml = r#"
[package]
id = "com.test.roundtrip"
name = "RoundTrip"
version = "1.0.0"
publisher = "Test Corp"
architecture = "x64"
privileges = "admin"
default_dir = "$pf/$package.name"

[logging]
enabled = true

[[files]]
source = "build/*"
dest = "$app"

[[registry]]
root = "hkcu"
key = "Software\\Test"
values = [{ name = "Key", type = "string", data = "Value" }]

[[shortcuts]]
name = "TestApp"
target = "$app/test.exe"
location = "desktop"
"#;
        let config = Config::from_toml(toml).unwrap();
        assert_eq!(config.package.id, "com.test.roundtrip");
        assert_eq!(config.package.architecture, Architecture::X64);
        assert_eq!(config.package.privileges, Privileges::Admin);
        assert_eq!(config.files.len(), 1);
        assert_eq!(config.registry.len(), 1);
        assert_eq!(config.shortcuts.len(), 1);
    }
}
