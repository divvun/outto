//! Windows services: register a package's service, start it, and replace it
//! in place when an install finds it already registered.
//!
//! A service holds its executable open while it runs, so before the files are
//! copied [`stop_running_service`] stops it. [`install_service`] then updates
//! the existing registration with `ChangeServiceConfigW`, which keeps every
//! setting the manifest does not declare, such as recovery actions, and
//! starts the service again. A failed install puts the old configuration back
//! and starts the old service.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::Foundation::{
    ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SERVICE_MARKED_FOR_DELETE,
    ERROR_SERVICE_NOT_ACTIVE,
};
use windows_sys::Win32::System::Services::*;

use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{ServiceEntry, ServiceOnInstall, ServiceStartType, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

/// How long a service may take to stop, or to finish stopping before it is
/// started again.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// The settings an install gives a service it finds registered. A failed
/// install puts back the ones it recorded from before.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    /// The command line the service runs, as the SCM holds it.
    pub executable: String,
    pub display_name: String,
    /// The SCM start type, such as `SERVICE_AUTO_START`.
    pub start_type: u32,
    #[serde(default)]
    pub delayed_auto_start: bool,
    /// The account it runs as; `None` leaves the account as it is.
    #[serde(default)]
    pub account: Option<String>,
}

/// Stop the service `entry` declares if it is registered and running, so its
/// files can be replaced, and record that, so the services phase and a
/// failed install's rollback start it again.
pub fn stop_running_service(
    entry: &ServiceEntry,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let running = match open(&entry.name, SERVICE_QUERY_STATUS)? {
        Some(service) => query_status(&service, &entry.name)?.dwCurrentState != SERVICE_STOPPED,
        None => false,
    };
    if !running {
        return Ok(());
    }

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Services: stopping {} so its files can be replaced",
            entry.name
        ),
    );
    manifest.record(Action::ServiceStopped {
        name: entry.name.clone(),
    });
    stop_service(&entry.name)
}

pub fn install_service(
    entry: &ServiceEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let executable = resolver.resolve(&entry.executable)?;
    let image_path = image_path(&executable);
    let display_name = entry.display_name.as_deref().unwrap_or(&entry.name);

    match open(&entry.name, SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG)? {
        None => {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Services: installing {} ({})", entry.name, executable),
            );
            let service = create_windows_service(
                &entry.name,
                display_name,
                &image_path,
                &entry.start_type,
                entry.account.as_deref(),
            )?;
            manifest.record(Action::ServiceInstalled {
                name: entry.name.clone(),
            });
            if entry.start_type == ServiceStartType::DelayedAuto {
                set_delayed_auto_start(&service, &entry.name, true)?;
            }
        }
        Some(service) => {
            callbacks.on_log(
                LogLevel::Info,
                &format!(
                    "Services: {} is already registered; updating it to run {}",
                    entry.name, executable
                ),
            );
            let mut previous = query_config(&service, &entry.name)?;
            let account = entry.account.as_deref().filter(|wanted| {
                previous
                    .account
                    .as_deref()
                    .is_none_or(|current| !current.eq_ignore_ascii_case(wanted))
            });
            if account.is_none() {
                previous.account = None;
            }
            manifest.record(Action::ServiceUpdated {
                name: entry.name.clone(),
                previous: previous.clone(),
            });
            change_config(
                &service,
                &entry.name,
                &ServiceConfig {
                    executable: image_path,
                    display_name: display_name.to_string(),
                    start_type: start_type_code(&entry.start_type),
                    delayed_auto_start: entry.start_type == ServiceStartType::DelayedAuto,
                    account: account.map(str::to_string),
                },
                previous.delayed_auto_start,
            )?;
        }
    }

    let stopped_for_install = manifest
        .actions
        .iter()
        .any(|a| matches!(a, Action::ServiceStopped { name } if *name == entry.name));
    if entry.on_install == ServiceOnInstall::Start || stopped_for_install {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Services: starting {}", entry.name),
        );
        manifest.record(Action::ServiceStarted {
            name: entry.name.clone(),
        });
        start_service(&entry.name)?;
    }

    Ok(())
}

