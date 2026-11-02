//! `[[retire]]`: removing installs this package's installer did not make,
//! such as those of the older product it takes the place of, once this
//! package has installed. See [`outto_core::config::RetireEntry`].
//!
//! Nothing here is recorded in the manifest, so neither a rollback nor this
//! package's uninstall touches it, and nothing here fails the install: each
//! step is logged, and one that fails is logged and the next one runs.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{Config, RetireStep, VariableResolver, retire_steps, split_registry_key};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::orphans::path_key;
use outto_core::manifest::rollback::{is_protected_dir, protected_dirs};

use crate::actions::{registry, services};
use crate::{detect, discard};

/// How long an ended program may take to exit and let go of its files.
const EXIT_TIMEOUT_MS: u32 = 10_000;

/// Retire every `[[retire]]` entry of `config`, once the install into
/// `install_dir` has succeeded and been registered. Returns how many steps
/// failed.
pub fn retire(
    config: &Config,
    resolver: &VariableResolver,
    install_dir: &Path,
    callbacks: &dyn InstallerCallbacks,
) -> usize {
    let mut failed = 0;
    for (entry, step) in retire_steps(&config.retire) {
        let log = |level: LogLevel, message: &str| {
            callbacks.on_log(level, &format!("Retire {}: {message}", entry.name));
        };
        let result = match step {
            RetireStep::EndProcess(image) => end_processes(image, &log),
            RetireStep::DeleteService(name) => delete_service(name, &log),
            RetireStep::RemoveInnoFiles(guid) => {
                remove_inno_files(guid, install_dir, callbacks, &log)
            }
            RetireStep::RemoveShortcut(shortcut) => remove_shortcut(shortcut, resolver, &log),
            RetireStep::DeleteRegistryKey(key) => delete_registry_key(key, &log),
            RetireStep::RemoveInnoEntry(guid) => remove_inno_entries(guid, &log),
        };
        if let Err(e) = result {
            failed += 1;
            log(LogLevel::Warn, &format!("{e}"));
        }
    }
    if failed > 0 {
        callbacks.on_log(
            LogLevel::Warn,
            &format!(
                "Retire: {failed} step(s) failed and were left as they are; \
                 the installation stands"
            ),
        );
    }
    failed
}

type Log<'a> = dyn Fn(LogLevel, &str) + 'a;

fn step_error(message: String) -> InstallerError {
    InstallerError::Other(message)
}

/// End every running process whose image name is `image`, but this one
/// and the ones that started it, such as the updater that runs this
/// installer, which waits for it.
fn end_processes(image: &str, log: &Log) -> InstallerResult<()> {
    let list = processes()?;
    let spared = ancestry(&list, unsafe {
        windows_sys::Win32::System::Threading::GetCurrentProcessId()
    });
    let mut running = false;
    let mut errors = Vec::new();
    for process in list.iter().filter(|p| p.image.eq_ignore_ascii_case(image)) {
        running = true;
        if spared.contains(&process.pid) {
            log(
                LogLevel::Info,
                &format!(
                    "not ending {image} (process {}), which started this installer",
                    process.pid
                ),
            );
            continue;
        }
        match end_process(process.pid) {
            Ok(()) => log(
                LogLevel::Info,
                &format!("ended {image} (process {})", process.pid),
            ),
            Err(e) => errors.push(format!("process {}: {e}", process.pid)),
        }
    }
    if !running {
        log(LogLevel::Info, &format!("{image} is not running"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(step_error(format!(
            "could not end {image}: {}",
            errors.join("; ")
        )))
    }
}

struct Process {
    pid: u32,
    parent: u32,
    image: String,
}

fn processes() -> InstallerResult<Vec<Process>> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(step_error(format!(
            "could not list processes: {}",
            std::io::Error::last_os_error()
        )));
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut list = Vec::new();
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while more {
        let name = &entry.szExeFile;
        let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
        list.push(Process {
            pid: entry.th32ProcessID,
            parent: entry.th32ParentProcessID,
            image: String::from_utf16_lossy(&name[..len]),
        });
        more = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    Ok(list)
}

/// `pid` and its parent, its parent's parent and so on, as far as `list`
/// knows them.
fn ancestry(list: &[Process], pid: u32) -> HashSet<u32> {
    let mut chain = HashSet::new();
    let mut current = pid;
    while current != 0 && chain.insert(current) {
        match list.iter().find(|p| p.pid == current) {
            Some(process) => current = process.parent,
            None => break,
        }
    }
    chain
}

/// End the process `pid` and wait for it to exit, so that its files can be
/// removed. One that has exited already counts as ended.
fn end_process(pid: u32) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    let process = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(code) if code == ERROR_INVALID_PARAMETER as i32 => Ok(()),
            _ => Err(err),
        };
    }
    let result = if unsafe { TerminateProcess(process, 1) } != 0
        || unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0
    {
        unsafe { WaitForSingleObject(process, EXIT_TIMEOUT_MS) };
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    };
    unsafe { CloseHandle(process) };
    result
}

