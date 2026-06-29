//! Restart Manager integration: when a file we need to replace is held open by
//! a running process, ask the user to close those apps, shut them down via the
//! Restart Manager, and relaunch them once the install finishes. Files that
//! can't be freed fall back to a replace-on-restart.
//!
//! The session spans the whole install: [`RestartManager`] is the
//! [`FileUnlocker`] passed into the file phase, and [`RestartManager::finish`]
//! relaunches the closed apps and ends the session afterwards.

use std::cell::Cell;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use outto_core::actions::files::FileUnlocker;
use outto_core::callbacks::{InstallerCallbacks, LogLevel, Prompt, PromptResponse};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmForceShutdown, RmGetList,
    RmRegisterResources, RmRestart, RmShutdown, RmStartSession,
};

/// Owns a Restart Manager session for the duration of an install.
#[derive(Default)]
pub struct RestartManager {
    session: Cell<Option<u32>>,
    shutdown_done: Cell<bool>,
}

impl RestartManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start the RM session lazily on first use; returns the handle, or `None`
    /// if the session couldn't be started (RM unavailable).
    fn ensure_session(&self) -> Option<u32> {
        if let Some(s) = self.session.get() {
            return Some(s);
        }
        let mut handle: u32 = 0;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        let rc = unsafe { RmStartSession(&mut handle, 0, key.as_mut_ptr()) };
        if rc == ERROR_SUCCESS {
            self.session.set(Some(handle));
            Some(handle)
        } else {
            None
        }
    }

    /// Relaunch the applications we shut down (best effort) and end the
    /// session. Safe to call when nothing was shut down or no session exists.
    pub fn finish(&self, callbacks: &dyn InstallerCallbacks) {
        if let Some(session) = self.session.get() {
            if self.shutdown_done.get() {
                callbacks.on_log(
                    LogLevel::Info,
                    "Restart Manager: relaunching previously closed applications",
                );
                unsafe {
                    RmRestart(session, 0, None);
                }
            }
            unsafe {
                RmEndSession(session);
            }
            self.session.set(None);
        }
    }
}

impl FileUnlocker for RestartManager {
    fn unlock(&self, dest: &Path, callbacks: &dyn InstallerCallbacks) -> bool {
        let Some(session) = self.ensure_session() else {
            return false;
        };

        // Register the locked file with the session.
        let dest_wide: Vec<u16> = OsStr::new(dest)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let files = [dest_wide.as_ptr()];
        let rc = unsafe {
            RmRegisterResources(
                session,
                files.len() as u32,
                files.as_ptr(),
                0,
                std::ptr::null(),
                0,
                std::ptr::null(),
            )
        };
        if rc != ERROR_SUCCESS {
            return false;
        }

        let holders = get_holders(session);
        if holders.is_empty() {
            // Nothing RM can see is holding it; let the caller defer to reboot.
            return false;
        }

        callbacks.on_log(
            LogLevel::Info,
            &format!(
                "Restart Manager: {} application(s) hold {}",
                holders.len(),
                dest.display()
            ),
        );

        let response = callbacks.on_prompt(Prompt::CloseApplications {
            apps: holders.clone(),
        });
        if !matches!(response, PromptResponse::Yes | PromptResponse::YesToAll) {
            return false;
        }

        let rc = unsafe { RmShutdown(session, RmForceShutdown as u32, None) };
        if rc != ERROR_SUCCESS {
            callbacks.on_log(
                LogLevel::Warn,
                &format!("Restart Manager: could not close applications (error {rc})"),
            );
            return false;
        }
        self.shutdown_done.set(true);
        true
    }
}

/// Query the applications/services currently holding the session's registered
/// files, returning their display names.
fn get_holders(session: u32) -> Vec<String> {
    // First call sizes the buffer (count = 0 → ERROR_MORE_DATA, needed set).
    let mut needed: u32 = 0;
    let mut count: u32 = 0;
    let mut reasons: u32 = 0;
    let rc = unsafe {
        RmGetList(
            session,
            &mut needed,
            &mut count,
            std::ptr::null_mut(),
            &mut reasons,
        )
    };
    // ERROR_SUCCESS with needed == 0 means no holders.
    if needed == 0 {
        let _ = rc;
        return Vec::new();
    }

    let mut infos: Vec<RM_PROCESS_INFO> = vec![RM_PROCESS_INFO::default(); needed as usize];
    count = needed;
    let rc = unsafe {
        RmGetList(
            session,
            &mut needed,
            &mut count,
            infos.as_mut_ptr(),
            &mut reasons,
        )
    };
    if rc != ERROR_SUCCESS {
        return Vec::new();
    }
    infos.truncate(count as usize);

    infos
        .iter()
        .map(|info| wide_to_string(&info.strAppName))
        .filter(|name| !name.is_empty())
        .collect()
}

/// Decode a fixed-size, NUL-terminated UTF-16 field (e.g. `strAppName`).
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
