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
pub fn needs_elevation(privileges: &crate::config::Privileges) -> bool {
    match privileges {
        crate::config::Privileges::Admin => !is_elevated(),
        crate::config::Privileges::User => false,
        crate::config::Privileges::Auto => false,
    }
}

/// Get architecture of the current system.
pub fn get_system_architecture() -> &'static str {
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
