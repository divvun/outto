use std::path::PathBuf;

use outto_core::config::Architecture;
use outto_core::error::InstallerResult;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW,
    RegQueryValueExW,
};

const UNINSTALL_KEY: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall";
const UNINSTALL_KEY_WOW64: &str =
    "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall";

/// Information about an existing installation found via Add/Remove Programs registry.
#[derive(Debug, Clone)]
pub struct ExistingInstall {
    pub install_dir: PathBuf,
    pub version: Option<String>,
    pub display_name: Option<String>,
}

/// An installation made by an Inno Setup installer, found via its
/// `<AppId>_is1` Add/Remove Programs entry.
#[derive(Debug, Clone)]
pub struct LegacyInnoInstall {
    /// Registry root of the entry, `"HKLM"` or `"HKCU"`.
    pub root: &'static str,
    /// Path of the entry under `root`.
    pub key: String,
    pub install_dir: PathBuf,
    pub version: Option<String>,
    pub display_name: Option<String>,
    /// Path to `unins000.exe`, parsed from `UninstallString`.
    pub uninstaller: Option<PathBuf>,
}

/// Detect an existing installation of the given package by its AppID.
pub fn detect_existing_install(package_id: &str) -> InstallerResult<Option<ExistingInstall>> {
    Ok(
        find_uninstall_entry(&[package_id.to_string()], |hkey| ExistingInstall {
            install_dir: read_string_value(hkey, "InstallLocation")
                .map(PathBuf::from)
                .unwrap_or_default(),
            version: read_string_value(hkey, "DisplayVersion"),
            display_name: read_string_value(hkey, "DisplayName"),
        })
        .map(|(_, _, install)| install),
    )
}

/// Detect an Inno Setup installation of the given AppId (a bare GUID). Inno
/// names its entry `<AppId>_is1`, and AppIds are conventionally written with
/// braces, so both `<GUID>_is1` and `{<GUID>}_is1` are checked.
pub fn detect_legacy_inno_install(guid: &str) -> Option<LegacyInnoInstall> {
    let subkeys = [format!("{{{guid}}}_is1"), format!("{guid}_is1")];
    find_uninstall_entry(&subkeys, |hkey| {
        let install_dir = read_string_value(hkey, "InstallLocation")
            .or_else(|| read_string_value(hkey, "Inno Setup: App Path"))
            .map(PathBuf::from)
            .unwrap_or_default();
        LegacyInnoInstall {
            root: "",
            key: String::new(),
            install_dir,
            version: read_string_value(hkey, "DisplayVersion"),
            display_name: read_string_value(hkey, "DisplayName"),
            uninstaller: read_string_value(hkey, "UninstallString")
                .and_then(|s| parse_uninstall_exe(&s)),
        }
    })
    .map(|(root, key, install)| LegacyInnoInstall {
        root,
        key,
        ..install
    })
}

