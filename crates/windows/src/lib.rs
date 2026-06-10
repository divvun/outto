//! Windows backend for the outto installer framework.
//!
//! Owns the Windows-specific install and uninstall pipelines: UAC elevation
//! checks, Add/Remove Programs registration, PE section embedding, all
//! Windows-only action types (registry, COM, services, shortcuts, fonts,
//! associations, environment variables), and the rollback dispatcher that
//! reverses them. The shared framework (config parsing, manifest, rollback
//! scaffolding, neutral action primitives) lives in `outto-core`.

#![cfg(windows)]

pub mod actions;
pub mod detect;
pub mod elevation;
pub mod manifest;
pub mod paths;
pub mod pe;
pub mod uninstall;

use std::path::PathBuf;

use outto_core::callbacks::{InstallOptions, InstallerCallbacks, LogLevel};
use outto_core::config::{UpgradePolicy, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::{CoreAction, InstallManifest, rollback};

pub use manifest::Action as WindowsAction;
pub use outto_core::Config;
pub use uninstall::uninstall as uninstall_package;

/// Build a Windows-flavoured `VariableResolver`, pre-populated with package
/// metadata, install dir, and all the Windows shell-folder variables.
pub fn make_resolver(config: &Config, install_dir: Option<&std::path::Path>) -> VariableResolver {
    let mut r = VariableResolver::new()
        .with_windows_paths(true)
        .with_package(&config.package.name, &config.package.version);
    r = paths::with_windows_env(r);
    if let Some(dir) = install_dir {
        r = r.with_install_dir(dir);
    }
    r
}

/// Windows install entry point. Mirrors the pipeline the root crate used to own:
/// arch + elevation check → ARP detect → prerequisites → create install dir →
/// [`actions::execute_install`] → manifest save → ARP write → upgrade cleanup.
/// On failure, rolls back every recorded action in reverse.
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

    if !detect::arch_matches(&config.package.architecture) {
        return Err(InstallerError::Validation(format!(
            "architecture mismatch: package requires {:?} but system is {}",
            config.package.architecture,
            elevation::get_system_architecture()
        )));
    }

    if elevation::needs_elevation(&config.package.privileges) {
        return Err(InstallerError::ElevationRequired(
            "this installer requires administrator privileges".into(),
        ));
    }

    let install_dir = if let Some(ref dir) = options.install_dir {
        PathBuf::from(
            dir.to_string_lossy()
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        )
    } else if let Some(ref default_dir) = config.package.default_dir {
        let config_resolver = make_resolver(config, None);
        config_resolver.resolve_path(default_dir)?
    } else {
        return Err(InstallerError::Config(
            "no install directory specified (set install_dir in options or default_dir in config)"
                .into(),
        ));
    };

    let resolver = make_resolver(config, Some(&install_dir));

    let mut old_manifest: Option<InstallManifest<WindowsAction>> = None;
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
            UpgradePolicy::SideBySide => {}
            UpgradePolicy::Overwrite => {
                old_manifest =
                    InstallManifest::load(&existing.install_dir, &config.package.id).ok();
                if existing.install_dir != install_dir {
                    old_install_dir = Some(existing.install_dir);
                }
            }
        }
    }

    actions::check_prerequisites_windows(config, callbacks)?;

    std::fs::create_dir_all(&install_dir).map_err(|e| InstallerError::DirOp {
        path: install_dir.clone(),
        source: e,
    })?;

    let mut install_manifest = InstallManifest::<WindowsAction>::new(
        &config.package.id,
        &config.package.name,
        &config.package.version,
        &install_dir,
        config.package.depends_on.clone(),
    );
    install_manifest.record(CoreAction::DirectoryCreated {
        path: install_dir.clone(),
    });

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
            install_manifest.save()?;

            if let Some(old) = old_manifest {
                let new_files: std::collections::HashSet<PathBuf> = install_manifest
                    .actions
                    .iter()
                    .filter_map(|a| match a {
                        WindowsAction::FileCopied { dest, .. } => Some(dest.clone()),
                        _ => None,
                    })
                    .collect();

                for action in &old.actions {
                    if let WindowsAction::FileCopied { dest, .. } = action {
                        if !new_files.contains(dest) && dest.exists() {
                            callbacks.on_log(
                                LogLevel::Info,
                                &format!("Upgrade: removing orphaned file {}", dest.display()),
                            );
                            let _ = std::fs::remove_file(dest);
                        }
                    }
                }

                if let Some(ref old_dir) = old_install_dir {
                    let old_pkg_dir =
                        InstallManifest::<WindowsAction>::package_dir(old_dir, &config.package.id);
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
                    if old_dir.exists() {
                        let _ = std::fs::remove_dir(old_dir);
                    }
                }
            }

            let uninstall_string = if let Some(ref uninstall_exe_src) = options.uninstall_exe {
                let pkg_dir =
                    InstallManifest::<WindowsAction>::package_dir(&install_dir, &config.package.id);
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
            callbacks.on_log(
                LogLevel::Error,
                &format!("Installation failed: {e}. Rolling back..."),
            );

            let rollback_result =
                rollback::rollback_actions(&install_manifest.actions, callbacks, true);

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

#[cfg(test)]
mod tests {
    use super::*;
    use outto_core::callbacks::{NoOpCallbacks, Prompt, PromptResponse};
    use outto_core::config::Config;
    use outto_core::error::ErrorAction;
    use std::collections::HashSet;
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

        let _ = fs::remove_dir_all(&test_dir);

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

        assert!(install_dir.join("app.exe").exists());
        assert!(install_dir.join("readme.txt").exists());

        assert!(
            InstallManifest::<WindowsAction>::manifest_path(&install_dir, "com.test.basic")
                .exists()
        );

        let result = uninstall_package(&install_dir, "com.test.basic", &callbacks);
        assert!(result.is_ok(), "Uninstall failed: {result:?}");

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
        assert!(!install_dir.join("extras/plugin.dll").exists());

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
}
