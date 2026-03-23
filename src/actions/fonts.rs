use std::fs;
use std::path::{Path, PathBuf};

use crate::config::FontEntry;
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel};

pub fn install_font(
    entry: &FontEntry,
    source_dir: &Path,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let source_path = source_dir.join(&entry.source);
    let font_name = source_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let file_name = source_path
        .file_name()
        .ok_or_else(|| InstallerError::Font {
            file: entry.source.clone(),
            message: "no filename".into(),
        })?;

    callbacks.on_log(LogLevel::Info, &format!("Fonts: installing {}", font_name));

    #[cfg(windows)]
    {
        let fonts_dir = get_fonts_dir()?;
        let dest_path = fonts_dir.join(file_name);

        fs::copy(&source_path, &dest_path).map_err(|e| InstallerError::Font {
            file: entry.source.clone(),
            message: format!("failed to copy to fonts dir: {e}"),
        })?;

        register_font(&dest_path)?;
        add_font_registry_entry(&font_name, &file_name.to_string_lossy())?;

        manifest.record(ActionRecord::FontInstalled {
            file: dest_path,
            font_name: font_name.clone(),
        });
    }

    #[cfg(not(windows))]
    {
        callbacks.on_log(
            LogLevel::Info,
            &format!("Fonts: [simulated] installed {font_name}"),
        );
        manifest.record(ActionRecord::FontInstalled {
            file: source_path,
            font_name: font_name.clone(),
        });
    }

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
fn get_fonts_dir() -> InstallerResult<PathBuf> {
    let windir = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    Ok(PathBuf::from(windir).join("Fonts"))
}

#[cfg(windows)]
fn register_font(font_path: &Path) -> InstallerResult<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Graphics::Gdi::AddFontResourceW;

    let path_wide: Vec<u16> = OsStr::new(font_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let result = unsafe { AddFontResourceW(path_wide.as_ptr()) };
    if result == 0 {
        return Err(InstallerError::Font {
            file: font_path.to_string_lossy().into(),
            message: "AddFontResourceW returned 0".into(),
        });
    }

    broadcast_font_change();
    Ok(())
}

#[cfg(windows)]
fn add_font_registry_entry(font_name: &str, file_name: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Registry::*;

    let key_wide = to_wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts");
    let name_wide = to_wide(&format!("{font_name} (TrueType)"));
    let data_wide = to_wide(file_name);
    let data_bytes: Vec<u8> = data_wide.iter().flat_map(|w| w.to_le_bytes()).collect();

    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition: u32 = 0;

    let result = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            key_wide.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut hkey,
            &mut disposition,
        )
    };

    if result != 0 {
        return Err(InstallerError::Font {
            file: file_name.to_string(),
            message: format!("failed to open fonts registry key: {result}"),
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
        return Err(InstallerError::Font {
            file: file_name.to_string(),
            message: format!("failed to set font registry value: {result}"),
        });
    }

    Ok(())
}

#[cfg(windows)]
fn broadcast_font_change() {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_FONTCHANGE,
            0,
            0,
            SMTO_ABORTIFHUNG,
            5000,
            std::ptr::null_mut(),
        );
    }
}

#[cfg(windows)]
pub fn uninstall_font(font_path: &Path, font_name: &str) -> InstallerResult<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Graphics::Gdi::RemoveFontResourceW;
    use windows_sys::Win32::System::Registry::*;

    let path_wide: Vec<u16> = OsStr::new(font_path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        RemoveFontResourceW(path_wide.as_ptr());
    }

    let key_wide = to_wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts");
    let name_wide = to_wide(&format!("{font_name} (TrueType)"));
    let mut hkey: HKEY = std::ptr::null_mut();

    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            key_wide.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if result == 0 {
        unsafe {
            RegDeleteValueW(hkey, name_wide.as_ptr());
            RegCloseKey(hkey);
        }
    }

    let _ = fs::remove_file(font_path);
    broadcast_font_change();

    Ok(())
}
