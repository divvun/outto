#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod bridge;
mod cli;
#[cfg(target_os = "macos")]
mod glass;
#[cfg(target_os = "macos")]
mod layout;
#[cfg(target_os = "macos")]
mod native_buttons;
#[cfg(any(windows, target_os = "macos"))]
mod payload;
#[cfg(windows)]
mod pe;
mod screens;
#[cfg(target_os = "macos")]
mod sidebar;
mod theme;

use std::collections::HashSet;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
use outto_macos as platform;
#[cfg(windows)]
use outto_windows as platform;

use platform::Config;

use app::{AppMode, AppState};
use bridge::SilentCallbacks;
use cli::Mode;

fn main() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    let args = match cli::parse_args() {
        Ok(a) => a,
        Err(e) => {
            fatal_error(&e);
        }
    };

    match &args.mode {
        Mode::Install {
            config_path,
            source_dir,
        } => run_install(args.flags.clone(), config_path.clone(), source_dir.clone()),
        Mode::InstallEmbedded => run_embedded_install(args.flags.clone()),
        Mode::Uninstall { dir } => run_uninstall(args.flags.clone(), dir.clone()),
    }
}

#[cfg(any(windows, target_os = "macos"))]
fn run_embedded_install(flags: cli::CliFlags) {
    let payload = match payload::extract_embedded_payload() {
        Ok(Some(p)) => p,
        Ok(None) => {
            fatal_error(
                "No embedded payload found.\n\n\
                 Use outto-cli to build a self-contained installer.",
            );
        }
        Err(e) => {
            fatal_error(&format!("Failed to extract embedded payload: {e}"));
        }
    };

    run_install_inner(
        flags,
        payload.config,
        payload.source_dir,
        payload.license_text,
        payload.uninstall_exe,
    );
}

#[cfg(not(any(windows, target_os = "macos")))]
fn run_embedded_install(_flags: cli::CliFlags) {
    fatal_error("Embedded payload extraction is only supported on Windows and macOS.");
}

fn run_install(flags: cli::CliFlags, config_path: PathBuf, source_dir: PathBuf) {
    let config = match Config::from_file(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to load config: {e}");
            std::process::exit(2);
        }
    };

    let license_text = config.package.license_file.as_ref().and_then(|lf| {
        let license_path = source_dir.join(lf);
        match std::fs::read_to_string(&license_path) {
            Ok(text) => Some(text),
            Err(e) => {
                eprintln!(
                    "Warning: could not read license file {}: {e}",
                    license_path.display()
                );
                None
            }
        }
    });

    run_install_inner(flags, config, source_dir, license_text, find_uninstaller());
}

fn run_install_inner(
    flags: cli::CliFlags,
    config: Config,
    source_dir: PathBuf,
    license_text: Option<String>,
    uninstall_exe: Option<PathBuf>,
) {
    // Use the platform's make_resolver so #{applications}/#{pf}/etc. resolve
    // while expanding default_dir.
    let resolver = platform::make_resolver(&config, None);
    let default_install_dir: Option<PathBuf> = config
        .package
        .default_dir
        .as_ref()
        .map(|d| resolver.resolve_path(d))
        .transpose()
        .unwrap_or_else(|e| fatal_error(&format!("Invalid default_dir: {e}")));

    if flags.very_silent {
        let install_dir = flags
            .dir
            .as_ref()
            .map(PathBuf::from)
            .or(default_install_dir.clone());

        let selected = flags
            .components
            .as_ref()
            .map(|list| list.iter().cloned().collect::<HashSet<String>>());

        let callbacks = SilentCallbacks;
        let options = outto_core::InstallOptions {
            source_dir,
            install_dir,
            selected_components: selected,
            uninstall_exe,
        };

        match platform::install(&config, &options, &callbacks) {
            Ok(()) => {
                println!("Installation complete.");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Installation failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let default_install_dir_str = default_install_dir
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    let state = AppState::new(
        AppMode::Install,
        config,
        flags,
        license_text,
        source_dir,
        None,
        default_install_dir_str,
        uninstall_exe,
    );

    if let Err(e) = app::run(state) {
        fatal_error(&format!("Failed to start installer: {e}"));
    }
}

fn run_uninstall(flags: cli::CliFlags, install_dir: PathBuf) {
    let config = load_config_for_uninstall(&install_dir);

    if flags.very_silent {
        let callbacks = SilentCallbacks;
        match platform::uninstall_package(&install_dir, &config.package.id, &callbacks) {
            Ok(()) => {
                println!("Uninstall complete.");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("Uninstall failed: {e}");
                std::process::exit(1);
            }
        }
    }

    let state = AppState::new(
        AppMode::Uninstall,
        config,
        flags,
        None,
        PathBuf::new(),
        Some(install_dir),
        String::new(),
        None,
    );

    if let Err(e) = app::run(state) {
        eprintln!("GUI error: {e}");
        std::process::exit(1);
    }
}

fn load_config_for_uninstall(install_dir: &std::path::Path) -> Config {
    let manifest_path = install_dir.join(".outto").join("manifest.json");
    if let Ok(data) = std::fs::read_to_string(&manifest_path) {
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&data) {
            let name = manifest["package_name"]
                .as_str()
                .unwrap_or("Application")
                .to_string();
            let version = manifest["package_version"]
                .as_str()
                .unwrap_or("0.0.0")
                .to_string();
            let id = manifest["package_id"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();

            let toml_str =
                format!("[package]\nid = \"{id}\"\nname = \"{name}\"\nversion = \"{version}\"\n");
            if let Ok(config) = Config::from_toml(&toml_str) {
                return config;
            }
        }
    }

    Config::from_toml("[package]\nid = \"unknown\"\nname = \"Application\"\nversion = \"0.0.0\"\n")
        .unwrap()
}

fn fatal_error(msg: &str) -> ! {
    eprintln!("Error: {msg}");

    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        fn to_wide(s: &str) -> Vec<u16> {
            OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        }

        let text = to_wide(msg);
        let title = to_wide("Outto Installer");

        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                    | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
            );
        }
    }

    std::process::exit(1);
}

fn find_uninstaller() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let candidates = [
        exe_dir.join("outto-uninstall.exe"),
        exe_dir.join("uninstall.exe"),
    ];

    for candidate in &candidates {
        if candidate.exists() {
            return Some(candidate.clone());
        }
    }

    None
}
