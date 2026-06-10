use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use outto_core::{
    ErrorAction, InstallerCallbacks, InstallerError, LogLevel, Prompt, PromptResponse,
};

#[cfg(target_os = "macos")]
use outto_macos as platform;
#[cfg(windows)]
use outto_windows as platform;

pub enum BridgeEvent {
    Progress {
        phase: String,
        current: u64,
        total: u64,
    },
    Log {
        level: LogLevel,
        message: String,
    },
    Finished(Result<(), String>),
    /// The user dismissed the macOS password prompt before anything ran;
    /// the GUI should return to the confirm screen.
    ElevationCancelled,
}

pub type BridgeQueue = Arc<Mutex<VecDeque<BridgeEvent>>>;

struct UninstallCallbacks {
    queue: BridgeQueue,
}

impl InstallerCallbacks for UninstallCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64) {
        eprintln!("[{current}/{total}] {phase}");
        let mut q = self.queue.lock().unwrap();
        q.push_back(BridgeEvent::Progress {
            phase: phase.to_string(),
            current,
            total,
        });
    }

    fn on_log(&self, level: LogLevel, message: &str) {
        eprintln!("[{level:?}] {message}");
        let mut q = self.queue.lock().unwrap();
        q.push_back(BridgeEvent::Log {
            level,
            message: message.to_string(),
        });
    }

    fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
        PromptResponse::Yes
    }

    fn on_error(&self, error: &InstallerError) -> ErrorAction {
        eprintln!("[ERROR] {error}");
        ErrorAction::Abort
    }
}

pub struct SilentCallbacks;

impl InstallerCallbacks for SilentCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64) {
        println!("[{}/{total}] {phase}", current + 1);
    }

    fn on_log(&self, level: LogLevel, message: &str) {
        eprintln!("[{level:?}] {message}");
    }

    fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
        PromptResponse::Yes
    }

    fn on_error(&self, error: &InstallerError) -> ErrorAction {
        eprintln!("[ERROR] {error}");
        ErrorAction::Abort
    }
}

pub fn spawn_uninstall(install_dir: PathBuf, package_id: String, queue: BridgeQueue) {
    std::thread::spawn(move || {
        let callbacks = UninstallCallbacks {
            queue: queue.clone(),
        };
        let result = platform::uninstall_package(&install_dir, &package_id, &callbacks);
        let mut q = queue.lock().unwrap();
        q.push_back(BridgeEvent::Finished(result.map_err(|e| e.to_string())));
    });
}

/// True if uninstalling would need root this process doesn't have (the
/// receipt lives in the system base).
#[cfg(target_os = "macos")]
pub fn uninstall_needs_elevation(package_id: &str) -> bool {
    platform::uninstall::uninstall_needs_elevation(package_id)
}

#[cfg(not(target_os = "macos"))]
pub fn uninstall_needs_elevation(_package_id: &str) -> bool {
    false
}

/// Run the uninstall in an elevated headless child (`/VERYSILENT` + a
/// progress file the child streams JSON events into), keeping this
/// unprivileged process alive as the UI.
#[cfg(target_os = "macos")]
pub fn spawn_elevated_uninstall(install_dir: PathBuf, queue: BridgeQueue) {
    use std::ffi::OsString;

    use platform::elevation::{ElevatedOutcome, StreamEvent, run_elevated_with_progress};

    std::thread::spawn(move || {
        let run = || -> Result<ElevatedOutcome, String> {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let tmp = tempfile::Builder::new()
                .prefix("outto-elevated")
                .tempdir()
                .map_err(|e| e.to_string())?;
            let progress_path = tmp.path().join("progress.jsonl");

            let mut argv: Vec<OsString> = vec!["/VERYSILENT".into()];
            if !install_dir.as_os_str().is_empty() {
                argv.push("--dir".into());
                argv.push(install_dir.clone().into());
            }
            argv.push("--progress-file".into());
            argv.push(progress_path.clone().into());

            run_elevated_with_progress(&exe, &argv, &progress_path, |ev| {
                let mut q = queue.lock().unwrap();
                q.push_back(match ev {
                    StreamEvent::Progress {
                        phase,
                        current,
                        total,
                    } => BridgeEvent::Progress {
                        phase,
                        current,
                        total,
                    },
                    StreamEvent::Log { level, message } => BridgeEvent::Log { level, message },
                });
            })
            .map_err(|e| e.to_string())
        };

        let event = match run() {
            Ok(ElevatedOutcome::Completed(result)) => BridgeEvent::Finished(result),
            Ok(ElevatedOutcome::AuthCancelled) => BridgeEvent::ElevationCancelled,
            Err(e) => BridgeEvent::Finished(Err(e)),
        };
        let mut q = queue.lock().unwrap();
        q.push_back(event);
    });
}
