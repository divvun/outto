use crate::config::{AssociationEntry, PathResolver};
use crate::error::{InstallerError, InstallerResult};
use crate::manifest::{ActionRecord, InstallManifest};
use crate::{InstallerCallbacks, LogLevel};

pub fn create_association(
    entry: &AssociationEntry,
    resolver: &PathResolver,
    manifest: &mut InstallManifest,
    callbacks: &dyn InstallerCallbacks,
) -> InstallerResult<()> {
    let command = resolver.resolve(&entry.command)?;
    let icon = entry
        .icon
        .as_deref()
        .map(|i| resolver.resolve(i))
        .transpose()?;

    callbacks.on_log(
        LogLevel::Info,
        &format!(
            "File association: {} -> {} ({})",
            entry.extension, entry.prog_id, command
        ),
    );

    #[cfg(windows)]
    {
        register_association_windows(
            &entry.extension,
            &entry.prog_id,
            entry.description.as_deref(),
            icon.as_deref(),
            &command,
        )?;
        notify_shell_change();
    }

    #[cfg(not(windows))]
    {
        callbacks.on_log(
            LogLevel::Info,
            &format!("  [simulated] Association {} registered", entry.extension),
        );
    }

    manifest.record(ActionRecord::AssociationCreated {
        extension: entry.extension.clone(),
        prog_id: entry.prog_id.clone(),
    });

    Ok(())
}

#[cfg(windows)]
fn to_wide(s: &str) -> Vec<u16> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn register_association_windows(
    extension: &str,
    prog_id: &str,
    description: Option<&str>,
    icon: Option<&str>,
    command: &str,
) -> InstallerResult<()> {
    set_hkcr_value(extension, "", prog_id)?;

    if let Some(desc) = description {
        set_hkcr_value(prog_id, "", desc)?;
    }

    if let Some(ico) = icon {
        set_hkcr_value(&format!("{prog_id}\\DefaultIcon"), "", ico)?;
    }

    set_hkcr_value(
        &format!("{prog_id}\\shell\\open\\command"),
        "",
        command,
    )?;

    Ok(())
}

#[cfg(windows)]
fn set_hkcr_value(key: &str, value_name: &str, data: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Registry::*;

    let key_wide = to_wide(key);
    let name_wide = to_wide(value_name);
    let data_wide = to_wide(data);
    let data_bytes: Vec<u8> = data_wide.iter().flat_map(|w| w.to_le_bytes()).collect();

    let mut hkey: HKEY = std::ptr::null_mut();
    let mut disposition: u32 = 0;

    let result = unsafe {
        RegCreateKeyExW(
            HKEY_CLASSES_ROOT,
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
        return Err(InstallerError::Association {
            extension: key.to_string(),
            message: format!("RegCreateKeyExW failed: {result}"),
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
        return Err(InstallerError::Association {
            extension: key.to_string(),
            message: format!("RegSetValueExW failed: {result}"),
        });
    }

    Ok(())
}

#[cfg(windows)]
fn notify_shell_change() {
    use windows_sys::Win32::UI::Shell::*;
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED as i32,
            SHCNF_IDLIST,
            std::ptr::null(),
            std::ptr::null(),
        );
    }
}

#[cfg(windows)]
pub fn remove_association(extension: &str, prog_id: &str) -> InstallerResult<()> {
    use windows_sys::Win32::System::Registry::*;

    let ext_wide = to_wide(extension);
    unsafe {
        RegDeleteKeyW(HKEY_CLASSES_ROOT, ext_wide.as_ptr());
    }

    for subkey in &[
        format!("{prog_id}\\shell\\open\\command"),
        format!("{prog_id}\\shell\\open"),
        format!("{prog_id}\\shell"),
        format!("{prog_id}\\DefaultIcon"),
        prog_id.to_string(),
    ] {
        let key_wide = to_wide(subkey);
        unsafe {
            RegDeleteKeyW(HKEY_CLASSES_ROOT, key_wide.as_ptr());
        }
    }

    notify_shell_change();
    Ok(())
}
