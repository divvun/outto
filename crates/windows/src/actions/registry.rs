use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use crate::manifest::Action;
use outto_core::callbacks::{InstallerCallbacks, LogLevel};
use outto_core::config::{RegistryEntry, RegistryRoot, RegistryValueType, VariableResolver};
use outto_core::error::{InstallerError, InstallerResult};
use outto_core::manifest::InstallManifest;

use windows_sys::Win32::System::Registry::*;

fn to_wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn root_to_hkey(root: &RegistryRoot) -> HKEY {
    match root {
        RegistryRoot::Hklm => HKEY_LOCAL_MACHINE,
        RegistryRoot::Hkcu => HKEY_CURRENT_USER,
        RegistryRoot::Hkcr => HKEY_CLASSES_ROOT,
    }
}

pub fn root_from_str(s: &str) -> InstallerResult<HKEY> {
    match s.to_uppercase().as_str() {
        "HKLM" => Ok(HKEY_LOCAL_MACHINE),
        "HKCU" => Ok(HKEY_CURRENT_USER),
        "HKCR" => Ok(HKEY_CLASSES_ROOT),
        _ => Err(InstallerError::Registry {
            key: s.to_string(),
            message: "unknown registry root".into(),
        }),
    }
}

fn read_existing_value(hkey: HKEY, value_name: &str) -> Option<String> {
    let name_wide = to_wide(value_name);
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
    if result != 0 || data_size == 0 {
        return None;
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
    if result != 0 {
        return None;
    }

    match data_type {
        REG_SZ | REG_EXPAND_SZ => {
            let wide: Vec<u16> = buffer
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(
                String::from_utf16_lossy(&wide)
                    .trim_end_matches('\0')
                    .to_string(),
            )
        }
        REG_DWORD if buffer.len() >= 4 => {
            let val = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]);
            Some(val.to_string())
        }
        REG_QWORD if buffer.len() >= 8 => {
            let val = u64::from_le_bytes([
                buffer[0], buffer[1], buffer[2], buffer[3], buffer[4], buffer[5], buffer[6],
                buffer[7],
            ]);
            Some(val.to_string())
        }
        _ => Some(
            buffer
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<String>(),
        ),
    }
}

pub fn apply_registry_entry(
    entry: &RegistryEntry,
    resolver: &VariableResolver,
    manifest: &mut InstallManifest<Action>,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let root_name = match entry.root {
        RegistryRoot::Hklm => "HKLM",
        RegistryRoot::Hkcu => "HKCU",
        RegistryRoot::Hkcr => "HKCR",
    };

    let key = resolver.resolve(&entry.key)?;

    callbacks.on_log(
        LogLevel::Info,
        &format!("Registry: creating {root_name}\\{key}"),
    );

    let hroot = root_to_hkey(&entry.root);
    let key_wide = to_wide(&key);
    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition: u32 = 0;

    let result = unsafe {
        RegCreateKeyExW(
            hroot,
            key_wide.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_ALL_ACCESS,
            std::ptr::null(),
            &mut hkey,
            &mut disposition,
        )
    };

    if result != 0 {
        return Err(InstallerError::Registry {
            key: format!("{root_name}\\{key}"),
            message: format!("RegCreateKeyExW failed with error code {result}"),
        });
    }

    let created = disposition == REG_CREATED_NEW_KEY;
    if created {
        manifest.record(Action::RegistryKeyCreated {
            root: root_name.to_string(),
            key: key.clone(),
            on_uninstall: entry.uninstall.clone(),
        });
    }

    for val in &entry.values {
        let resolved_data = match &val.data {
            toml::Value::String(s) => resolver.resolve(s)?,
            other => other.to_string(),
        };

        let previous = read_existing_value(hkey, &val.name);

        set_registry_value(hkey, &val.name, &val.value_type, &resolved_data)?;

        manifest.record(Action::RegistryValueSet {
            root: root_name.to_string(),
            key: key.clone(),
            value_name: val.name.clone(),
            previous_data: previous,
            on_uninstall: entry.uninstall.clone(),
        });

        callbacks.on_log(
            LogLevel::Debug,
            &format!("Registry: set {} = {}", val.name, resolved_data),
        );
    }

    unsafe {
        RegCloseKey(hkey);
    }

    Ok(())
}

