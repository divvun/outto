use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{ServiceEntry, ServiceOnInstall, ServiceStartType, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

pub fn install_service(
    entry: &ServiceEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let executable = resolver.resolve(&entry.executable)?;
    let display_name = entry.display_name.as_deref().unwrap_or(&entry.name);

    callbacks.on_log(
        LogLevel::Info,
        &format!("Services: installing {} ({})", entry.name, executable),
    );

    create_windows_service(
        &entry.name,
        display_name,
        &executable,
        &entry.start_type,
        entry.account.as_deref(),
    )?;

    manifest.record(Action::ServiceInstalled {
        name: entry.name.clone(),
    });

    if matches!(entry.on_install, ServiceOnInstall::Start) {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Services: starting {}", entry.name),
        );
        start_service_by_name(&entry.name)?;
        manifest.record(Action::ServiceStarted {
            name: entry.name.clone(),
        });
    }

    Ok(())
}

fn to_wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn create_windows_service(
    name: &str,
    display_name: &str,
    executable: &str,
    start_type: &ServiceStartType,
    account: Option<&str>,
) -> InstallerResult<()> {
    use windows_sys::Win32::System::Services::*;

    let name_wide = to_wide(name);
    let display_wide = to_wide(display_name);
    let exe_wide = to_wide(executable);

    let sc_manager = unsafe {
        OpenSCManagerW(
            std::ptr::null(),
            std::ptr::null(),
            SC_MANAGER_CREATE_SERVICE,
        )
    };
    if sc_manager.is_null() {
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "failed to open Service Control Manager".into(),
        });
    }

    let dwstart = match start_type {
        ServiceStartType::Auto | ServiceStartType::DelayedAuto => SERVICE_AUTO_START,
        ServiceStartType::Manual => SERVICE_DEMAND_START,
        ServiceStartType::Disabled => SERVICE_DISABLED,
    };

    let account_wide = account.map(to_wide);
    let account_ptr = account_wide
        .as_ref()
        .map(|v| v.as_ptr())
        .unwrap_or(std::ptr::null());

    let service = unsafe {
        CreateServiceW(
            sc_manager,
            name_wide.as_ptr(),
            display_wide.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            dwstart,
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
        unsafe { CloseServiceHandle(sc_manager) };
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "CreateServiceW failed".into(),
        });
    }

    if matches!(start_type, ServiceStartType::DelayedAuto) {
        let delayed = SERVICE_DELAYED_AUTO_START_INFO {
            fDelayedAutostart: 1,
        };
        unsafe {
            ChangeServiceConfig2W(
                service,
                SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
                &delayed as *const _ as *const std::ffi::c_void,
            );
        }
    }

    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(sc_manager);
    }

    Ok(())
}

fn start_service_by_name(name: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Services::*;

    let name_wide = to_wide(name);

    let sc_manager =
        unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if sc_manager.is_null() {
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "failed to open SCM".into(),
        });
    }

    let service = unsafe { OpenServiceW(sc_manager, name_wide.as_ptr(), SERVICE_START) };
    if service.is_null() {
        unsafe { CloseServiceHandle(sc_manager) };
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "failed to open service".into(),
        });
    }

    let result = unsafe { StartServiceW(service, 0, std::ptr::null()) };
    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(sc_manager);
    }

    if result == 0 {
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "StartServiceW failed".into(),
        });
    }

    Ok(())
}

pub fn stop_service(name: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Services::*;

    let name_wide = to_wide(name);

    let sc_manager =
        unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if sc_manager.is_null() {
        return Ok(());
    }

    let service = unsafe { OpenServiceW(sc_manager, name_wide.as_ptr(), SERVICE_STOP) };
    if service.is_null() {
        unsafe { CloseServiceHandle(sc_manager) };
        return Ok(());
    }

    let mut status = std::mem::MaybeUninit::<SERVICE_STATUS>::zeroed();
    unsafe {
        ControlService(service, SERVICE_CONTROL_STOP, status.as_mut_ptr());
        CloseServiceHandle(service);
        CloseServiceHandle(sc_manager);
    }

    Ok(())
}

pub fn delete_service(name: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Services::*;

    const DELETE_ACCESS: u32 = 0x00010000;
    let name_wide = to_wide(name);

    let sc_manager =
        unsafe { OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT) };
    if sc_manager.is_null() {
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "failed to open SCM for deletion".into(),
        });
    }

    let service = unsafe { OpenServiceW(sc_manager, name_wide.as_ptr(), DELETE_ACCESS) };
    if service.is_null() {
        unsafe { CloseServiceHandle(sc_manager) };
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "failed to open service for deletion".into(),
        });
    }

    let result = unsafe { DeleteService(service) };
    unsafe {
        CloseServiceHandle(service);
        CloseServiceHandle(sc_manager);
    }

    if result == 0 {
        return Err(InstallerError::Service {
            name: name.to_string(),
            message: "DeleteService failed".into(),
        });
    }

    Ok(())
}
