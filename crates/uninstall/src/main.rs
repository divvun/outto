#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod bridge;
mod theme;

use std::path::PathBuf;

use bridge::SilentCallbacks;

#[cfg(target_os = "macos")]
use outto_macos as platform;
#[cfg(windows)]
use outto_windows as platform;

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
            install_dir = Some(PathBuf::from(arg["/DIR=".len()..].trim_matches('"')));
        } else {
            fatal_error(&format!(
                "Unknown argument: {arg}\n\nUsage: outto-uninstall --dir <install_path> [/SILENT] [/VERYSILENT]"
            ));
        }

        i += 1;
    }

    let (install_dir, package_id) = infer_target(install_dir);

    let (app_name, app_version) = load_manifest_info(&install_dir, &package_id);

    // /VERYSILENT: no GUI
    if very_silent {
        relocate_self();
        let callbacks = SilentCallbacks;
        match platform::uninstall_package(&install_dir, &package_id, &callbacks) {
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
    let state = app::AppState::new(
        app_name,
        app_version,
        install_dir,
        package_id,
        silent,
        no_cancel,
    );

    if let Err(e) = app::run(state) {
        fatal_error(&format!("Failed to start uninstaller: {e}"));
    }
}

/// Derive `(install_dir, package_id)` from either the `--dir` flag or the
/// uninstaller's own path on disk.
///
/// Windows layout: `{install_dir}/.outto/{package_id}/uninstall.exe` — the
/// grandparent dir is literally named `.outto`, so we assert that and walk up.
///
/// macOS layout: `~/Library/no.divvun.install/packages/{package_id}/uninstall.app/Contents/MacOS/Uninstall`
/// — package_id is the grandparent's grandparent's file name. `install_dir`
/// doesn't actually matter for macOS uninstall (receipt-based), so we read it
/// from the receipt when available, else fall back to an empty PathBuf.
#[cfg(windows)]
fn infer_target(cli_install_dir: Option<PathBuf>) -> (PathBuf, String) {
    if let Some(dir) = cli_install_dir {
        let pkg_id = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .and_then(|p| p.file_name())
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        return (dir, pkg_id);
    }
    let Ok(exe) = std::env::current_exe() else {
        fatal_error("Missing --dir and could not determine own location.");
    };
    let Some(pkg_dir) = exe.parent() else {
        fatal_error("Could not determine uninstaller directory.");
    };
    let package_id = pkg_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let Some(outto_dir) = pkg_dir.parent() else {
        fatal_error("Could not determine .outto directory.");
    };
    if !outto_dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".outto"))
    {
        fatal_error(
            "Missing --dir <path>\n\nUsage: outto-uninstall --dir <install_path> [/SILENT] [/VERYSILENT]",
        );
    }
    let Some(install_dir) = outto_dir.parent() else {
        fatal_error("Could not determine install directory.");
    };
    (install_dir.to_path_buf(), package_id)
}

#[cfg(target_os = "macos")]
fn infer_target(cli_install_dir: Option<PathBuf>) -> (PathBuf, String) {
    let Ok(exe) = std::env::current_exe() else {
        fatal_error("Could not determine own location.");
    };
    // Expected: .../packages/<pkg-id>/uninstall.app/Contents/MacOS/Uninstall
    // Walk up: Uninstall → MacOS → Contents → uninstall.app → <pkg-id>
    let pkg_dir = exe
        .ancestors()
        .nth(4)
        .ok_or("")
        .unwrap_or_else(|_| fatal_error("Could not locate enclosing package receipt directory."));
    let package_id = pkg_dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| fatal_error("Could not derive package id from receipt path."));

    // Prefer the explicit --dir if provided, else pull install_dir out of the
    // receipt (receipts carry it verbatim), else an empty path — uninstall
    // doesn't actually dereference it on macOS.
    let install_dir = cli_install_dir
        .or_else(|| {
            outto_macos::detect::detect_existing_install(&package_id)
                .ok()
                .flatten()
                .map(|e| e.install_dir)
        })
        .unwrap_or_default();
    (install_dir, package_id)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn infer_target(_cli_install_dir: Option<PathBuf>) -> (PathBuf, String) {
    fatal_error("outto-uninstall is only supported on Windows and macOS");
}

/// Move our own exe out of the install directory into temp so we can delete
/// our own enclosing directory later.
///
/// Windows allows renaming a running exe; macOS doesn't need it (posix lets
/// you delete a running binary, it just won't reclaim space until the process
/// exits — which is fine for an uninstaller).
#[cfg(windows)]
pub fn relocate_self() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let in_outto = exe
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".outto"));
    if !in_outto {
        return;
    }
    let temp_path =
        std::env::temp_dir().join(format!("outto-uninstall-{}.exe", std::process::id()));
    let _ = std::fs::rename(&exe, &temp_path);
}

#[cfg(not(windows))]
pub fn relocate_self() {
    // No-op on macOS; posix is happy to delete a running binary.
}

/// Clean up after uninstall finishes: on Windows, the `.outto` dir under the
/// install directory; on macOS, the receipt dir at
/// `~/Library/no.divvun.install/packages/<pkg-id>/`.
#[cfg(windows)]
pub fn cleanup_after_uninstall(install_dir: &std::path::Path) {
    let _ = std::env::set_current_dir(std::env::temp_dir());

    let outto_dir = install_dir.join(".outto");
    if outto_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&outto_dir) {
            eprintln!("Warning: could not remove {}: {e}", outto_dir.display());
        }
    }
    if install_dir.exists() {
        let _ = std::fs::remove_dir(install_dir);
    }

    if let Ok(exe) = std::env::current_exe() {
        let _ = schedule_delete_on_reboot(&exe);
    }
}

#[cfg(target_os = "macos")]
pub fn cleanup_after_uninstall(_install_dir: &std::path::Path) {
    // The macOS uninstall path already deletes the receipt dir in
    // `outto_macos::uninstall::uninstall` via `detect::remove_receipt`.
    // If we're running from inside that dir, cd out so our own parent can go.
    let _ = std::env::set_current_dir(std::env::temp_dir());
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn cleanup_after_uninstall(_install_dir: &std::path::Path) {}

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

#[cfg(windows)]
fn load_manifest_info(install_dir: &std::path::Path, package_id: &str) -> (String, String) {
    let manifest_path = install_dir
        .join(".outto")
        .join(package_id)
        .join("manifest.json");
    parse_name_and_version(&manifest_path)
}

#[cfg(target_os = "macos")]
fn load_manifest_info(_install_dir: &std::path::Path, package_id: &str) -> (String, String) {
    // Try the user-scope receipt dir first, then system-scope. The receipt
    // carries display_name/version directly; the full manifest is alongside
    // it if we ever want the full action list.
    if let Ok(Some(existing)) = outto_macos::detect::detect_existing_install(package_id) {
        return (
            existing.display_name.unwrap_or_else(|| "Application".into()),
            existing.version.unwrap_or_default(),
        );
    }
    ("Application".to_string(), String::new())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn load_manifest_info(_install_dir: &std::path::Path, _package_id: &str) -> (String, String) {
    ("Application".to_string(), String::new())
}

#[cfg(windows)]
fn parse_name_and_version(manifest_path: &std::path::Path) -> (String, String) {
    if let Ok(data) = std::fs::read_to_string(manifest_path) {
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
            OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
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
