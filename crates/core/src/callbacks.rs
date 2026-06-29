use std::collections::HashSet;
use std::path::PathBuf;

use crate::error::{ErrorAction, InstallerError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub enum Prompt {
    OverwriteFile { path: PathBuf },
    ExistingInstallDetected { existing: ExistingInstall },
    /// One or more running applications are holding files we need to replace.
    /// `Yes` closes them (and they're relaunched afterwards); `No` defers the
    /// replacement to the next restart.
    CloseApplications { apps: Vec<String> },
}

#[derive(Debug, Clone)]
pub struct ExistingInstall {
    pub install_dir: PathBuf,
    pub version: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptResponse {
    Yes,
    No,
    YesToAll,
    Cancel,
}

pub trait InstallerCallbacks {
    fn on_progress(&self, phase: &str, current: u64, total: u64);
    fn on_prompt(&self, prompt: Prompt) -> PromptResponse;
    fn on_log(&self, level: LogLevel, message: &str);
    fn on_error(&self, error: &InstallerError) -> ErrorAction;

    /// Signalled once, near the end of a successful install, when the
    /// configured reboot policy has decided a system restart is wanted.
    /// Front-ends record this and decide whether/when to actually reboot
    /// (honouring `/NORESTART`, silent mode, etc.). Default: ignore.
    fn on_reboot_required(&self) {}
}

pub struct NoOpCallbacks;

impl InstallerCallbacks for NoOpCallbacks {
    fn on_progress(&self, _phase: &str, _current: u64, _total: u64) {}
    fn on_prompt(&self, _prompt: Prompt) -> PromptResponse {
        PromptResponse::Yes
    }
    fn on_log(&self, _level: LogLevel, _message: &str) {}
    fn on_error(&self, _error: &InstallerError) -> ErrorAction {
        ErrorAction::Abort
    }
}

pub struct InstallOptions {
    pub source_dir: PathBuf,
    pub install_dir: Option<PathBuf>,
    pub selected_components: Option<HashSet<String>>,
    pub uninstall_exe: Option<PathBuf>,
}
