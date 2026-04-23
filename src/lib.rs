pub mod actions;
pub mod config;
pub mod detect;
pub mod elevation;
pub mod error;
pub mod manifest;
pub mod pe;
pub mod uninstall;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub use config::Config;
pub use error::{ErrorAction, InstallerError, InstallerResult};
pub use manifest::InstallManifest;

// Re-export key types
pub use config::types::{Architecture, ComponentEntry, OverwritePolicy, Privileges, UpgradePolicy};
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
        PathBuf::from(
            dir.to_string_lossy()
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        )
    } else if let Some(ref default_dir) = config.package.default_dir {
        let config_resolver = config::VariableResolver::new()
            .with_package(&config.package.name, &config.package.version);
        config_resolver.resolve_path(default_dir)?
    } else {
        return Err(InstallerError::Config(
            "no install directory specified (set install_dir in options or default_dir in config)"
                .into(),
        ));
    };

    // Create path resolver with actual install dir
    let resolver = config::VariableResolver::new()
        .with_package(&config.package.name, &config.package.version)
        .with_install_dir(&install_dir);

    // Check for existing installation
    let mut old_manifest: Option<InstallManifest> = None;
    let mut old_install_dir: Option<PathBuf> = None;
    if let Some(existing) = detect::detect_existing_install(&config.package.id)? {
        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Existing installation found: {} v{} at {}",
                existing.display_name.as_deref().unwrap_or("unknown"),
                existing.version.as_deref().unwrap_or("unknown"),
                existing.install_dir.display()
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
                // Load manifest from the OLD install location (may differ from new)
                old_manifest =
                    InstallManifest::load(&existing.install_dir, &config.package.id).ok();
                if existing.install_dir != install_dir {
                    old_install_dir = Some(existing.install_dir);
                }
            }
        }
    }

    // Check prerequisites
    actions::prerequisites::check_prerequisites(&config.prerequisites, callbacks)?;

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
        config.package.depends_on.clone(),
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

            // Clean up files from previous version that are no longer installed
            if let Some(old) = old_manifest {
                let new_files: std::collections::HashSet<PathBuf> = install_manifest
                    .actions
                    .iter()
                    .filter_map(|a| match a {
                        manifest::ActionRecord::FileCopied { dest, .. } => Some(dest.clone()),
                        _ => None,
                    })
                    .collect();

                for action in &old.actions {
                    if let manifest::ActionRecord::FileCopied { dest, .. } = action {
                        if !new_files.contains(dest) && dest.exists() {
                            callbacks.on_log(
                                LogLevel::Info,
                                &format!("Upgrade: removing orphaned file {}", dest.display()),
                            );
                            let _ = std::fs::remove_file(dest);
                        }
                    }
                }

                // If install dir changed, clean up old .outto/{package_id}/ directory
                if let Some(ref old_dir) = old_install_dir {
                    let old_pkg_dir = InstallManifest::package_dir(old_dir, &config.package.id);
                    if old_pkg_dir.exists() {
                        callbacks.on_log(
                            LogLevel::Info,
                            &format!(
                                "Upgrade: removing old package dir {}",
                                old_pkg_dir.display()
                            ),
                        );
                        let _ = std::fs::remove_dir_all(&old_pkg_dir);
                    }
                    // Remove old install dir if empty
                    if old_dir.exists() {
                        let _ = std::fs::remove_dir(old_dir);
                    }
                }
            }

            // Copy uninstaller exe if provided
            let uninstall_string = if let Some(ref uninstall_exe_src) = options.uninstall_exe {
                let pkg_dir = InstallManifest::package_dir(&install_dir, &config.package.id);
                let uninstall_dest = pkg_dir.join("uninstall.exe");
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
                depends_on: &config.package.depends_on,
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

/// Uninstall a package and cascade-uninstall all dependents.
pub fn uninstall_package(
    install_dir: &Path,
    package_id: &str,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    uninstall::uninstall(install_dir, package_id, callbacks)
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
            self.logs.lock().unwrap().push((level, message.to_string()));
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

        let toml = r##"
[package]
id = "com.test.basic"
name = "BasicTest"
version = "1.0.0"

[[files]]
source = "build/*"
dest = "#{app}"
overwrite = "always"
"##;
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
        assert!(InstallManifest::manifest_path(&install_dir, "com.test.basic").exists());

        // Test uninstall
        let result = uninstall_package(&install_dir, "com.test.basic", &callbacks);
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

        let toml = r##"
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
dest = "#{app}"
component = "core"

[[files]]
source = "extras/*"
dest = "#{app}/extras"
component = "extras"
"##;
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
        let toml = r##"
[package]
id = "com.test.roundtrip"
name = "RoundTrip"
version = "1.0.0"
publisher = "Test Corp"
architecture = "x64"
privileges = "admin"
default_dir = "#{pf}/#{package.name}"

[logging]
enabled = true

[[files]]
source = "build/*"
dest = "#{app}"

[[registry]]
root = "hkcu"
key = "Software\\Test"
values = [{ name = "Key", type = "string", data = "Value" }]

[[shortcuts]]
name = "TestApp"
target = "#{app}/test.exe"
location = "desktop"
"##;
        let config = Config::from_toml(toml).unwrap();
        assert_eq!(config.package.id, "com.test.roundtrip");
        assert_eq!(config.package.architecture, Architecture::X64);
        assert_eq!(config.package.privileges, Privileges::Admin);
        assert_eq!(config.files.len(), 1);
        assert_eq!(config.registry.len(), 1);
        assert_eq!(config.shortcuts.len(), 1);
    }
}