fn set_registry_value(
    hkey: HKEY,
    name: &str,
    value_type: &RegistryValueType,
    data: &str,
) -> InstallerResult<()> {
    let name_wide = to_wide(name);

    let (reg_type, data_bytes) = match value_type {
        RegistryValueType::String => {
            let wide = to_wide(data);
            let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();
            (REG_SZ, bytes)
        }
        RegistryValueType::ExpandString => {
            let wide = to_wide(data);
            let bytes: Vec<u8> = wide.iter().flat_map(|w| w.to_le_bytes()).collect();
            (REG_EXPAND_SZ, bytes)
        }
        RegistryValueType::Dword => {
            let val = parse_dword(data).map_err(|msg| InstallerError::Registry {
                key: name.to_string(),
                message: msg,
            })?;
            (REG_DWORD, val.to_le_bytes().to_vec())
        }
        RegistryValueType::Qword => {
            let val = parse_qword(data).map_err(|msg| InstallerError::Registry {
                key: name.to_string(),
                message: msg,
            })?;
            (REG_QWORD, val.to_le_bytes().to_vec())
        }
        RegistryValueType::MultiString => {
            let mut bytes = Vec::new();
            for part in data.split('\0') {
                let wide = to_wide(part);
                bytes.extend(wide.iter().flat_map(|w| w.to_le_bytes()));
            }
            bytes.extend_from_slice(&[0u8, 0]);
            (REG_MULTI_SZ, bytes)
        }
        RegistryValueType::Binary => {
            let bytes = hex_to_bytes(data).map_err(|e| InstallerError::Registry {
                key: name.to_string(),
                message: format!("invalid binary data: {e}"),
            })?;
            (REG_BINARY, bytes)
        }
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

    if result != 0 {
        return Err(InstallerError::Registry {
            key: name.to_string(),
            message: format!("RegSetValueExW failed with error code {result}"),
        });
    }

    Ok(())
}

pub fn hex_to_bytes(hex: &str) -> Result<Vec<u8>, String> {
    let hex = hex.replace(' ', "");
    if !hex.len().is_multiple_of(2) {
        return Err("odd length hex string".into());
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&hex[i..i + 2], 16)
                .map_err(|e| format!("invalid hex at position {i}: {e}"))
        })
        .collect()
}

pub fn parse_dword(data: &str) -> Result<u32, String> {
    data.parse()
        .map_err(|_| format!("cannot parse '{data}' as DWORD"))
}

pub fn parse_qword(data: &str) -> Result<u64, String> {
    data.parse()
        .map_err(|_| format!("cannot parse '{data}' as QWORD"))
}

/// Win32 `ERROR_FILE_NOT_FOUND`: the key or value is already gone, which for
/// an uninstall is success.
const NOT_FOUND: u32 = 2;

pub fn delete_key(root: &str, key: &str) -> InstallerResult<()> {
    let hroot = root_from_str(root)?;
    let key_wide = to_wide(key);
    let result = unsafe { RegDeleteKeyW(hroot, key_wide.as_ptr()) };
    if result != 0 && result != NOT_FOUND {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}"),
            message: format!("RegDeleteKeyW failed with error code {result}"),
        });
    }
    Ok(())
}

/// Delete `key` if it has neither values nor subkeys; leave it otherwise.
/// Returns whether it is gone.
pub fn delete_key_if_empty(root: &str, key: &str) -> InstallerResult<bool> {
    let hroot = root_from_str(root)?;
    let key_wide = to_wide(key);
    let mut hkey: HKEY = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(hroot, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result == NOT_FOUND {
        return Ok(true);
    }
    if result != 0 {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}"),
            message: format!("RegOpenKeyExW failed: {result}"),
        });
    }
    let mut subkeys: u32 = 0;
    let mut values: u32 = 0;
    let result = unsafe {
        RegQueryInfoKeyW(
            hkey,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
            &mut subkeys,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut values,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    unsafe { RegCloseKey(hkey) };
    if result != 0 {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}"),
            message: format!("RegQueryInfoKeyW failed: {result}"),
        });
    }
    if subkeys != 0 || values != 0 {
        return Ok(false);
    }
    delete_key(root, key)?;
    Ok(true)
}

