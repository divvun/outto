#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod bridge;
mod theme;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use bridge::SilentCallbacks;
use outto_core::{ExitCode, Failure};

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
    if args.get(1).is_some_and(|arg| arg == "--version") {
        println!("outto-uninstall {}", outto_core::VERSION);
        return;
    }

    // Before parsing, which can itself fail with a fatal error.
    NO_MESSAGE_BOXES.store(
        args.iter().skip(1).any(|arg| {
            let upper = arg.to_uppercase();
            upper == "/VERYSILENT"
                || upper == "/SUPPRESSMSGBOXES"
                || arg == "--very-silent"
                || arg == "--suppress-msgboxes"
        }),
        Ordering::Relaxed,
    );

    let Options {
        install_dir,
        silent,
        very_silent,
        no_cancel,
        progress_file,
        log,
    } = parse_args(args.get(1..).unwrap_or_default()).unwrap_or_else(|e| fatal_error(&e));

    if let Some(value) = &log {
        // An elevated child appends to the log its parent opened.
        outto_core::logfile::start(value.as_deref(), "Uninstall", progress_file.is_some());
        outto_core::logfile::write(
            outto_core::LogLevel::Info,
            &format!(
                "outto-uninstall {} started: {}",
                outto_core::VERSION,
                args.join(" ")
            ),
        );
    }

    let (install_dir, package_id) = infer_target(install_dir);

    let (app_name, app_version) = load_manifest_info(&install_dir, &package_id);

    // Before anything changes, the uninstaller's own place included: a
    // refused uninstall must leave it where Add/Remove Programs points.
    #[cfg(windows)]
    match outto_windows::uninstall::check_uninstall_allowed(&install_dir, &package_id) {
        Ok(()) => {}
        Err(outto_core::InstallerError::UninstallRefused(reason)) => refuse(&app_name, &reason),
        Err(e) => fail(
            &format!("Could not check whether {app_name} may be uninstalled: {e}"),
            ExitCode::Failed,
        ),
    }

    // /VERYSILENT: no GUI
    if very_silent {
        relocate_self();
        let result = run_headless(progress_file.as_deref(), &install_dir, &package_id);
        outto_core::logfile::write_result("Uninstallation", &result);
        match result {
            Ok(()) => {
                cleanup_after_uninstall(&install_dir);
                println!("Uninstall complete.");
                std::process::exit(0);
            }
            Err(failure) => {
                fail(&format!("Uninstall failed: {failure}"), failure.exit);
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
        fail(
            &format!("Failed to start uninstaller: {e}"),
            ExitCode::Failed,
        );
    }
    restore_self();
}

/// What the command line asks of the uninstaller.
#[derive(Debug, Default, PartialEq)]
struct Options {
    install_dir: Option<PathBuf>,
    silent: bool,
    very_silent: bool,
    no_cancel: bool,
    progress_file: Option<PathBuf>,
    log: Option<Option<String>>,
}

/// Parse the arguments after the program name. The standard `--flags` match
/// the installer's. The Inno Setup switches that package managers pass to an
/// Inno uninstaller are accepted too, so a bare
/// `uninstall.exe /VERYSILENT /SP- /SUPPRESSMSGBOXES /NORESTART` (pahkat's
/// Inno uninstall arguments, which replace the ones `QuietUninstallString`
/// carries) works: without `--dir`, the install is found from the
/// uninstaller's own location (`infer_target`).
fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        let upper = arg.to_uppercase();

        if upper == "/SILENT" || arg == "--silent" {
            options.silent = true;
        } else if upper == "/VERYSILENT" || arg == "--very-silent" {
            options.very_silent = true;
            options.silent = true;
        } else if upper == "/SUPPRESSMSGBOXES" || arg == "--suppress-msgboxes" {
            // Prompts are always auto-answered in this binary; this only
            // silences the error and refusal boxes (NO_MESSAGE_BOXES).
        } else if upper == "/NORESTART" || arg == "--no-restart" {
            // Accepted for parity with the installer and with Inno Setup's
            // uninstaller: this one never restarts the machine.
        } else if upper == "/SP-" {
            // Accepted for parity with Inno Setup's switches: this
            // uninstaller asks no startup question to skip.
        } else if upper == "/NOCANCEL" || arg == "--no-cancel" {
            options.no_cancel = true;
        } else if arg == "--dir" {
            i += 1;
            options.install_dir = args.get(i).map(PathBuf::from);
        } else if upper.starts_with("/DIR=") {
            options.install_dir = Some(PathBuf::from(arg["/DIR=".len()..].trim_matches('"')));
        } else if let Some(value) = outto_core::logfile::parse_log_switch(arg) {
            options.log = Some(value);
        } else if arg == "--progress-file" {
            i += 1;
            options.progress_file = args.get(i).map(PathBuf::from);
        } else {
            return Err(format!(
                "Unknown argument: {arg}\n\nUsage: outto-uninstall --dir <install_path> [--silent | --very-silent] [--suppress-msgboxes] [--no-cancel] [/LOG[=path]] [--progress-file <path>]\n       outto-uninstall --version\n\nThe Inno Setup switches /SILENT, /VERYSILENT, /SUPPRESSMSGBOXES, /NORESTART, /SP-, /NOCANCEL and /DIR= are accepted too."
            ));
        }

        i += 1;
    }
    Ok(options)
}