/// Put back the configuration a failed install changed.
pub fn restore_config(name: &str, previous: &ServiceConfig) -> InstallerResult<()> {
    let service = open(name, SERVICE_QUERY_CONFIG | SERVICE_CHANGE_CONFIG)?
        .ok_or_else(|| not_registered(name))?;
    let delayed = query_delayed_auto_start(&service);
    change_config(&service, name, previous, delayed)
}

/// Start the service `name`, once a stop in progress has finished. Nothing
/// to do if it is already running.
pub fn start_service(name: &str) -> InstallerResult<()> {
    let service =
        open(name, SERVICE_START | SERVICE_QUERY_STATUS)?.ok_or_else(|| not_registered(name))?;
    let deadline = Instant::now() + STOP_TIMEOUT;
    let state = wait_for(&service, name, deadline, |s| s != SERVICE_STOP_PENDING)?.dwCurrentState;
    if state == SERVICE_STOP_PENDING {
        return Err(did_not_stop(name));
    }
    if state != SERVICE_STOPPED {
        return Ok(());
    }

    if unsafe { StartServiceW(service.handle.0, 0, std::ptr::null()) } == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_SERVICE_ALREADY_RUNNING as i32) {
            return Err(os_error(name, "StartServiceW failed", err));
        }
    }
    Ok(())
}

/// Stop the service `name` and wait for its process to exit, so its files
/// can be replaced or removed. Nothing to do if it is not registered or not
/// running.
pub fn stop_service(name: &str) -> InstallerResult<()> {
    let Some(service) = open(name, SERVICE_STOP | SERVICE_QUERY_STATUS)? else {
        return Ok(());
    };
    let deadline = Instant::now() + STOP_TIMEOUT;
    // A service that is starting, pausing or resuming can't take a stop
    // request until it gets there.
    let status = wait_for(&service, name, deadline, |s| {
        !matches!(
            s,
            SERVICE_START_PENDING | SERVICE_PAUSE_PENDING | SERVICE_CONTINUE_PENDING
        )
    })?;
    if status.dwCurrentState == SERVICE_STOPPED {
        return Ok(());
    }

    if status.dwCurrentState != SERVICE_STOP_PENDING {
        let mut ignored = SERVICE_STATUS::default();
        if unsafe { ControlService(service.handle.0, SERVICE_CONTROL_STOP, &mut ignored) } == 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(ERROR_SERVICE_NOT_ACTIVE as i32) {
                return Err(os_error(
                    name,
                    "ControlService(SERVICE_CONTROL_STOP) failed",
                    err,
                ));
            }
        }
    }
    if wait_for(&service, name, deadline, |s| s == SERVICE_STOPPED)?.dwCurrentState
        != SERVICE_STOPPED
    {
        return Err(did_not_stop(name));
    }

    // A service reports itself stopped before its process exits, and the
    // process holds the executable open until it does.
    if status.dwServiceType & SERVICE_WIN32_OWN_PROCESS != 0 && status.dwProcessId != 0 {
        wait_for_exit(status.dwProcessId, deadline);
    }
    Ok(())
}

/// Delete the service `name`. One already marked for deletion, which the
/// SCM removes once nothing holds it open, counts as deleted.
pub fn delete_service(name: &str) -> InstallerResult<()> {
    const DELETE_ACCESS: u32 = 0x00010000;

    let service = open(name, DELETE_ACCESS)?.ok_or_else(|| not_registered(name))?;
    if unsafe { DeleteService(service.handle.0) } == 0 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_SERVICE_MARKED_FOR_DELETE as i32) {
            return Err(os_error(name, "DeleteService failed", err));
        }
    }
    Ok(())
}

/// Whether a service named `name` is registered.
pub fn is_registered(name: &str) -> InstallerResult<bool> {
    Ok(open(name, SERVICE_QUERY_STATUS)?.is_some())
}

fn to_wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// A Service Control Manager handle, closed when dropped.
struct ScHandle(SC_HANDLE);

impl Drop for ScHandle {
    fn drop(&mut self) {
        unsafe { CloseServiceHandle(self.0) };
    }
}

/// An open service and the SCM handle it was opened through.
struct Service {
    handle: ScHandle,
    _manager: ScHandle,
}