/// Whether the registry entry a [`LegacyInnoInstall`] was found at still exists.
pub fn legacy_inno_entry_exists(install: &LegacyInnoInstall) -> bool {
    let root = if install.root == "HKCU" {
        HKEY_CURRENT_USER
    } else {
        HKEY_LOCAL_MACHINE
    };
    let key_wide = to_wide(&install.key);
    let mut hkey: HKEY = std::ptr::null_mut();
    let result = unsafe { RegOpenKeyExW(root, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
    if result == 0 {
        unsafe { RegCloseKey(hkey) };
    }
    result == 0
}

/// Extract the executable from an `UninstallString`, which Inno writes as a
/// quoted path (`"C:\...\unins000.exe"`), possibly followed by arguments.
fn parse_uninstall_exe(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    let exe = match s.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => s,
    };
    (!exe.is_empty()).then(|| PathBuf::from(exe))
}

/// Open the first of `subkeys` that exists under the HKLM (native, then
/// WOW6432Node) or HKCU Uninstall keys, and read it with `read`. Returns the
/// root name and path of the matching key alongside the result.
fn find_uninstall_entry<T>(
    subkeys: &[String],
    read: impl Fn(HKEY) -> T,
) -> Option<(&'static str, String, T)> {
    let roots: [(HKEY, &str, &str); 3] = [
        (HKEY_LOCAL_MACHINE, "HKLM", UNINSTALL_KEY),
        (HKEY_LOCAL_MACHINE, "HKLM", UNINSTALL_KEY_WOW64),
        (HKEY_CURRENT_USER, "HKCU", UNINSTALL_KEY),
    ];

    for (root, root_name, base) in roots {
        for subkey in subkeys {
            let key = format!("{base}\\{subkey}");
            let key_wide = to_wide(&key);
            let mut hkey: HKEY = std::ptr::null_mut();

            let result = unsafe { RegOpenKeyExW(root, key_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
            if result == 0 {
                let value = read(hkey);
                unsafe { RegCloseKey(hkey) };
                return Some((root_name, key, value));
            }
        }
    }

    None
}

fn to_wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn read_string_value(hkey: HKEY, name: &str) -> Option<String> {
    let name_wide = to_wide(name);
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

/// Check whether the given architecture matches the current system.
pub fn arch_matches(required: &Architecture) -> bool {
    match required {
        Architecture::Any => true,
        Architecture::X64 => crate::elevation::get_system_architecture() == "x64",
        Architecture::X86 => {
            let arch = crate::elevation::get_system_architecture();
            arch == "x86" || arch == "x64"
        }
    }
}

pub struct UninstallRegistryInfo<'a> {
    pub package_id: &'a str,
    pub display_name: &'a str,
    pub version: &'a str,
    pub publisher: Option<&'a str>,
    pub install_dir: &'a std::path::Path,
    pub display_icon: Option<&'a str>,
    pub url: Option<&'a str>,
    pub support_url: Option<&'a str>,
    pub uninstall_string: &'a str,
    pub depends_on: &'a [String],
}

/// Write uninstall registry entries for Add/Remove Programs.
pub fn write_uninstall_registry(info: &UninstallRegistryInfo<'_>) -> InstallerResult<()> {
    use outto_core::error::InstallerError;
    use windows_sys::Win32::System::Registry::*;

    fn set_str(hkey: HKEY, name: &str, data: &str) {
        let name_wide = super::detect::to_wide(name);
        let data_wide = super::detect::to_wide(data);
        let data_bytes: Vec<u8> = data_wide.iter().flat_map(|w| w.to_le_bytes()).collect();
        unsafe {
            RegSetValueExW(
                hkey,
                name_wide.as_ptr(),
                0,
                REG_SZ,
                data_bytes.as_ptr(),
                data_bytes.len() as u32,
            );
        }
    }

    let key = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{}",
        info.package_id
    );
    let key_wide = to_wide(&key);
    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition: u32 = 0;

    let result = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
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
        let result = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
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
                key,
                message: format!("failed to create uninstall key: {result}"),
            });
        }
    }

    set_str(hkey, "DisplayName", info.display_name);
    set_str(hkey, "DisplayVersion", info.version);
    set_str(hkey, "InstallLocation", &info.install_dir.to_string_lossy());

    if let Some(pub_name) = info.publisher {
        set_str(hkey, "Publisher", pub_name);
    }
    if let Some(icon) = info.display_icon {
        set_str(hkey, "DisplayIcon", icon);
    }
    if let Some(u) = info.url {
        set_str(hkey, "URLInfoAbout", u);
    }
    if let Some(su) = info.support_url {
        set_str(hkey, "HelpLink", su);
    }
    set_str(hkey, "UninstallString", info.uninstall_string);
    set_str(
        hkey,
        "QuietUninstallString",
        &format!("{} /VERYSILENT", info.uninstall_string),
    );

    let one: u32 = 1;
    let name_wide = to_wide("NoModify");
    unsafe {
        RegSetValueExW(
            hkey,
            name_wide.as_ptr(),
            0,
            REG_DWORD,
            &one as *const u32 as *const u8,
            4,
        );
    }
    let name_wide = to_wide("NoRepair");
    unsafe {
        RegSetValueExW(
            hkey,
            name_wide.as_ptr(),
            0,
            REG_DWORD,
            &one as *const u32 as *const u8,
            4,
        );
    }

    // Mark as outto-managed for enumeration during cascade uninstall
    set_str(hkey, "ManagedBy", "outto");

    // Write dependency list as REG_MULTI_SZ
    if !info.depends_on.is_empty() {
        let name_wide = to_wide("DependsOn");
        let mut multi_sz: Vec<u16> = Vec::new();
        for dep in info.depends_on {
            multi_sz.extend(dep.encode_utf16());
            multi_sz.push(0);
        }
        multi_sz.push(0);
        let data_bytes: Vec<u8> = multi_sz.iter().flat_map(|w| w.to_le_bytes()).collect();
        unsafe {
            RegSetValueExW(
                hkey,
                name_wide.as_ptr(),
                0,
                REG_MULTI_SZ,
                data_bytes.as_ptr(),
                data_bytes.len() as u32,
            );
        }
    }

    unsafe { RegCloseKey(hkey) };

    Ok(())
}

