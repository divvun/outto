use std::process::Command;

use crate::config::PrerequisiteEntry;
use crate::error::{InstallerError, InstallerResult};
use crate::{InstallerCallbacks, LogLevel};

pub fn check_prerequisites(
    entries: &[PrerequisiteEntry],
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    for entry in entries {
        let met = check_single(&entry.check)?;
        if !met {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Prerequisite not met: {}", entry.name),
            );

            if entry.required {
                if let Some(ref installer) = entry.installer {
                    callbacks.on_log(
                        LogLevel::Info,
                        &format!("Running prerequisite installer: {installer}"),
                    );
                    run_prerequisite_installer(installer, entry.arguments.as_deref())?;

                    let met_after = check_single(&entry.check)?;
                    if !met_after {
                        return Err(InstallerError::Prerequisite {
                            name: entry.name.clone(),
                        });
                    }
                } else {
                    return Err(InstallerError::Prerequisite {
                        name: entry.name.clone(),
                    });
                }
            }
        } else {
            callbacks.on_log(
                LogLevel::Info,
                &format!("Prerequisite satisfied: {}", entry.name),
            );
        }
    }
    Ok(())
}

fn check_single(
    check: &crate::config::PrerequisiteCheck,
) -> InstallerResult<bool> {
    if let Some(ref reg_path) = check.registry {
        return check_registry(reg_path, check.value.as_deref(), check.equals.as_ref());
    }

    if let Some(ref file_path) = check.file {
        return Ok(std::path::Path::new(file_path).exists());
    }

    if let Some(ref cmd) = check.command {
        let output = Command::new("cmd")
            .args(["/C", cmd])
            .output()
            .map_err(|e| InstallerError::CommandExec {
                command: cmd.clone(),
                message: format!("prerequisite check failed: {e}"),
            })?;
        return Ok(output.status.success());
    }

    Ok(false)
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn check_registry(
    reg_path: &str,
    value_name: Option<&str>,
    expected: Option<&toml::Value>,
) -> InstallerResult<bool> {
    use windows_sys::Win32::System::Registry::*;

    let (root_str, key) = reg_path
        .split_once('\\')
        .ok_or_else(|| InstallerError::Config(format!("invalid registry path: {reg_path}")))?;

    let hroot = crate::actions::registry::root_from_str(root_str)?;

    let key_wide = to_wide(key);
    let mut hkey: HKEY = std::ptr::null_mut();

    let result = unsafe {
        RegOpenKeyExW(hroot, key_wide.as_ptr(), 0, KEY_READ, &mut hkey)
    };

    if result != 0 {
        return Ok(false);
    }

    let found = if let Some(vname) = value_name {
        let name_wide = to_wide(vname);
        let mut data_type: u32 = 0;
        let mut data_size: u32 = 0;

        let result = unsafe {
            RegQueryValueExW(
                hkey,
                name_wide.as_ptr(),
                std::ptr::null(),
                &mut data_type,
                std::ptr::null_mut(),
                &mut data_size,
            )
        };

        if result != 0 {
            false
        } else if let Some(exp) = expected {
            let mut buffer = vec![0u8; data_size as usize];
            unsafe {
                RegQueryValueExW(
                    hkey,
                    name_wide.as_ptr(),
                    std::ptr::null(),
                    &mut data_type,
                    buffer.as_mut_ptr(),
                    &mut data_size,
                );
            }

            match (data_type, exp) {
                (REG_DWORD, toml::Value::Integer(expected_int)) => {
                    if buffer.len() >= 4 {
                        let val = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
                        val as i64 == *expected_int
                    } else {
                        false
                    }
                }
                (REG_SZ | REG_EXPAND_SZ, toml::Value::String(expected_str)) => {
                    let wide: Vec<u16> = buffer
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    let val = String::from_utf16_lossy(&wide)
                        .trim_end_matches('\0')
                        .to_string();
                    val == *expected_str
                }
                _ => true,
            }
        } else {
            true
        }
    } else {
        true
    };

    unsafe { RegCloseKey(hkey) };
    Ok(found)
}

#[cfg(not(windows))]
fn check_registry(
    _reg_path: &str,
    _value_name: Option<&str>,
    _expected: Option<&toml::Value>,
) -> InstallerResult<bool> {
    Ok(false)
}

fn run_prerequisite_installer(
    installer: &str,
    arguments: Option<&str>,
) -> InstallerResult<()> {
    let mut cmd = Command::new(installer);
    if let Some(args) = arguments {
        cmd.args(super::run::split_args(args));
    }

    let output = cmd.output().map_err(|e| InstallerError::CommandExec {
        command: installer.to_string(),
        message: format!("prerequisite installer failed: {e}"),
    })?;

    if !output.status.success() {
        return Err(InstallerError::CommandExec {
            command: installer.to_string(),
            message: format!("installer exited with {}", output.status),
        });
    }

    Ok(())
}