fn open_manager(name: &str, access: u32) -> InstallerResult<ScHandle> {
    let manager = unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), access) };
    if manager.is_null() {
        return Err(os_error(
            name,
            "failed to open Service Control Manager",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(ScHandle(manager))
}

/// Open the service `name`, or `None` if no service of that name is
/// registered.
fn open(name: &str, access: u32) -> InstallerResult<Option<Service>> {
    let manager = open_manager(name, SC_MANAGER_CONNECT)?;
    let name_wide = to_wide(name);
    let handle = unsafe { OpenServiceW(manager.0, name_wide.as_ptr(), access) };
    if handle.is_null() {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
            return Ok(None);
        }
        return Err(os_error(name, "failed to open service", err));
    }
    Ok(Some(Service {
        handle: ScHandle(handle),
        _manager: manager,
    }))
}

/// The image path the service control manager runs for `executable`,
/// quoted. Unquoted, a path with a space in it is split at the space and
/// each prefix tried as a program, so `C:\Program Files\App\svc.exe`
/// would first run `C:\Program.exe` with the service's rights.
fn image_path(executable: &str) -> String {
    format!("\"{executable}\"")
}

fn create_windows_service(
    name: &str,
    display_name: &str,
    executable: &str,
    start_type: &ServiceStartType,
    account: Option<&str>,
) -> InstallerResult<ScHandle> {
    let name_wide = to_wide(name);
    let display_wide = to_wide(display_name);
    let exe_wide = to_wide(executable);

    let sc_manager = open_manager(name, SC_MANAGER_CREATE_SERVICE)?;

    let account_wide = account.map(to_wide);
    let account_ptr = account_wide
        .as_ref()
        .map(|v| v.as_ptr())
        .unwrap_or(std::ptr::null());

    let service = unsafe {
        CreateServiceW(
            sc_manager.0,
            name_wide.as_ptr(),
            display_wide.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            start_type_code(start_type),
            SERVICE_ERROR_NORMAL,
            exe_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            account_ptr,
            std::ptr::null(),
        )
    };

    if service.is_null() {
        return Err(os_error(
            name,
            "CreateServiceW failed",
            std::io::Error::last_os_error(),
        ));
    }

    Ok(ScHandle(service))
}

fn start_type_code(start_type: &ServiceStartType) -> u32 {
    match start_type {
        ServiceStartType::Auto | ServiceStartType::DelayedAuto => SERVICE_AUTO_START,
        ServiceStartType::Manual => SERVICE_DEMAND_START,
        ServiceStartType::Disabled => SERVICE_DISABLED,
    }
}

/// Set what `config` names on an existing service, leaving the service type,
/// error control, dependencies and password, and everything outside
/// `ChangeServiceConfigW`, such as recovery actions, as they are. The account
/// changes only when `config` has one. `delayed_now` is the service's current
/// delayed auto-start flag.
fn change_config(
    service: &Service,
    name: &str,
    config: &ServiceConfig,
    delayed_now: bool,
) -> InstallerResult<()> {
    let exe_wide = to_wide(&config.executable);
    let display_wide = to_wide(&config.display_name);
    let account_wide = config.account.as_deref().map(to_wide);
    let account_ptr = account_wide
        .as_ref()
        .map(|v| v.as_ptr())
        .unwrap_or(std::ptr::null());

    let ok = unsafe {
        ChangeServiceConfigW(
            service.handle.0,
            SERVICE_NO_CHANGE,
            config.start_type,
            SERVICE_NO_CHANGE,
            exe_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null(),
            account_ptr,
            std::ptr::null(),
            display_wide.as_ptr(),
        )
    };
    if ok == 0 {
        return Err(os_error(
            name,
            "ChangeServiceConfigW failed",
            std::io::Error::last_os_error(),
        ));
    }

    if config.delayed_auto_start != delayed_now {
        set_delayed_auto_start(&service.handle, name, config.delayed_auto_start)?;
    }
    Ok(())
}

fn set_delayed_auto_start(service: &ScHandle, name: &str, delayed: bool) -> InstallerResult<()> {
    let info = SERVICE_DELAYED_AUTO_START_INFO {
        fDelayedAutostart: delayed.into(),
    };
    let ok = unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            &info as *const _ as *const std::ffi::c_void,
        )
    };
    if ok == 0 {
        return Err(os_error(
            name,
            "could not set delayed auto-start",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// The service's configuration, its current account included.
fn query_config(service: &Service, name: &str) -> InstallerResult<ServiceConfig> {
    let mut needed: u32 = 0;
    unsafe { QueryServiceConfigW(service.handle.0, std::ptr::null_mut(), 0, &mut needed) };
    // u64s keep the buffer aligned for QUERY_SERVICE_CONFIGW.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8).max(1)];
    let config = buffer.as_mut_ptr() as *mut QUERY_SERVICE_CONFIGW;
    let ok = unsafe {
        QueryServiceConfigW(
            service.handle.0,
            config,
            (buffer.len() * 8) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(os_error(
            name,
            "QueryServiceConfigW failed",
            std::io::Error::last_os_error(),
        ));
    }

    let config = unsafe { &*config };
    Ok(ServiceConfig {
        executable: unsafe { wide_ptr_to_string(config.lpBinaryPathName) },
        display_name: unsafe { wide_ptr_to_string(config.lpDisplayName) },
        start_type: config.dwStartType,
        delayed_auto_start: query_delayed_auto_start(service),
        account: Some(unsafe { wide_ptr_to_string(config.lpServiceStartName) })
            .filter(|a| !a.is_empty()),
    })
}

fn query_delayed_auto_start(service: &Service) -> bool {
    let mut info = SERVICE_DELAYED_AUTO_START_INFO::default();
    let mut needed: u32 = 0;
    let ok = unsafe {
        QueryServiceConfig2W(
            service.handle.0,
            SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
            &mut info as *mut _ as *mut u8,
            std::mem::size_of::<SERVICE_DELAYED_AUTO_START_INFO>() as u32,
            &mut needed,
        )
    };
    ok != 0 && info.fDelayedAutostart != 0
}

/// # Safety
/// `p` is null or points to a NUL-terminated UTF-16 string.
unsafe fn wide_ptr_to_string(p: windows_sys::core::PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let mut len = 0;
    while unsafe { *p.add(len) } != 0 {
        len += 1;
    }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(p, len) })
}

fn query_status(service: &Service, name: &str) -> InstallerResult<SERVICE_STATUS_PROCESS> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed: u32 = 0;
    let ok = unsafe {
        QueryServiceStatusEx(
            service.handle.0,
            SC_STATUS_PROCESS_INFO,
            &mut status as *mut _ as *mut u8,
            std::mem::size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        return Err(os_error(
            name,
            "QueryServiceStatusEx failed",
            std::io::Error::last_os_error(),
        ));
    }
    Ok(status)
}

/// Poll the service's status until `done` accepts its state or `deadline`
/// passes, and return the last status.
fn wait_for(
    service: &Service,
    name: &str,
    deadline: Instant,
    done: impl Fn(u32) -> bool,
) -> InstallerResult<SERVICE_STATUS_PROCESS> {
    loop {
        let status = query_status(service, name)?;
        if done(status.dwCurrentState) || Instant::now() >= deadline {
            return Ok(status);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Wait until the process `pid` has exited or `deadline` passes.
fn wait_for_exit(pid: u32, deadline: Instant) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    unsafe {
        WaitForSingleObject(process, remaining.as_millis().min(u32::MAX as u128) as u32);
        CloseHandle(process);
    }
}

fn os_error(name: &str, what: &str, err: std::io::Error) -> InstallerError {
    InstallerError::Service {
        name: name.to_string(),
        message: format!("{what}: {err}"),
    }
}

fn not_registered(name: &str) -> InstallerError {
    InstallerError::Service {
        name: name.to_string(),
        message: "no such service is registered".into(),
    }
}

fn did_not_stop(name: &str) -> InstallerError {
    InstallerError::Service {
        name: name.to_string(),
        message: format!("did not stop within {} seconds", STOP_TIMEOUT.as_secs()),
    }
}

#[cfg(test)]
mod tests {
    use super::image_path;

    #[test]
    fn quotes_an_image_path_with_spaces() {
        assert_eq!(
            image_path(r"C:\Program Files\App\svc.exe"),
            r#""C:\Program Files\App\svc.exe""#
        );
    }
}
