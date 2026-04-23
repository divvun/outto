use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use outto::{ErrorAction, InstallerCallbacks, InstallerError, LogLevel, Prompt, PromptResponse};

/// Events sent from the install/uninstall thread to the GUI.
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
    Prompt {
        prompt: Prompt,
        response_tx: std::sync::mpsc::SyncSender<PromptResponse>,
    },
    Error {
        error_message: String,
        response_tx: std::sync::mpsc::SyncSender<ErrorAction>,
    },
    Finished(Result<(), String>),
}

/// Shared queue between the background thread and the GUI.
/// The background thread pushes events; the GUI polls and drains them.
pub type BridgeQueue = Arc<Mutex<VecDeque<BridgeEvent>>>;

/// Pending prompt waiting for user response in the GUI.
pub struct PendingPrompt {
    pub prompt: Prompt,
    pub response_tx: std::sync::mpsc::SyncSender<PromptResponse>,
}

/// Pending error waiting for user response in the GUI.
pub struct PendingError {
    pub error_message: String,
    pub response_tx: std::sync::mpsc::SyncSender<ErrorAction>,
}

/// InstallerCallbacks for GUI mode. Pushes events into the shared queue.
/// Also prints to stderr for console debugging.
struct GuiCallbacks {
    queue: BridgeQueue,
    suppress_prompts: bool,
}

impl InstallerCallbacks for GuiCallbacks {
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

    fn on_prompt(&self, prompt: Prompt) -> PromptResponse {
        if self.suppress_prompts {
            return PromptResponse::Yes;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        {
            let mut q = self.queue.lock().unwrap();
            q.push_back(BridgeEvent::Prompt {
                prompt,
                response_tx: tx,
            });
        }
        // Block the install thread until the GUI user responds
        rx.recv().unwrap_or(PromptResponse::Cancel)
    }

    fn on_error(&self, error: &InstallerError) -> ErrorAction {
        if self.suppress_prompts {
            eprintln!("[ERROR] {error}");
            return ErrorAction::Abort;
        }
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        {
            let mut q = self.queue.lock().unwrap();
            q.push_back(BridgeEvent::Error {
                error_message: error.to_string(),
                response_tx: tx,
            });
        }
        rx.recv().unwrap_or(ErrorAction::Abort)
    }
}

/// InstallerCallbacks for /VERYSILENT mode. No GUI, console only.
pub struct SilentCallbacks;

impl InstallerCallbacks for SilentCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64) {
        println!("[{current}/{total}] {phase}");
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

/// Spawn the install on a background thread.
pub fn spawn_install(
    config: outto::Config,
    source_dir: PathBuf,
    install_dir: Option<PathBuf>,
    selected_components: Option<std::collections::HashSet<String>>,
    suppress_prompts: bool,
    queue: BridgeQueue,
    uninstall_exe: Option<PathBuf>,
) {
    std::thread::spawn(move || {
        let callbacks = GuiCallbacks {
            queue: queue.clone(),
            suppress_prompts,
        };
        let options = outto::InstallOptions {
            source_dir,
            install_dir,
            selected_components,
            uninstall_exe,
        };

        let result = outto::install(&config, &options, &callbacks);
        let mut q = queue.lock().unwrap();
        q.push_back(BridgeEvent::Finished(result.map_err(|e| e.to_string())));
    });
}

/// Spawn the uninstall on a background thread.
pub fn spawn_uninstall(
    install_dir: PathBuf,
    package_id: String,
    suppress_prompts: bool,
    queue: BridgeQueue,
) {
    std::thread::spawn(move || {
        let callbacks = GuiCallbacks {
            queue: queue.clone(),
            suppress_prompts,
        };
        let result = outto::uninstall_package(&install_dir, &package_id, &callbacks);
        let mut q = queue.lock().unwrap();
        q.push_back(BridgeEvent::Finished(result.map_err(|e| e.to_string())));
    });
}