/// Run the headless uninstall with the right callbacks: JSON events to the
/// progress file when `--progress-file` is set (the caller is another
/// program, or on macOS the GUI parent of an elevated child), otherwise plain
/// console output.
fn run_headless(
    progress_file: Option<&std::path::Path>,
    install_dir: &std::path::Path,
    package_id: &str,
) -> Result<(), Failure> {
    if let Some(path) = progress_file {
        let cb = match outto_core::progress::FileProgressCallbacks::create(path) {
            Ok(cb) => cb,
            Err(e) => {
                return Err(Failure::failed(format!(
                    "can't open progress file {}: {e}",
                    path.display()
                )));
            }
        };
        let result = platform::uninstall_package(
            install_dir,
            package_id,
            &outto_core::logfile::LoggingCallbacks::new(&cb),
        )
        .map_err(Failure::from);
        cb.write_finished(result.as_ref().map(|_| ()));
        return result;
    }
    platform::uninstall_package(
        install_dir,
        package_id,
        &outto_core::logfile::LoggingCallbacks::new(&SilentCallbacks),
    )
    .map_err(Failure::from)
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
            "Missing --dir <path>\n\nUsage: outto-uninstall --dir <install_path> [--silent | --very-silent]",
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
    let pkg_dir =
        exe.ancestors().nth(4).ok_or("").unwrap_or_else(|_| {
            fatal_error("Could not locate enclosing package receipt directory.")
        });
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
    if !in_outto_dir(&exe) {
        return;
    }
    let temp = std::env::temp_dir().join(outto_windows::self_delete::relocated_name(
        std::process::id(),
    ));
    if std::fs::rename(&exe, &temp).is_ok()
        && let Ok(mut relocated) = RELOCATED.lock()
    {
        *relocated = Some(Relocated {
            temp,
            original: exe,
        });
    }
}

/// Whether `exe` is the uninstaller kept in `.outto\<package id>\`.
#[cfg(windows)]
fn in_outto_dir(exe: &std::path::Path) -> bool {
    exe.parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".outto"))
}

/// Where [`relocate_self`] moved the running exe, and from where.
/// `current_exe()` keeps reporting the original path after the rename, so
/// this is the only record of the copy in the temp directory.
#[cfg(windows)]
struct Relocated {
    temp: PathBuf,
    original: PathBuf,
}

#[cfg(windows)]
static RELOCATED: std::sync::Mutex<Option<Relocated>> = std::sync::Mutex::new(None);

#[cfg(windows)]
fn take_relocated() -> Option<Relocated> {
    RELOCATED.lock().ok().and_then(|mut r| r.take())
}

/// After an uninstall that did not finish, put the exe back where Add/Remove
/// Programs runs it from, or failing that, delete the temp copy.
#[cfg(windows)]
pub fn restore_self() {
    use outto_core::LogLevel;
    use outto_core::logfile::write as log;

    let Some(relocated) = take_relocated() else {
        return;
    };
    match std::fs::rename(&relocated.temp, &relocated.original) {
        Ok(()) => log(
            LogLevel::Info,
            &format!(
                "Moved the uninstaller back to {}",
                relocated.original.display()
            ),
        ),
        Err(e) => {
            log(
                LogLevel::Warn,
                &format!(
                    "Could not move the uninstaller back to {}: {e}",
                    relocated.original.display()
                ),
            );
            delete_after_exit(&relocated.temp);
        }
    }
}

#[cfg(not(windows))]
pub fn relocate_self() {
    // No-op on macOS; posix is happy to delete a running binary.
}

#[cfg(not(windows))]
pub fn restore_self() {}

/// Clean up after uninstall finishes: on Windows, the `.outto` dir under the
/// install directory; on macOS, the receipt dir at
/// `~/Library/no.divvun.install/packages/<pkg-id>/`.
#[cfg(windows)]
pub fn cleanup_after_uninstall(install_dir: &std::path::Path) {
    let _ = std::env::set_current_dir(std::env::temp_dir());

    use outto_core::LogLevel;
    use outto_core::logfile::write as log;

    let outto_dir = install_dir.join(".outto");
    if outto_dir.exists() {
        match std::fs::remove_dir_all(&outto_dir) {
            Ok(()) => log(LogLevel::Info, &format!("Removed {}", outto_dir.display())),
            Err(e) => {
                eprintln!("Warning: could not remove {}: {e}", outto_dir.display());
                log(
                    LogLevel::Warn,
                    &format!("Could not remove {}: {e}", outto_dir.display()),
                );
            }
        }
    }
    if install_dir.exists() {
        match std::fs::remove_dir(install_dir) {
            Ok(()) => log(
                LogLevel::Info,
                &format!("Removed {}", install_dir.display()),
            ),
            Err(e) => log(
                LogLevel::Info,
                &format!("Left {} in place: {e}", install_dir.display()),
            ),
        }
    }

    if let Some(relocated) = take_relocated() {
        delete_after_exit(&relocated.temp);
        return;
    }
    // Not moved out of `.outto`, so still in it: a running exe can't be
    // deleted, only scheduled for deletion at restart.
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    if !in_outto_dir(&exe) {
        return;
    }
    match schedule_delete_on_reboot(&exe) {
        Ok(()) => log(
            LogLevel::Info,
            &format!("Scheduled {} for deletion at reboot", exe.display()),
        ),
        Err(e) => log(
            LogLevel::Warn,
            &format!("Could not schedule {} for deletion: {e}", exe.display()),
        ),
    }
}