pub fn delete_value(root: &str, key: &str, value_name: &str) -> InstallerResult<()> {
    let hroot = root_from_str(root)?;
    let key_wide = to_wide(key);
    let name_wide = to_wide(value_name);
    let mut hkey: HKEY = std::ptr::null_mut();

    let result = unsafe { RegOpenKeyExW(hroot, key_wide.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) };
    if result == NOT_FOUND {
        return Ok(());
    }
    if result != 0 {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}"),
            message: format!("RegOpenKeyExW failed: {result}"),
        });
    }

    let result = unsafe { RegDeleteValueW(hkey, name_wide.as_ptr()) };
    unsafe { RegCloseKey(hkey) };

    if result != 0 && result != NOT_FOUND {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}\\{value_name}"),
            message: format!("RegDeleteValueW failed: {result}"),
        });
    }
    Ok(())
}

pub fn set_string_value(
    root: &str,
    key: &str,
    value_name: &str,
    data: &str,
) -> InstallerResult<()> {
    let hroot = root_from_str(root)?;
    let key_wide = to_wide(key);
    let name_wide = to_wide(value_name);
    let data_wide = to_wide(data);
    let data_bytes: Vec<u8> = data_wide.iter().flat_map(|w| w.to_le_bytes()).collect();
    let mut hkey: HKEY = std::ptr::null_mut();

    let result = unsafe { RegOpenKeyExW(hroot, key_wide.as_ptr(), 0, KEY_SET_VALUE, &mut hkey) };
    if result != 0 {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}"),
            message: format!("RegOpenKeyExW failed: {result}"),
        });
    }

    let result = unsafe {
        RegSetValueExW(
            hkey,
            name_wide.as_ptr(),
            0,
            REG_SZ,
            data_bytes.as_ptr(),
            data_bytes.len() as u32,
        )
    };
    unsafe { RegCloseKey(hkey) };

    if result != 0 {
        return Err(InstallerError::Registry {
            key: format!("{root}\\{key}\\{value_name}"),
            message: format!("RegSetValueExW failed: {result}"),
        });
    }
    Ok(())
}