/// Stop the service `name`, waiting until it has, and delete it. One that
/// will not stop is deleted all the same, which the SCM carries out once it
/// does stop.
fn delete_service(name: &str, log: &Log) -> InstallerResult<()> {
    if !services::is_registered(name)? {
        log(LogLevel::Info, &format!("service {name} is not registered"));
        return Ok(());
    }
    log(LogLevel::Info, &format!("stopping service {name}"));
    let stopped = services::stop_service(name);
    services::delete_service(name)?;
    log(LogLevel::Info, &format!("deleted service {name}"));
    stopped.map_err(|e| {
        step_error(format!(
            "service {name} is marked for deletion, but did not stop: {e}"
        ))
    })
}

/// Remove the directory each Add/Remove Programs entry of the Inno Setup
/// AppId `guid` names, with everything in it. A file in use, such as the
/// running program that started this installer, is moved aside and
/// deleted at the next restart.
fn remove_inno_files(
    guid: &str,
    install_dir: &Path,
    callbacks: &dyn InstallerCallbacks,
    log: &Log,
) -> InstallerResult<()> {
    let installs = detect::legacy_inno_installs(guid);
    if installs.is_empty() {
        log(
            LogLevel::Info,
            &format!("no Inno Setup install of {{{guid}}} is registered"),
        );
        return Ok(());
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    for install in installs {
        let dir = install.install_dir;
        if dir.as_os_str().is_empty() {
            log(
                LogLevel::Warn,
                &format!(
                    "{}\\{} names no install directory",
                    install.root, install.key
                ),
            );
        } else if !dirs.iter().any(|d| same_path(d, &dir)) {
            dirs.push(dir);
        }
    }
    let errors: Vec<String> = dirs
        .iter()
        .filter_map(|dir| {
            remove_install_dir(dir, install_dir, callbacks, log)
                .err()
                .map(|e| e.to_string())
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(step_error(errors.join("; ")))
    }
}

fn remove_install_dir(
    dir: &Path,
    own: &Path,
    callbacks: &dyn InstallerCallbacks,
    log: &Log,
) -> InstallerResult<()> {
    if !dir.is_absolute() {
        return Err(step_error(format!(
            "{} is not an absolute path; left it in place",
            dir.display()
        )));
    }
    if is_protected_dir(dir, &protected_dirs()) {
        return Err(step_error(format!(
            "{} is a system directory; left it in place",
            dir.display()
        )));
    }
    if overlaps(dir, own) {
        return Err(step_error(format!(
            "{} is, holds or is inside this package's install directory {}; \
             left it in place",
            dir.display(),
            own.display()
        )));
    }
    if !dir.exists() {
        log(
            LogLevel::Info,
            &format!("{} is already gone", dir.display()),
        );
        return Ok(());
    }
    log(LogLevel::Info, &format!("removing {}", dir.display()));
    match discard::discard_tree(dir, callbacks) {
        0 => Ok(()),
        n => Err(step_error(format!(
            "{n} item(s) in {} could not be removed",
            dir.display()
        ))),
    }
}

fn key(path: &Path) -> String {
    path_key(path, None, true)
}

fn same_path(a: &Path, b: &Path) -> bool {
    key(a) == key(b)
}

/// Whether `a` and `b` are the same directory or one is inside the other.
fn overlaps(a: &Path, b: &Path) -> bool {
    let (a, b) = (key(a), key(b));
    let inside = |inner: &str, outer: &str| {
        inner
            .strip_prefix(outer)
            .is_some_and(|rest| outer.ends_with('\\') || rest.starts_with('\\'))
    };
    a == b || inside(&a, &b) || inside(&b, &a)
}

/// Delete the shortcut `shortcut`, and the folders it was in once empty,
/// up to the Start Menu, Startup or desktop folder they are in.
fn remove_shortcut(shortcut: &str, resolver: &VariableResolver, log: &Log) -> InstallerResult<()> {
    let path = resolver.resolve_path(shortcut)?;
    if !path.exists() {
        log(
            LogLevel::Info,
            &format!("{} is already gone", path.display()),
        );
        return Ok(());
    }
    std::fs::remove_file(&path).map_err(|e| InstallerError::FileOp {
        path: path.clone(),
        source: e,
    })?;
    log(LogLevel::Info, &format!("removed {}", path.display()));

    let shell_folders: HashSet<String> = crate::paths::SHELL_FOLDERS
        .iter()
        .filter_map(|(name, _)| resolver.resolve(&format!("#{{{name}}}")).ok())
        .map(|folder| key(Path::new(&folder)))
        .collect();
    let protected = protected_dirs();
    for dir in path.ancestors().skip(1) {
        if shell_folders.contains(&key(dir))
            || is_protected_dir(dir, &protected)
            || std::fs::remove_dir(dir).is_err()
        {
            break;
        }
        log(
            LogLevel::Info,
            &format!("removed empty folder {}", dir.display()),
        );
    }
    Ok(())
}

fn delete_registry_key(path: &str, log: &Log) -> InstallerResult<()> {
    let (root, key) = split_registry_key(path).ok_or_else(|| InstallerError::Registry {
        key: path.to_string(),
        message: "not a key under HKLM, HKCU or HKCR".into(),
    })?;
    if registry::delete_tree(root, key)? {
        log(LogLevel::Info, &format!("deleted {path}"));
    } else {
        log(LogLevel::Info, &format!("{path} is already gone"));
    }
    Ok(())
}

/// Delete every Add/Remove Programs entry of the Inno Setup AppId `guid`.
fn remove_inno_entries(guid: &str, log: &Log) -> InstallerResult<()> {
    let installs = detect::legacy_inno_installs(guid);
    if installs.is_empty() {
        log(
            LogLevel::Info,
            &format!("no Add/Remove Programs entry for {{{guid}}} is left"),
        );
    }
    let mut errors = Vec::new();
    for install in installs {
        let entry = format!("{}\\{}", install.root, install.key);
        match registry::delete_tree(install.root, &install.key) {
            Ok(_) => log(LogLevel::Info, &format!("deleted {entry}")),
            Err(e) => errors.push(format!("{entry}: {e}")),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(step_error(format!(
            "could not delete {}",
            errors.join("; ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, parent: u32) -> Process {
        Process {
            pid,
            parent,
            image: format!("p{pid}.exe"),
        }
    }

    #[test]
    fn ancestry_follows_parents_and_stops_at_a_loop() {
        let list = [
            process(4, 0),
            process(100, 4),
            process(200, 100),
            process(300, 200),
            process(400, 100),
            process(500, 600),
            process(600, 500),
        ];
        assert_eq!(ancestry(&list, 300), HashSet::from([300, 200, 100, 4]));
        assert_eq!(ancestry(&list, 500), HashSet::from([500, 600]));
        assert_eq!(ancestry(&list, 700), HashSet::from([700]));
    }

    #[test]
    fn overlapping_directories() {
        let own = Path::new(r"C:\Program Files\Pahkat");
        assert!(overlaps(Path::new(r"C:\Program Files\Pahkat"), own));
        assert!(overlaps(Path::new(r"c:\program files\pahkat\"), own));
        assert!(overlaps(Path::new(r"C:\Program Files"), own));
        assert!(overlaps(Path::new(r"C:\Program Files\Pahkat\old"), own));
        assert!(overlaps(Path::new(r"C:\"), own));
        assert!(!overlaps(
            Path::new(r"C:\Program Files\Pahkat Service"),
            own
        ));
        assert!(!overlaps(
            Path::new(r"C:\Program Files (x86)\Pahkat Service"),
            own
        ));
    }

    #[test]
    fn a_missing_install_registry_key_and_shortcut_are_already_gone() {
        let logs = std::sync::Mutex::new(Vec::new());
        let log = |_: LogLevel, m: &str| logs.lock().unwrap().push(m.to_string());
        let guid = "0D8E3C9A-6F1B-4E27-9A5D-7C3B2E1F0A99";
        remove_inno_files(guid, Path::new(r"C:\x"), &outto_core::NoOpCallbacks, &log).unwrap();
        remove_inno_entries(guid, &log).unwrap();
        delete_registry_key(r"HKCU\Software\OuttoTest_retire_absent", &log).unwrap();
        let resolver = VariableResolver::new().with_windows_paths(true);
        let shortcut = std::env::temp_dir().join("outto_test_retire_absent.lnk");
        remove_shortcut(&shortcut.display().to_string(), &resolver, &log).unwrap();
        end_processes("outto-test-retire-absent.exe", &log).unwrap();
        delete_service("OuttoTestRetireAbsent", &log).unwrap();
        let logs = logs.into_inner().unwrap();
        assert_eq!(logs.len(), 6, "{logs:?}");
    }
}