/// Remove uninstall registry entry.
pub fn remove_uninstall_registry(package_id: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Registry::*;

    let key = format!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{package_id}");
    let key_wide = to_wide(&key);

    unsafe {
        RegDeleteKeyW(HKEY_LOCAL_MACHINE, key_wide.as_ptr());
        RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr());
    }

    Ok(())
}

/// Info about an installed outto package, used for dependency resolution.
#[derive(Debug, Clone)]
pub struct InstalledPackageInfo {
    pub package_id: String,
    pub install_dir: PathBuf,
    pub depends_on: Vec<String>,
}

/// Enumerate all outto-managed packages from the Uninstall registry.
/// Returns packages that have `ManagedBy = "outto"`.
pub fn enumerate_outto_packages() -> Vec<InstalledPackageInfo> {
    enumerate_outto_packages_windows()
}

fn enumerate_outto_packages_windows() -> Vec<InstalledPackageInfo> {
    use windows_sys::Win32::System::Registry::*;

    fn read_multi_sz(hkey: HKEY, name: &str) -> Vec<String> {
        let name_wide = super::detect::to_wide(name);
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

        if result != 0 || data_size == 0 || data_type != REG_MULTI_SZ {
            return Vec::new();
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
            return Vec::new();
        }

        let wide: Vec<u16> = buffer
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();

        String::from_utf16_lossy(&wide)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect()
    }

    let mut packages = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let roots: &[(HKEY, &str)] = &[
        (
            HKEY_LOCAL_MACHINE,
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        ),
        (
            HKEY_CURRENT_USER,
            "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall",
        ),
    ];

    for &(root, path) in roots {
        let path_wide = to_wide(path);
        let mut hkey: HKEY = std::ptr::null_mut();

        let result = unsafe { RegOpenKeyExW(root, path_wide.as_ptr(), 0, KEY_READ, &mut hkey) };
        if result != 0 {
            continue;
        }

        let mut index: u32 = 0;
        loop {
            let mut name_buf = [0u16; 256];
            let mut name_len = name_buf.len() as u32;

            let result = unsafe {
                RegEnumKeyExW(
                    hkey,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };

            if result != 0 {
                break;
            }

            let subkey_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);

            let subkey_path = format!("{path}\\{subkey_name}");
            let subkey_wide = to_wide(&subkey_path);
            let mut sub_hkey: HKEY = std::ptr::null_mut();

            let result =
                unsafe { RegOpenKeyExW(root, subkey_wide.as_ptr(), 0, KEY_READ, &mut sub_hkey) };

            if result == 0 {
                if let Some(managed_by) = read_string_value(sub_hkey, "ManagedBy") {
                    if managed_by == "outto" && seen.insert(subkey_name.clone()) {
                        let install_dir = read_string_value(sub_hkey, "InstallLocation")
                            .map(PathBuf::from)
                            .unwrap_or_default();
                        let depends_on = read_multi_sz(sub_hkey, "DependsOn");

                        packages.push(InstalledPackageInfo {
                            package_id: subkey_name,
                            install_dir,
                            depends_on,
                        });
                    }
                }
                unsafe { RegCloseKey(sub_hkey) };
            }

            index += 1;
        }

        unsafe { RegCloseKey(hkey) };
    }

    packages
}