/// Open `key` under `root` for reading; `None` if it does not exist.
fn open_for_read(root: HKEY, key: &str) -> Option<HKEY> {
    let key_wide = to_wide(key);
    let mut hkey: HKEY = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(root, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    (result == 0).then_some(hkey)
}

/// The string value `value` of `key` (`HKLM\...`), or its default value
/// when `value` is empty; `None` if the key or value does not exist.
pub fn read_string(path: &str, value: &str) -> InstallerResult<Option<String>> {
    let (root, key) = split(path)?;
    let Some(hkey) = open_for_read(root_from_str(root)?, key) else {
        return Ok(None);
    };
    let data = read_existing_value(hkey, value);
    unsafe { RegCloseKey(hkey) };
    Ok(data)
}

/// Whether a key matching `pattern` (`HKLM\...`) exists, where a `*`
/// segment matches any one subkey.
pub fn key_pattern_exists(pattern: &str) -> InstallerResult<bool> {
    let (root, key) = split(pattern)?;
    let segments: Vec<&str> = key.split('\\').filter(|s| !s.is_empty()).collect();
    let root = root_from_str(root)?;
    // The root itself is never closed; every other handle is.
    Ok(matches(root, &segments, false))
}

fn split(path: &str) -> InstallerResult<(&'static str, &str)> {
    outto_core::config::split_registry_key(path).ok_or_else(|| InstallerError::Registry {
        key: path.to_string(),
        message: "not a key under HKLM, HKCU or HKCR".into(),
    })
}

fn matches(parent: HKEY, segments: &[&str], close: bool) -> bool {
    let found = match segments.split_first() {
        None => true,
        Some((&"*", rest)) => subkeys(parent).iter().any(|name| {
            open_for_read(parent, name).is_some_and(|child| matches(child, rest, true))
        }),
        Some((name, rest)) => {
            open_for_read(parent, name).is_some_and(|child| matches(child, rest, true))
        }
    };
    if close {
        unsafe { RegCloseKey(parent) };
    }
    found
}

fn subkeys(hkey: HKEY) -> Vec<String> {
    let mut names = Vec::new();
    let mut index = 0;
    loop {
        let mut name = [0u16; 256];
        let mut len = name.len() as u32;
        let result = unsafe {
            RegEnumKeyExW(
                hkey,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if result != 0 {
            break;
        }
        names.push(String::from_utf16_lossy(&name[..len as usize]));
        index += 1;
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_patterns_match_any_subkey_at_a_star() {
        let base = format!(r"Software\OuttoTest_key_pattern_{}", std::process::id());
        let _ = delete_tree("HKCU", &base);
        let pattern = format!(r"HKCU\{base}\LanguageProfile\*\*");
        assert!(!key_pattern_exists(&pattern).unwrap());

        create_key("HKCU", &format!(r"{base}\LanguageProfile\0x00000409"));
        assert!(
            !key_pattern_exists(&pattern).unwrap(),
            "an empty language matches"
        );
        assert!(key_pattern_exists(&format!(r"HKCU\{base}\LanguageProfile\*")).unwrap());

        create_key(
            "HKCU",
            &format!(r"{base}\LanguageProfile\0x00000409\{{2CC0C11A-EFB4-5241-A262-330A9475C109}}"),
        );
        assert!(key_pattern_exists(&pattern).unwrap());
        assert!(key_pattern_exists(&format!(r"hkcu\{base}\languageprofile\*\*")).unwrap());

        delete_tree("HKCU", &base).unwrap();
        assert!(!key_pattern_exists(&pattern).unwrap());
        assert!(key_pattern_exists(r"HKXX\Software").is_err());
    }

    #[test]
    fn read_string_reads_named_and_default_values() {
        let base = format!(r"Software\OuttoTest_read_string_{}", std::process::id());
        let _ = delete_tree("HKCU", &base);
        assert_eq!(read_string(&format!(r"HKCU\{base}"), "").unwrap(), None);
        create_key("HKCU", &base);
        set_string_value("HKCU", &base, "", r"C:\Program Files\X\1.2.3\x.dll").unwrap();
        set_string_value("HKCU", &base, "Named", "value").unwrap();
        assert_eq!(
            read_string(&format!(r"HKCU\{base}"), "")
                .unwrap()
                .as_deref(),
            Some(r"C:\Program Files\X\1.2.3\x.dll")
        );
        assert_eq!(
            read_string(&format!(r"HKCU\{base}"), "Named")
                .unwrap()
                .as_deref(),
            Some("value")
        );
        assert_eq!(
            read_string(&format!(r"HKCU\{base}"), "Missing").unwrap(),
            None
        );
        delete_tree("HKCU", &base).unwrap();
    }

    fn create_key(root: &str, key: &str) {
        let key_wide = to_wide(key);
        let mut hkey: HKEY = std::ptr::null_mut();
        let result = unsafe {
            RegCreateKeyExW(
                root_from_str(root).unwrap(),
                key_wide.as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            )
        };
        assert_eq!(result, 0);
        unsafe { RegCloseKey(hkey) };
    }

    fn delete_tree(root: &str, key: &str) -> InstallerResult<()> {
        let key_wide = to_wide(key);
        let result = unsafe { RegDeleteTreeW(root_from_str(root)?, key_wide.as_ptr()) };
        if result == 0 {
            Ok(())
        } else {
            Err(InstallerError::Other(format!("{result}")))
        }
    }

    #[test]
    fn test_hex_to_bytes_simple() {
        assert_eq!(hex_to_bytes("FF00AB").unwrap(), vec![255, 0, 171]);
    }

    #[test]
    fn test_hex_to_bytes_with_spaces() {
        assert_eq!(hex_to_bytes("FF 00 AB").unwrap(), vec![255, 0, 171]);
    }

    #[test]
    fn test_hex_to_bytes_empty() {
        let empty: Vec<u8> = vec![];
        assert_eq!(hex_to_bytes("").unwrap(), empty);
    }

    #[test]
    fn test_hex_to_bytes_odd_length() {
        assert!(hex_to_bytes("FFF").is_err());
    }

    #[test]
    fn test_parse_dword_valid() {
        assert_eq!(parse_dword("42").unwrap(), 42u32);
    }

    #[test]
    fn test_parse_dword_overflow() {
        assert!(parse_dword("4294967296").is_err());
    }

    #[test]
    fn test_parse_qword_valid() {
        assert_eq!(parse_qword("18446744073709551615").unwrap(), u64::MAX);
    }
}
