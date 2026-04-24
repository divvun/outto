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
