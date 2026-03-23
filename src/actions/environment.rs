use crate::config::{EnvAction, EnvScope, EnvironmentEntry, PathResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel};

/// Compute the new value for an environment variable given the current value and action.
/// Returns `None` only when removing a variable that doesn't exist (no-op).
pub fn compute_env_value(
    action: &EnvAction,
    current: Option<&str>,
    new_value: &str,
) -> Option<String> {
    match action {
        EnvAction::Set => Some(new_value.to_string()),
        EnvAction::Append => {
            let current = current.unwrap_or("");
            Some(if current.is_empty() {
                new_value.to_string()
            } else {
                format!("{current};{new_value}")
            })
        }
        EnvAction::Prepend => {
            let current = current.unwrap_or("");
            Some(if current.is_empty() {
                new_value.to_string()
            } else {
                format!("{new_value};{current}")
            })
        }
        EnvAction::Remove => current.map(|c| {
            c.split(';')
                .filter(|part| *part != new_value)
                .collect::<Vec<_>>()
                .join(";")
        }),
    }
}

pub fn apply_env_entry(
    entry: &EnvironmentEntry,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let value = resolver.resolve(&entry.value)?;
    let scope_str = match entry.scope {
        EnvScope::System => "system",
        EnvScope::User => "user",
    };
    let action_str = match entry.action {
        EnvAction::Set => "set",
        EnvAction::Append => "append",
        EnvAction::Prepend => "prepend",
        EnvAction::Remove => "remove",
    };

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "Environment: {} {} = {} ({})",
            action_str, entry.name, value, scope_str
        ),
    );

    #[cfg(windows)]
    let previous_value = {
        let prev = read_env_var(&entry.name, &entry.scope)?;

        let computed = compute_env_value(&entry.action, prev.as_deref(), &value);
        let new_value = match computed {
            Some(v) => v,
            None => return Ok(()), // Remove on nonexistent var — no-op
        };

        write_env_var(&entry.name, &new_value, &entry.scope)?;
        broadcast_settings_change();
        prev
    };

    #[cfg(not(windows))]
    let previous_value: Option<String> = {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Environment: [simulated] {action_str} {}", entry.name),
        );
        None
    };

    manifest.record(ActionRecord::EnvironmentVariableSet {
        name: entry.name.clone(),
        scope: scope_str.to_string(),
        action: action_str.to_string(),
        value,
        previous_value,
    });

    Ok(())
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn env_registry_location(
    scope: &EnvScope,
) -> (windows_sys::Win32::System::Registry::HKEY, &'static str) {
    use windows_sys::Win32::System::Registry::*;
    match scope {
        EnvScope::System => (
            HKEY_LOCAL_MACHINE,
            "SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Environment",
        ),
        EnvScope::User => (HKEY_CURRENT_USER, "Environment"),
    }
}

#[cfg(windows)]
fn read_env_var(name: &str, scope: &EnvScope) -> InstallerResult<Option<String>> {
    use windows_sys::Win32::System::Registry::*;

    let (hkey_root, subkey) = env_registry_location(scope);
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(name);

    let mut hkey: HKEY = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(hkey_root, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result != 0 {
        return Ok(None);
    }

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
        unsafe { RegCloseKey(hkey) };
        return Ok(None);
    }

    let mut buffer = vec![0u8; data_size as usize];
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            name_wide.as_ptr(),
            std::ptr::null(),
            &mut data_type,
            buffer.as_mut_ptr(),
            &mut data_size,
        )
    };
    unsafe { RegCloseKey(hkey) };

    if result != 0 {
        return Ok(None);
    }

    let wide: Vec<u16> = buffer
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let value = String::from_utf16_lossy(&wide)
        .trim_end_matches('\0')
        .to_string();

    Ok(Some(value))
}

