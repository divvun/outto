#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod bridge;
mod theme;

use std::path::PathBuf;

use bridge::SilentCallbacks;

fn main() {
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }

    // Parse arguments
    let args: Vec<String> = std::env::args().collect();

    let mut install_dir: Option<PathBuf> = None;
    let mut silent = false;
    let mut very_silent = false;
    let mut no_cancel = false;

    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        let upper = arg.to_uppercase();

        if upper == "/SILENT" {
            silent = true;
        } else if upper == "/VERYSILENT" {
            very_silent = true;
            silent = true;
        } else if upper == "/NOCANCEL" {
            no_cancel = true;
        } else if arg == "--dir" {
            i += 1;
            install_dir = args.get(i).map(PathBuf::from);
        } else if upper.starts_with("/DIR=") {
            install_dir = Some(PathBuf::from(
                arg["/DIR=".len()..].trim_matches('"'),
            ));
        } else {
            fatal_error(&format!(
                "Unknown argument: {arg}\n\nUsage: outto-uninstall --dir <install_path> [/SILENT] [/VERYSILENT]"
            ));
        }

        i += 1;
    }

    // Infer install_dir from own location if not provided
    let install_dir = install_dir.unwrap_or_else(|| {
        let Ok(exe) = std::env::current_exe() else {
            fatal_error("Missing --dir and could not determine own location.");
        };
        let Some(parent) = exe.parent() else {
            fatal_error("Could not determine uninstaller directory.");
        };
        if !parent
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(".outto"))
        {
            fatal_error("Missing --dir <path>\n\nUsage: outto-uninstall --dir <install_path> [/SILENT] [/VERYSILENT]");
        }
        let Some(install_dir) = parent.parent() else {
            fatal_error("Could not determine install directory.");
        };
        install_dir.to_path_buf()
    });

    let (app_name, app_version) = load_manifest_info(&install_dir);

    // /VERYSILENT: no GUI
    if very_silent {
        relocate_self();
        let callbacks = SilentCallbacks;
        match outto::uninstall_from_dir(&install_dir, &callbacks) {
            Ok(()) => {
                cleanup_after_uninstall(&install_dir);
                println!("Uninstall complete.");
                std::process::exit(0);
            }
            Err(e) => {
                fatal_error(&format!("Uninstall failed: {e}"));
            }
        }
    }

    // GUI mode — relocation happens when user clicks Uninstall (in app.rs)
    let state = app::AppState::new(app_name, app_version, install_dir, silent, no_cancel);

    if let Err(e) = app::run(state) {
        fatal_error(&format!("Failed to start uninstaller: {e}"));
    }
}

/// Move our own exe out of the install directory into temp.
/// Windows allows moving a running exe. This lets us delete
/// the .outto directory later without our exe being in the way.
pub fn relocate_self() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };

    // Only relocate if we're in an .outto directory
    let in_outto = exe
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".outto"));

    if !in_outto {
        return;
    }

    let temp_path = std::env::temp_dir()
        .join(format!("outto-uninstall-{}.exe", std::process::id()));

    // Move (rename) — works on running exes on Windows
    let _ = std::fs::rename(&exe, &temp_path);
}

/// Clean up the .outto directory, install directory, and schedule our own exe for deletion.
pub fn cleanup_after_uninstall(install_dir: &std::path::Path) {
    // Move CWD out of the install dir — Windows won't delete a dir that's any process's CWD
    let _ = std::env::set_current_dir(std::env::temp_dir());

    // Delete the .outto directory (uninstall.exe was already moved out by relocate_self)
    let outto_dir = install_dir.join(".outto");
    if outto_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&outto_dir) {
            eprintln!("Warning: could not remove {}: {e}", outto_dir.display());
        }
    }

    // Remove the install directory if it's now empty
    if install_dir.exists() {
        // remove_dir only succeeds if empty
        let _ = std::fs::remove_dir(install_dir);
    }

    // Schedule our own exe (now in temp) for deletion on reboot
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let _ = schedule_delete_on_reboot(&exe);
    }
}

#[cfg(windows)]
fn schedule_delete_on_reboot(path: &std::path::Path) -> std::io::Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            wide.as_ptr(),
            std::ptr::null(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_DELAY_UNTIL_REBOOT,
        )
    };

    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn load_manifest_info(install_dir: &std::path::Path) -> (String, String) {
    let manifest_path = install_dir.join(".outto").join("manifest.json");
    if let Ok(data) = std::fs::read_to_string(&manifest_path) {
        if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&data) {
            let name = manifest["package_name"]
                .as_str()
                .unwrap_or("Application")
                .to_string();
            let version = manifest["package_version"]
                .as_str()
                .unwrap_or("")
                .to_string();
            return (name, version);
        }
    }
    ("Application".to_string(), String::new())
}

fn fatal_error(msg: &str) -> ! {
    eprintln!("Error: {msg}");

    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        fn to_wide(s: &str) -> Vec<u16> {
            OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
        }

        let text = to_wide(msg);
        let title = to_wide("Outto Uninstaller");

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
