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

pub type Config = platform::Config;

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
    /// The user dismissed the macOS password prompt before anything ran;
    /// the GUI should return to the step it came from.
    ElevationCancelled,
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
    config: Config,
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
        let options = outto_core::InstallOptions {
            source_dir,
            install_dir,
            selected_components,
            uninstall_exe,
        };

        let result = platform::install(&config, &options, &callbacks);
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
        let result = platform::uninstall_package(&install_dir, &package_id, &callbacks);
        let mut q = queue.lock().unwrap();
        q.push_back(BridgeEvent::Finished(result.map_err(|e| e.to_string())));
    });
}

/// True if running the install in-process would trigger the osascript
/// self-relaunch (which would abandon this GUI). Mirrors the elevation
/// decision in `outto_macos::install()`: resolve the install dir the same way,
/// then apply `needs_elevation` + `auto_elevate`.
#[cfg(target_os = "macos")]
pub fn install_needs_elevation(config: &Config, install_dir: &Option<PathBuf>) -> bool {
    if !config.privileges.auto_elevate {
        return false;
    }
    let resolved = install_dir.clone().or_else(|| {
        config
            .package
            .default_dir
            .as_ref()
            .and_then(|d| platform::make_resolver(config, None).resolve_path(d).ok())
    });
    let Some(dir) = resolved else {
        return false;
    };
    platform::elevation::needs_elevation(
        &config.privileges.required,
        &dir,
        platform::elevation::DEFAULT_SYSTEM_ROOTS,
    )
}

#[cfg(not(target_os = "macos"))]
pub fn install_needs_elevation(_config: &Config, _install_dir: &Option<PathBuf>) -> bool {
    false
}

#[cfg(target_os = "macos")]
pub fn uninstall_needs_elevation(package_id: &str) -> bool {
    platform::uninstall::uninstall_needs_elevation(package_id)
}

#[cfg(not(target_os = "macos"))]
pub fn uninstall_needs_elevation(_package_id: &str) -> bool {
    false
}

/// Run the install in an elevated headless child (`/VERYSILENT` + a progress
/// file the child streams JSON events into), keeping this unprivileged
/// process alive as the UI. Events land in `queue` just like the in-process
/// path.
#[cfg(target_os = "macos")]
pub fn spawn_elevated_install(
    config_path: PathBuf,
    source_dir: PathBuf,
    install_dir: Option<PathBuf>,
    selected_components: Option<std::collections::HashSet<String>>,
    uninstall_exe: Option<PathBuf>,
    flags: crate::cli::CliFlags,
    queue: BridgeQueue,
) {
    use std::ffi::OsString;

    let mut argv: Vec<OsString> = vec![
        "install".into(),
        "--config".into(),
        config_path.into(),
        "--source".into(),
        source_dir.into(),
        "/VERYSILENT".into(),
        "/SUPPRESSMSGBOXES".into(),
    ];
    if let Some(dir) = install_dir {
        argv.push(format!("/DIR={}", dir.display()).into());
    }
    if let Some(comps) = selected_components {
        let mut list: Vec<String> = comps.into_iter().collect();
        list.sort();
        argv.push(format!("/COMPONENTS={}", list.join(",")).into());
    }
    if let Some(ua) = uninstall_exe {
        argv.push("--uninstall-app".into());
        argv.push(ua.into());
    }
    if flags.no_restart {
        argv.push("/NORESTART".into());
    }
    match &flags.log {
        Some(Some(path)) => argv.push(format!("/LOG={path}").into()),
        Some(None) => argv.push("/LOG".into()),
        None => {}
    }

    spawn_elevated_with_args(argv, queue);
}

/// Run the uninstall in an elevated headless child, same mechanism as
/// `spawn_elevated_install`.
#[cfg(target_os = "macos")]
pub fn spawn_elevated_uninstall(install_dir: PathBuf, queue: BridgeQueue) {
    use std::ffi::OsString;

    let argv: Vec<OsString> = vec![
        "uninstall".into(),
        "--dir".into(),
        install_dir.into(),
        "/VERYSILENT".into(),
        "/SUPPRESSMSGBOXES".into(),
    ];
    spawn_elevated_with_args(argv, queue);
}

#[cfg(target_os = "macos")]
fn spawn_elevated_with_args(mut argv: Vec<std::ffi::OsString>, queue: BridgeQueue) {
    use platform::elevation::{ElevatedOutcome, StreamEvent, run_elevated_with_progress};

    std::thread::spawn(move || {
        let mut run = || -> Result<ElevatedOutcome, String> {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let tmp = tempfile::Builder::new()
                .prefix("outto-elevated")
                .tempdir()
                .map_err(|e| e.to_string())?;
            let progress_path = tmp.path().join("progress.jsonl");
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