#[cfg(windows)]
fn write_env_var(name: &str, value: &str, scope: &EnvScope) -> InstallerResult<()> {
    use windows_sys::Win32::System::Registry::*;

    let (hkey_root, subkey) = env_registry_location(scope);
    let key_wide = to_wide(subkey);
    let name_wide = to_wide(name);
    let value_wide = to_wide(value);
    let data_bytes: Vec<u8> = value_wide.iter().flat_map(|w| w.to_le_bytes()).collect();

    let mut hkey: HKEY = std::ptr::null_mut();
    let result =
        unsafe { RegOpenKeyExW(hkey_root, key_wide.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) };
    if result != 0 {
        return Err(InstallerError::Environment {
            name: name.to_string(),
            message: format!("failed to open registry key: error {result}"),
        });
    }

    let reg_type = if name.eq_ignore_ascii_case("PATH") {
        REG_EXPAND_SZ
    } else {
        REG_SZ
    };

    let result = unsafe {
        RegSetValueExW(
            hkey,
            name_wide.as_ptr(),
            0,
            reg_type,
            data_bytes.as_ptr(),
            data_bytes.len() as u32,
        )
    };
    unsafe { RegCloseKey(hkey) };

    if result != 0 {
        return Err(InstallerError::Environment {
            name: name.to_string(),
            message: format!("RegSetValueExW failed: error {result}"),
        });
    }

    Ok(())
}

#[cfg(windows)]
fn broadcast_settings_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    let env_wide = to_wide("Environment");

    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            env_wide.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(windows)]
pub fn rollback_env_var(
    name: &str,
    scope: &str,
    previous_value: Option<&str>,
) -> InstallerResult<()> {
    let env_scope = match scope {
        "system" => EnvScope::System,
        _ => EnvScope::User,
    };

    match previous_value {
        Some(prev) => write_env_var(name, prev, &env_scope),
        None => {
            use windows_sys::Win32::System::Registry::*;

            let (hkey_root, subkey) = env_registry_location(&env_scope);
            let key_wide = to_wide(subkey);
            let name_wide = to_wide(name);

            let mut hkey: HKEY = std::ptr::null_mut();
            let result =
                unsafe { RegOpenKeyExW(hkey_root, key_wide.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) };
            if result == 0 {
                unsafe {
                    RegDeleteValueW(hkey, name_wide.as_ptr());
                    RegCloseKey(hkey);
                }
            }
            broadcast_settings_change();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_replaces_value() {
        let result = compute_env_value(&EnvAction::Set, Some("old"), "new");
        assert_eq!(result, Some("new".to_string()));
    }

    #[test]
    fn test_set_when_no_current() {
        let result = compute_env_value(&EnvAction::Set, None, "value");
        assert_eq!(result, Some("value".to_string()));
    }

    #[test]
    fn test_append_to_empty() {
        let result = compute_env_value(&EnvAction::Append, None, "C");
        assert_eq!(result, Some("C".to_string()));

        let result = compute_env_value(&EnvAction::Append, Some(""), "C");
        assert_eq!(result, Some("C".to_string()));
    }

    #[test]
    fn test_append_to_existing() {
        let result = compute_env_value(&EnvAction::Append, Some("A;B"), "C");
        assert_eq!(result, Some("A;B;C".to_string()));
    }

    #[test]
    fn test_prepend_to_empty() {
        let result = compute_env_value(&EnvAction::Prepend, None, "C");
        assert_eq!(result, Some("C".to_string()));

        let result = compute_env_value(&EnvAction::Prepend, Some(""), "C");
        assert_eq!(result, Some("C".to_string()));
    }

    #[test]
    fn test_prepend_to_existing() {
        let result = compute_env_value(&EnvAction::Prepend, Some("A;B"), "C");
        assert_eq!(result, Some("C;A;B".to_string()));
    }

    #[test]
    fn test_remove_existing_entry() {
        let result = compute_env_value(&EnvAction::Remove, Some("A;B;C"), "B");
        assert_eq!(result, Some("A;C".to_string()));
    }

    #[test]
    fn test_remove_nonexistent_entry() {
        let result = compute_env_value(&EnvAction::Remove, Some("A;B;C"), "D");
        assert_eq!(result, Some("A;B;C".to_string()));
    }

    #[test]
    fn test_remove_from_none() {
        let result = compute_env_value(&EnvAction::Remove, None, "A");
        assert_eq!(result, None);
    }

    #[test]
    fn test_remove_all_entries() {
        let result = compute_env_value(&EnvAction::Remove, Some("A"), "A");
        assert_eq!(result, Some("".to_string()));
    }

    #[test]
    fn test_remove_duplicates() {
        let result = compute_env_value(&EnvAction::Remove, Some("A;B;A;C"), "A");
        assert_eq!(result, Some("B;C".to_string()));
    }

    #[test]
    fn test_append_no_double_semicolon() {
        let result = compute_env_value(&EnvAction::Append, Some("A"), "B");
        assert_eq!(result, Some("A;B".to_string()));
        assert!(!result.unwrap().contains(";;"));
    }
}
