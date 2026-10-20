use outto_core::config::Privileges;

/// Check if the current process is running with elevated (admin) privileges.
pub fn is_elevated() -> bool {
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Security::*;
    use windows_sys::Win32::System::Threading::*;

    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }

        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut size: u32 = std::mem::size_of::<TOKEN_ELEVATION>() as u32;

        let result = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut std::ffi::c_void,
            size,
            &mut size,
        );

        CloseHandle(token);

        result != 0 && elevation.TokenIsElevated != 0
    }
}

/// Check whether elevation is needed for the given privileges setting.
pub fn needs_elevation(privileges: &Privileges) -> bool {
    match privileges {
        Privileges::Admin => !is_elevated(),
        Privileges::User => false,
        Privileges::Auto => false,
    }
}

/// The architecture of Windows itself: `x64`, `x86`, `arm64` or `unknown`.
///
/// The installer is an x64 program, which Arm64 Windows 11 runs under
/// emulation; there `GetNativeSystemInfo` reports the emulated x64.
/// `IsWow64Process2` (Windows 10 1511 and later) reports the real machine,
/// so it is asked first, looked up at run time so older Windows still loads
/// the installer.
pub fn get_system_architecture() -> &'static str {
    native_machine_from_wow64()
        .map(machine_name)
        .unwrap_or_else(native_system_info_architecture)
}

fn machine_name(machine: u16) -> &'static str {
    use windows_sys::Win32::System::SystemInformation::*;
    match machine {
        IMAGE_FILE_MACHINE_AMD64 => "x64",
        IMAGE_FILE_MACHINE_I386 => "x86",
        IMAGE_FILE_MACHINE_ARM64 => "arm64",
        _ => "unknown",
    }
}

fn native_machine_from_wow64() -> Option<u16> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use windows_sys::core::BOOL;

    type IsWow64Process2 = unsafe extern "system" fn(HANDLE, *mut u16, *mut u16) -> BOOL;

    let kernel32: Vec<u16> = "kernel32.dll\0".encode_utf16().collect();
    unsafe {
        let module = GetModuleHandleW(kernel32.as_ptr());
        if module.is_null() {
            return None;
        }
        let proc = GetProcAddress(module, c"IsWow64Process2".as_ptr().cast())?;
        let is_wow64_process2 =
            std::mem::transmute::<unsafe extern "system" fn() -> isize, IsWow64Process2>(proc);
        let mut process: u16 = 0;
        let mut native: u16 = 0;
        (is_wow64_process2(GetCurrentProcess(), &mut process, &mut native) != 0).then_some(native)
    }
}

fn native_system_info_architecture() -> &'static str {
    use windows_sys::Win32::System::SystemInformation::*;

    let mut info = std::mem::MaybeUninit::<SYSTEM_INFO>::zeroed();
    unsafe {
        GetNativeSystemInfo(info.as_mut_ptr());
        let info = info.assume_init();
        match info.Anonymous.Anonymous.wProcessorArchitecture {
            PROCESSOR_ARCHITECTURE_AMD64 => "x64",
            PROCESSOR_ARCHITECTURE_INTEL => "x86",
            PROCESSOR_ARCHITECTURE_ARM64 => "arm64",
            _ => "unknown",
        }
    }
}
