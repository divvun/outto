//! The log every install and uninstall of a package appends to, whatever
//! its switches: `%ProgramData%\outto\<package id>.log`, or for a package
//! with `privileges = "user"`, `%LOCALAPPDATA%\outto\<package id>.log`. See
//! [`outto_core::logfile::start_package_log`] for what goes in it.
//!
//! An elevated installer appends to the all-users log, and in `%ProgramData%`
//! anyone may create a directory, even before outto first runs. So the
//! directory gets an owner and an ACL of its own, which let no one but
//! Administrators and SYSTEM write to it, and a directory that is a link
//! elsewhere, or a log file that is a link or that someone else owns, is
//! not written to.

use std::fs::OpenOptions;
use std::io;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};

use outto_core::config::Privileges;
use outto_core::logfile;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    IsWellKnownSid, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, WinBuiltinAdministratorsSid,
    WinLocalSystemSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, DELETE, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, GetFileInformationByHandle,
    READ_CONTROL, WRITE_DAC, WRITE_OWNER,
};

/// The all-users log directory's security: owned by Administrators, and a
/// DACL that inherits nothing from `%ProgramData%`, giving SYSTEM and
/// Administrators full control and Users read and execute, all of it
/// inherited by what is put in it.
pub const MACHINE_DIR_SDDL: &str = "O:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

/// Whose log a run writes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `%ProgramData%\outto`.
    Machine,
    /// `%LOCALAPPDATA%\outto`.
    User,
}

impl Scope {
    /// The current user's for a package with `privileges = "user"`, all
    /// users' otherwise.
    pub fn for_privileges(privileges: &Privileges) -> Self {
        match privileges {
            Privileges::User => Self::User,
            Privileges::Admin | Privileges::Auto => Self::Machine,
        }
    }

    pub fn dir(self) -> Option<PathBuf> {
        let variable = match self {
            Self::Machine => "ProgramData",
            Self::User => "LOCALAPPDATA",
        };
        std::env::var_os(variable)
            .filter(|v| !v.is_empty())
            .map(|v| PathBuf::from(v).join("outto"))
    }
}

/// Where an install of `package_id` logs. The directory is created, and for
/// [`Scope::Machine`] secured, first.
pub fn install_log(package_id: &str, scope: Scope) -> io::Result<PathBuf> {
    let dir = scope.dir().ok_or_else(|| no_dir(scope))?;
    prepare_in(&dir, package_id, scope)
}

/// The log of an uninstall of `package_id`: the one its installs wrote,
/// all users' or the current user's. When there is neither, all users'
/// for an elevated uninstaller and the current user's otherwise.
pub fn uninstall_log(package_id: &str) -> io::Result<PathBuf> {
    let name = logfile::package_log_file_name(package_id);
    let scope = [Scope::Machine, Scope::User]
        .into_iter()
        .find(|scope| scope.dir().is_some_and(|dir| dir.join(&name).is_file()))
        .unwrap_or(if crate::elevation::is_elevated() {
            Scope::Machine
        } else {
            Scope::User
        });
    install_log(package_id, scope)
}

/// Make `dir` ready to hold `package_id`'s log and return the log's path:
/// for [`Scope::Machine`], secured as [`MACHINE_DIR_SDDL`] says, without a
/// log file that is not outto's.
pub fn prepare_in(dir: &Path, package_id: &str, scope: Scope) -> io::Result<PathBuf> {
    let path = dir.join(logfile::package_log_file_name(package_id));
    match scope {
        Scope::User => std::fs::create_dir_all(dir)?,
        Scope::Machine => {
            secure_dir(dir)?;
            drop_foreign_file(&path)?;
        }
    }
    Ok(path)
}

/// Open `path` as this run's package log, beginning with `header`. A log
/// that can't be opened is a warning on stderr, not a reason to stop.
pub fn start(path: io::Result<PathBuf>, header: &str) {
    let result = path.and_then(|path| logfile::start_package_log(&path, header));
    if let Err(e) = result {
        eprintln!("Warning: cannot open the package log: {e}");
    }
}

fn no_dir(scope: Scope) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("no directory for the {scope:?} package log"),
    )
}

/// Create `dir` with [`MACHINE_DIR_SDDL`]'s security, or give an existing
/// one that security, unless it is a link to somewhere else.
fn secure_dir(dir: &Path) -> io::Result<()> {
    let descriptor = SecurityDescriptor::from_sddl(MACHINE_DIR_SDDL)?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    if unsafe { CreateDirectoryW(to_wide(dir).as_ptr(), &attributes) } != 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
        return Err(err);
    }

    // Anyone could have made it first. Through a handle to the directory
    // itself, never what a link in its place points to, check what it is,
    // then take it over.
    let handle = OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | WRITE_OWNER)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(dir)?;
    let metadata = handle.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::other(format!(
            "{} is not a plain directory",
            dir.display()
        )));
    }
    let (owner, dacl) = descriptor.owner_and_dacl()?;
    let result = unsafe {
        SetSecurityInfo(
            handle.as_raw_handle() as HANDLE,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    Ok(())
}

/// Delete the file at `path` if it is a link, has more than one name, or is
/// owned by neither Administrators nor SYSTEM: outto did not make it, and
/// an elevated installer must not append to it.
fn drop_foreign_file(path: &Path) -> io::Result<()> {
    let file = match OpenOptions::new()
        .access_mode(READ_CONTROL | DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let handle = file.as_raw_handle() as HANDLE;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(handle, &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let linked =
        info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 || info.nNumberOfLinks > 1;
    if linked || !owned_by_administrators_or_system(handle)? {
        drop(file);
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn owned_by_administrators_or_system(handle: HANDLE) -> io::Result<bool> {
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let result = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let descriptor = SecurityDescriptor(descriptor);
    let ours = unsafe {
        IsWellKnownSid(owner, WinBuiltinAdministratorsSid) != 0
            || IsWellKnownSid(owner, WinLocalSystemSid) != 0
    };
    drop(descriptor);
    Ok(ours)
}

/// A security descriptor the system allocated, freed when dropped.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(descriptor))
    }

    fn owner_and_dacl(&self) -> io::Result<(PSID, *mut ACL)> {
        let mut owner: PSID = std::ptr::null_mut();
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let (mut defaulted, mut present) = (0, 0);
        let ok = unsafe {
            GetSecurityDescriptorOwner(self.0, &mut owner, &mut defaulted) != 0
                && GetSecurityDescriptorDacl(self.0, &mut present, &mut dacl, &mut defaulted) != 0
        };
        if !ok {
            return Err(io::Error::last_os_error());
        }
        Ok((owner, dacl))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { LocalFree(self.0) };
        }
    }
}

fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes() {
        assert_eq!(Scope::for_privileges(&Privileges::User), Scope::User);
        assert_eq!(Scope::for_privileges(&Privileges::Admin), Scope::Machine);
        assert_eq!(Scope::for_privileges(&Privileges::Auto), Scope::Machine);
        assert!(
            Scope::Machine
                .dir()
                .unwrap()
                .ends_with(r"ProgramData\outto")
        );
        assert!(Scope::User.dir().unwrap().ends_with(r"AppData\Local\outto"));
    }

    #[test]
    fn a_user_log_directory_is_created() {
        let dir = std::env::temp_dir().join(format!("outto_test_user_log_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = prepare_in(&dir, "com.test.log", Scope::User).unwrap();
        assert_eq!(path, dir.join("com.test.log.log"));
        assert!(dir.is_dir());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