/// Have the temp copy of the exe deleted once this process has exited.
#[cfg(windows)]
fn delete_after_exit(temp: &std::path::Path) {
    use outto_core::LogLevel;
    use outto_core::logfile::write as log;
    use outto_windows::self_delete::{Removal, delete_after_exit};

    match delete_after_exit(temp) {
        Ok(Removal::AfterExit { helper_pid }) => log(
            LogLevel::Info,
            &format!(
                "Deleting {} once the uninstaller has exited (cmd.exe, process {helper_pid})",
                temp.display()
            ),
        ),
        Ok(Removal::AtRestart(e)) => log(
            LogLevel::Warn,
            &format!(
                "Could not start cmd.exe to delete {} ({e}); scheduled it for deletion at reboot",
                temp.display()
            ),
        ),
        Err(e) => log(
            LogLevel::Warn,
            &format!("Could not delete {}: {e}", temp.display()),
        ),
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
            existing
                .display_name
                .unwrap_or_else(|| "Application".into()),
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

/// Set by `/VERYSILENT` and `/SUPPRESSMSGBOXES`: report errors on stderr and
/// in the log only. A silent uninstaller usually runs from another program
/// that waits for it, and a message box would block that program for good.
static NO_MESSAGE_BOXES: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
fn refuse(app_name: &str, reason: &str) -> ! {
    eprintln!("Uninstall refused: {reason}");
    outto_core::logfile::write(
        outto_core::LogLevel::Warn,
        &format!("Uninstall refused: {reason}"),
    );
    if !NO_MESSAGE_BOXES.load(Ordering::Relaxed) {
        message_box(
            reason,
            &format!("{app_name} Uninstall"),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONINFORMATION,
        );
    }
    std::process::exit(ExitCode::Refused.code());
}

#[cfg(windows)]
fn message_box(text: &str, title: &str, icon: u32) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    fn to_wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    let text = to_wide(text);
    let title = to_wide(title);
    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK | icon,
        );
    }
}

/// Exit because the command line or the uninstaller's own location doesn't
/// say what to uninstall.
fn fatal_error(msg: &str) -> ! {
    fail(msg, ExitCode::InvalidInvocation)
}

fn fail(msg: &str, exit: ExitCode) -> ! {
    eprintln!("Error: {msg}");
    outto_core::logfile::write(outto_core::LogLevel::Error, msg);
    restore_self();

    #[cfg(windows)]
    if !NO_MESSAGE_BOXES.load(Ordering::Relaxed) {
        message_box(
            msg,
            "Outto Uninstaller",
            windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }

    std::process::exit(exit.code());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_args(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn accepts_inno_uninstall_switches_without_dir() {
        let options = parse(&["/VERYSILENT", "/SP-", "/SUPPRESSMSGBOXES", "/NORESTART"]).unwrap();
        assert_eq!(
            options,
            Options {
                silent: true,
                very_silent: true,
                ..Options::default()
            }
        );
        assert_eq!(parse(&["/sp-"]).unwrap(), Options::default());
    }

    #[test]
    fn reads_the_install_dir_and_log() {
        let options = parse(&[
            "--dir",
            r"C:\Program Files\App",
            "/VERYSILENT",
            "/LOG=x.log",
        ])
        .unwrap();
        assert_eq!(
            options.install_dir,
            Some(PathBuf::from(r"C:\Program Files\App"))
        );
        assert_eq!(options.log, Some(Some("x.log".to_string())));
        assert_eq!(
            parse(&[r#"/DIR="C:\App""#]).unwrap().install_dir,
            Some(PathBuf::from(r"C:\App"))
        );
    }

    #[test]
    fn accepts_the_installers_standard_flags() {
        let options = parse(&[
            "--dir",
            "/Applications/App",
            "--very-silent",
            "--suppress-msgboxes",
            "--no-restart",
            "--no-cancel",
            "--progress-file",
            "progress.jsonl",
        ])
        .unwrap();
        assert_eq!(
            options,
            Options {
                install_dir: Some(PathBuf::from("/Applications/App")),
                silent: true,
                very_silent: true,
                no_cancel: true,
                progress_file: Some(PathBuf::from("progress.jsonl")),
                ..Options::default()
            }
        );
        assert_eq!(
            parse(&["--silent"]).unwrap(),
            Options {
                silent: true,
                ..Options::default()
            }
        );
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert!(
            parse(&["/VERYSILENT", "/BOGUS"])
                .unwrap_err()
                .starts_with("Unknown argument: /BOGUS")
        );
    }
}
