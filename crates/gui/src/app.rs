use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iced::keyboard;
use iced::widget::{button, column, container, row, space, text};
use iced::{Element, Fill, Subscription, Task};

use outto_core::{ErrorAction, LogLevel, PromptResponse};

use crate::bridge::Config;

use crate::bridge::{self, BridgeEvent, BridgeQueue, PendingError, PendingPrompt};
use crate::cli::CliFlags;
use crate::screens;
use crate::theme;

/// Whether the background install/uninstall thread is running.
static BRIDGE_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

// --- Wizard step state machine ---

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WizardStep {
    Welcome,
    License,
    Directory,
    Components,
    Summary,
    Installing,
    Complete,
    UninstallConfirm,
    Uninstalling,
    UninstallComplete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppMode {
    Install,
    Uninstall,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FocusTarget {
    LicenseCheckbox,
    DirectoryInput,
    ComponentCheckbox(usize),
    Button(usize),
}

pub struct StepConfig {
    pub has_license: bool,
    pub has_directory: bool,
    pub has_components: bool,
}

impl WizardStep {
    pub fn next(self, cfg: &StepConfig) -> Option<WizardStep> {
        match self {
            Self::Welcome => {
                if cfg.has_license {
                    Some(Self::License)
                } else if cfg.has_directory {
                    Some(Self::Directory)
                } else if cfg.has_components {
                    Some(Self::Components)
                } else {
                    Some(Self::Summary)
                }
            }
            Self::License => {
                if cfg.has_directory {
                    Some(Self::Directory)
                } else if cfg.has_components {
                    Some(Self::Components)
                } else {
                    Some(Self::Summary)
                }
            }
            Self::Directory => {
                if cfg.has_components {
                    Some(Self::Components)
                } else {
                    Some(Self::Summary)
                }
            }
            Self::Components => Some(Self::Summary),
            Self::Summary => Some(Self::Installing),
            Self::UninstallConfirm => Some(Self::Uninstalling),
            _ => None,
        }
    }

    pub fn prev(self, cfg: &StepConfig) -> Option<WizardStep> {
        match self {
            Self::License => Some(Self::Welcome),
            Self::Directory => {
                if cfg.has_license {
                    Some(Self::License)
                } else {
                    Some(Self::Welcome)
                }
            }
            Self::Components => {
                if cfg.has_directory {
                    Some(Self::Directory)
                } else if cfg.has_license {
                    Some(Self::License)
                } else {
                    Some(Self::Welcome)
                }
            }
            Self::Summary => {
                if cfg.has_components {
                    Some(Self::Components)
                } else if cfg.has_directory {
                    Some(Self::Directory)
                } else if cfg.has_license {
                    Some(Self::License)
                } else {
                    Some(Self::Welcome)
                }
            }
            _ => None,
        }
    }
}

// --- Log line for display ---

pub struct LogLine {
    pub level: LogLevel,
    pub message: String,
}

// --- Progress state ---

pub struct ProgressState {
    pub phase: String,
    pub current: u64,
    pub total: u64,
    pub percent: f32,
    pub log_lines: Vec<LogLine>,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            phase: String::new(),
            current: 0,
            total: 0,
            percent: 0.0,
            log_lines: Vec::new(),
        }
    }
}

// --- Messages ---

#[derive(Debug, Clone)]
pub enum Message {
    // Navigation
    NextStep,
    PrevStep,
    Cancel,

    // License
    LicenseAccepted(bool),

    // Directory
    InstallDirChanged(String),
    BrowseDirectory,
    DirectoryPicked(Option<PathBuf>),

    // Components
    ComponentToggled(String, bool),

    // Actions
    StartInstall,
    StartUninstall,

    // Bridge
    BridgeUpdate,

    // Prompts
    PromptResponse(PromptResponse),
    ErrorResponse(ErrorAction),

    // Completion
    Finish,

    // Keyboard
    FocusNext,
    FocusPrev,
    ActivateFocused,

    // Native glass
    #[cfg(target_os = "macos")]
    InstallGlass(iced::window::Id),
    #[cfg(target_os = "macos")]
    NativeButtonsTick,
}

// --- App state ---

pub struct AppState {
    pub mode: AppMode,
    pub step: WizardStep,
    pub config: Config,
    pub config_path: PathBuf,
    pub flags: CliFlags,

    // User choices
    pub license_text: Option<String>,
    pub license_accepted: bool,
    pub install_dir: String,
    pub selected_components: HashMap<String, bool>,

    // Progress
    pub progress: ProgressState,
    pub result: Option<Result<(), String>>,

    // Prompt handling
    pub pending_prompt: Option<PendingPrompt>,
    pub pending_error: Option<PendingError>,

    // Bridge
    pub bridge_queue: BridgeQueue,

    // Source/uninstall dirs
    pub source_dir: PathBuf,
    pub uninstall_dir: Option<PathBuf>,
    pub uninstall_exe: Option<PathBuf>,

    pub step_config: StepConfig,

    // Keyboard focus
    pub focused_index: usize,

    // Minimum install time
    pub install_started_at: Option<Instant>,
    pub pending_finish: bool,

    /// True while an elevated child is doing the work — the unprivileged
    /// parent can't signal a root process, so Cancel would be a lie.
    pub cancel_locked: bool,
}

impl AppState {
    pub fn new(
        mode: AppMode,
        config: Config,
        config_path: PathBuf,
        flags: CliFlags,
        license_text: Option<String>,
        source_dir: PathBuf,
        uninstall_dir: Option<PathBuf>,
        default_install_dir: String,
        uninstall_exe: Option<PathBuf>,
    ) -> Self {
        let has_license = license_text.is_some();
        let has_directory = config.package.default_dir.is_none() && flags.dir.is_none();
        let has_components = !config.components.is_empty();
        let step_config = StepConfig {
            has_license,
            has_directory,
            has_components,
        };

        // Determine install dir: /DIR flag > config default > empty (user picks)
        let install_dir = if let Some(ref dir) = flags.dir {
            dir.clone()
        } else {
            default_install_dir
        };

        // Pre-populate component selection
        let mut selected_components = HashMap::new();
        if let Some(ref comp_list) = flags.components {
            for comp in &config.components {
                selected_components.insert(
                    comp.name.clone(),
                    comp.required || comp_list.contains(&comp.name),
                );
            }
        } else {
            for comp in &config.components {
                selected_components.insert(comp.name.clone(), comp.required || comp.default);
            }
        }

        // Determine starting step
        let start_step = match mode {
            AppMode::Install => {
                if flags.silent {
                    WizardStep::Installing
                } else if flags.sp_minus {
                    if has_license {
                        WizardStep::License
                    } else if has_directory {
                        WizardStep::Directory
                    } else if has_components {
                        WizardStep::Components
                    } else {
                        WizardStep::Summary
                    }
                } else {
                    WizardStep::Welcome
                }
            }
            AppMode::Uninstall => {
                if flags.silent {
                    WizardStep::Uninstalling
                } else {
                    WizardStep::UninstallConfirm
                }
            }
        };

        let mut state = Self {
            mode,
            step: start_step,
            config,
            config_path,
            flags,
            license_text,
            license_accepted: false,
            install_dir,
            selected_components,
            progress: ProgressState::default(),
            result: None,
            pending_prompt: None,
            pending_error: None,
            bridge_queue: Arc::new(Mutex::new(VecDeque::new())),
            source_dir,
            uninstall_dir,
            uninstall_exe,
            step_config,
            focused_index: 0,
            install_started_at: None,
            pending_finish: false,
            cancel_locked: false,
        };
        state.focused_index = default_focus_index(&state);
        state
    }

    pub fn cancel_disabled(&self) -> bool {
        self.flags.no_cancel || self.cancel_locked
    }

    fn start_install(&mut self) {
        self.install_started_at = Some(Instant::now());
        let selected: HashSet<String> = self
            .selected_components
            .iter()
            .filter(|&(_, &v)| v)
            .map(|(k, _)| k.clone())
            .collect();

        let install_dir = if self.install_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.install_dir))
        };

        BRIDGE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);

        // Installing into a root-owned location would make the in-process
        // pipeline relaunch the whole installer via osascript and abandon
        // this GUI. Instead, keep this process as the (unprivileged) UI and
        // hand the actual work to an elevated headless child.
        #[cfg(target_os = "macos")]
        if bridge::install_needs_elevation(&self.config, &install_dir) {
            self.cancel_locked = true;
            bridge::spawn_elevated_install(
                self.config_path.clone(),
                self.source_dir.clone(),
                install_dir,
                Some(selected),
                self.uninstall_exe.clone(),
                self.flags.clone(),
                self.bridge_queue.clone(),
            );
            return;
        }

        bridge::spawn_install(
            self.config.clone(),
            self.source_dir.clone(),
            install_dir,
            Some(selected),
            self.flags.suppress_msgboxes,
            self.bridge_queue.clone(),
            self.uninstall_exe.clone(),
        );
    }

    fn start_uninstall(&mut self) {
        let dir = self
            .uninstall_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(&self.install_dir));
        let package_id = self.config.package.id.clone();

        BRIDGE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);

        #[cfg(target_os = "macos")]
        if bridge::uninstall_needs_elevation(&package_id) {
            self.cancel_locked = true;
            bridge::spawn_elevated_uninstall(dir, self.bridge_queue.clone());
            return;
        }

        bridge::spawn_uninstall(
            dir,
            package_id,
            self.flags.suppress_msgboxes,
            self.bridge_queue.clone(),
        );
    }

    fn drain_bridge_queue(&mut self) {
        let events: Vec<BridgeEvent> = {
            let mut queue = self.bridge_queue.lock().unwrap();
            queue.drain(..).collect()
        };

        for event in events {
            match event {
                BridgeEvent::Progress {
                    phase,
                    current,
                    total,
                } => {
                    self.progress.phase = phase;
                    self.progress.current = current;
                    self.progress.total = total;
                    self.progress.percent = if total > 0 {
                        (current as f32 / total as f32) * 100.0
                    } else {
                        0.0
                    };
                }
                BridgeEvent::Log { level, message } => {
                    self.progress.log_lines.push(LogLine {
                        level,
                        message: theme::normalize_path(&message),
                    });
                }
                BridgeEvent::Prompt {
                    prompt,
                    response_tx,
                } => {
                    self.pending_prompt = Some(PendingPrompt {
                        prompt,
                        response_tx,
                    });
                }
                BridgeEvent::Error {
                    error_message,
                    response_tx,
                } => {
                    self.pending_error = Some(PendingError {
                        error_message,
                        response_tx,
                    });
                }
                BridgeEvent::ElevationCancelled => {
                    // The user dismissed the password prompt; nothing ran.
                    // Put them back where they were so they can retry.
                    self.cancel_locked = false;
                    self.install_started_at = None;
                    self.pending_finish = false;
                    self.progress = ProgressState::default();
                    self.step = match self.mode {
                        AppMode::Install => WizardStep::Summary,
                        AppMode::Uninstall => WizardStep::UninstallConfirm,
                    };
                    self.focused_index = default_focus_index(self);
                }
                BridgeEvent::Finished(result) => {
                    self.cancel_locked = false;
                    self.result = Some(result);
                    // Check minimum display time (1 second)
                    let elapsed = self
                        .install_started_at
                        .map(|t| t.elapsed().as_millis() >= 1000)
                        .unwrap_or(true);
                    if elapsed {
                        match self.step {
                            WizardStep::Installing => self.step = WizardStep::Complete,
                            WizardStep::Uninstalling => self.step = WizardStep::UninstallComplete,
                            _ => {}
                        }
                        self.focused_index = default_focus_index(self);
                    } else {
                        self.pending_finish = true;
                    }
                }
            }
        }
    }
}

// --- Iced Application ---

pub fn run(state: AppState) -> iced::Result {
    let auto_start = state.step == WizardStep::Installing || state.step == WizardStep::Uninstalling;
    let auto_mode = state.mode;

    // iced 0.14 boot must be Fn (not FnOnce). Use a Mutex to allow moving state out.
    let state_cell = Mutex::new(Some((state, auto_start, auto_mode)));

    let mut app = iced::application(
        move || {
            let (state, auto_start, auto_mode) = state_cell
                .lock()
                .unwrap()
                .take()
                .expect("boot called more than once");
            let task = if auto_start {
                match auto_mode {
                    AppMode::Install => Task::done(Message::StartInstall),
                    AppMode::Uninstall => Task::done(Message::StartUninstall),
                }
            } else {
                Task::none()
            };
            (state, task)
        },
        update,
        view,
    )
    .subscription(subscription)
    .title(|state: &AppState| format!("{} Setup", state.config.package.name))
    .theme(|_state: &AppState| theme::make_theme())
    .default_font(theme::default_font())
    .window_size(iced::Size::new(theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT))
    .resizable(false);

    // On macOS we paint onto an NSVisualEffectView installed behind the iced
    // surface (see `glass.rs`); the iced canvas itself needs to be transparent
    // so the OS material actually shows through. Windows keeps its opaque
    // canvas.
    #[cfg(target_os = "macos")]
    {
        app = app.transparent(true);
    }

    if let Some(bytes) = theme::default_font_bytes() {
        app = app.font(bytes);
    }

    app.run()
}

fn update(state: &mut AppState, message: Message) -> Task<Message> {
    let task = update_inner(state, message);
    #[cfg(target_os = "macos")]
    crate::native_buttons::apply(compute_button_layout(state));
    task
}

fn update_inner(state: &mut AppState, message: Message) -> Task<Message> {
    match message {
        Message::NextStep => {
            if let Some(next) = state.step.next(&state.step_config) {
                state.step = next;
                state.focused_index = default_focus_index(state);
            }
            Task::none()
        }
        Message::PrevStep => {
            if let Some(prev) = state.step.prev(&state.step_config) {
                state.step = prev;
                state.focused_index = default_focus_index(state);
            }
            Task::none()
        }
        Message::Cancel => {
            // While an elevated child runs there is nothing we can do to stop
            // it (it's root, we're not), so refuse to silently walk away.
            if state.cancel_locked {
                return Task::none();
            }
            std::process::exit(0);
        }
        Message::LicenseAccepted(accepted) => {
            state.license_accepted = accepted;
            // Accepting enables Continue — move focus there so Enter proceeds.
            state.focused_index = default_focus_index(state);
            Task::none()
        }
        Message::InstallDirChanged(dir) => {
            state.install_dir = dir;
            state.focused_index = default_focus_index(state);
            Task::none()
        }
        Message::BrowseDirectory => Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .set_title("Select Installation Directory")
                    .pick_folder()
                    .await
                    .map(|h| h.path().to_path_buf())
            },
            Message::DirectoryPicked,
        ),
        Message::DirectoryPicked(path) => {
            if let Some(p) = path {
                state.install_dir = p.to_string_lossy().into_owned();
                state.focused_index = default_focus_index(state);
            }
            Task::none()
        }
        Message::ComponentToggled(name, checked) => {
            state.selected_components.insert(name, checked);
            Task::none()
        }
        Message::StartInstall => {
            state.step = WizardStep::Installing;
            state.start_install();
            state.focused_index = default_focus_index(state);
            Task::none()
        }
        Message::StartUninstall => {
            state.step = WizardStep::Uninstalling;
            state.start_uninstall();
            state.focused_index = default_focus_index(state);
            Task::none()
        }
        Message::BridgeUpdate => {
            state.drain_bridge_queue();
            // Check if pending finish should now advance (minimum 1s elapsed)
            if state.pending_finish {
                let elapsed = state
                    .install_started_at
                    .map(|t| t.elapsed().as_millis() >= 1000)
                    .unwrap_or(true);
                if elapsed {
                    state.pending_finish = false;
                    match state.step {
                        WizardStep::Installing => state.step = WizardStep::Complete,
                        WizardStep::Uninstalling => state.step = WizardStep::UninstallComplete,
                        _ => {}
                    }
                    state.focused_index = default_focus_index(state);
                }
            }
            Task::none()
        }
        Message::PromptResponse(response) => {
            if let Some(pending) = state.pending_prompt.take() {
                let _ = pending.response_tx.send(response);
            }
            Task::none()
        }
        Message::ErrorResponse(action) => {
            if let Some(pending) = state.pending_error.take() {
                let _ = pending.response_tx.send(action);
            }
            Task::none()
        }
        Message::Finish => {
            std::process::exit(if state.result.as_ref().is_some_and(|r| r.is_ok()) {
                0
            } else {
                1
            });
        }
        Message::FocusNext => {
            let count = focusable_items(state).len();
            if count > 0 {
                state.focused_index = (state.focused_index + 1) % count;
            }
            Task::none()
        }
        Message::FocusPrev => {
            let count = focusable_items(state).len();
            if count > 0 {
                state.focused_index = if state.focused_index == 0 {
                    count - 1
                } else {
                    state.focused_index - 1
                };
            }
            Task::none()
        }
        Message::ActivateFocused => activate_focused(state),

        #[cfg(target_os = "macos")]
        Message::InstallGlass(id) => {
            use iced::window::raw_window_handle::HasWindowHandle;
            let task = iced::window::run(id, |window| {
                if let Ok(handle) = window.window_handle() {
                    let _ = crate::glass::install(&handle);
                    let _ = crate::native_buttons::install(&handle);
                }
            })
            .discard();
            crate::native_buttons::apply(compute_button_layout(state));
            task
        }
        #[cfg(target_os = "macos")]
        Message::NativeButtonsTick => {
            // Reapply the traffic-light nudge on every tick — AppKit's own
            // layout passes can reset their frames, so we force them back.
            crate::glass::tick_nudge_traffic_lights();

            let mut tasks: Vec<Task<Message>> = Vec::new();
            for action in crate::native_buttons::drain_clicks() {
                if let Some(msg) = map_native_button(state, action) {
                    tasks.push(Task::done(msg));
                }
            }
            if tasks.is_empty() {
                Task::none()
            } else {
                Task::batch(tasks)
            }
        }
    }
}

fn focusable_items(state: &AppState) -> Vec<FocusTarget> {
    let mut items = vec![];
    let no_cancel = state.cancel_disabled();

    // Content widgets first
    match state.step {
        WizardStep::License => {
            items.push(FocusTarget::LicenseCheckbox);
        }
        WizardStep::Directory => {
            items.push(FocusTarget::DirectoryInput);
        }
        WizardStep::Components => {
            for (i, comp) in state.config.components.iter().enumerate() {
                if !comp.required {
                    items.push(FocusTarget::ComponentCheckbox(i));
                }
            }
        }
        _ => {}
    }

    // Button bar (only enabled buttons)
    match state.step {
        WizardStep::Welcome => {
            items.push(FocusTarget::Button(0)); // Next
            if !no_cancel {
                items.push(FocusTarget::Button(1));
            }
        }
        WizardStep::License => {
            items.push(FocusTarget::Button(0)); // Back
            if state.license_accepted {
                items.push(FocusTarget::Button(1));
            } // Next
            if !no_cancel {
                items.push(FocusTarget::Button(2));
            }
        }
        WizardStep::Directory => {
            items.push(FocusTarget::Button(0)); // Back
            if !state.install_dir.is_empty() {
                items.push(FocusTarget::Button(1));
            } // Next
            if !no_cancel {
                items.push(FocusTarget::Button(2));
            }
        }
        WizardStep::Components => {
            items.push(FocusTarget::Button(0)); // Back
            items.push(FocusTarget::Button(1)); // Next
            if !no_cancel {
                items.push(FocusTarget::Button(2));
            }
        }
        WizardStep::Summary => {
            items.push(FocusTarget::Button(0)); // Back
            items.push(FocusTarget::Button(1)); // Install
            if !no_cancel {
                items.push(FocusTarget::Button(2));
            }
        }
        WizardStep::Installing | WizardStep::Uninstalling => {
            if !no_cancel {
                items.push(FocusTarget::Button(0));
            }
        }
        WizardStep::Complete | WizardStep::UninstallComplete => {
            items.push(FocusTarget::Button(0)); // Finish
        }
        WizardStep::UninstallConfirm => {
            items.push(FocusTarget::Button(0)); // Uninstall
            if !no_cancel {
                items.push(FocusTarget::Button(1));
            }
        }
    }

    items
}

pub fn current_focus_target(state: &AppState) -> Option<FocusTarget> {
    let items = focusable_items(state);
    items.get(state.focused_index).copied()
}

/// Which `FocusTarget::Button(i)` is the step's primary action, if it's
/// currently enabled. Indices mirror `focusable_items`/`activate_button`.
fn primary_button_index(state: &AppState) -> Option<usize> {
    match state.step {
        WizardStep::Welcome => Some(0),
        WizardStep::License => state.license_accepted.then_some(1),
        WizardStep::Directory => (!state.install_dir.is_empty()).then_some(1),
        WizardStep::Components => Some(1),
        WizardStep::Summary => Some(1),
        WizardStep::Complete | WizardStep::UninstallComplete => Some(0),
        WizardStep::UninstallConfirm => Some(0),
        // Enter must not implicitly hit Cancel mid-operation.
        WizardStep::Installing | WizardStep::Uninstalling => None,
    }
}

/// Default keyboard focus for the current step: the primary button, so Enter
/// confirms rather than going back; falls back to the first focusable item
/// (e.g. the license checkbox while the license is unaccepted).
fn default_focus_index(state: &AppState) -> usize {
    let items = focusable_items(state);
    primary_button_index(state)
        .and_then(|pb| items.iter().position(|t| *t == FocusTarget::Button(pb)))
        .unwrap_or(0)
}

fn activate_focused(state: &mut AppState) -> Task<Message> {
    let Some(target) = current_focus_target(state) else {
        return Task::none();
    };

    match target {
        FocusTarget::LicenseCheckbox => {
            state.license_accepted = !state.license_accepted;
            // Accepting enables Continue — move focus there so the next
            // Enter proceeds. Unaccepting falls back to the checkbox.
            state.focused_index = default_focus_index(state);
        }
        FocusTarget::ComponentCheckbox(i) => {
            if let Some(comp) = state.config.components.get(i) {
                let name = comp.name.clone();
                let current = state
                    .selected_components
                    .get(&name)
                    .copied()
                    .unwrap_or(false);
                state.selected_components.insert(name, !current);
            }
        }
        FocusTarget::DirectoryInput => {
            // Text input handles its own keyboard — no-op
        }
        FocusTarget::Button(idx) => {
            return activate_button(state, idx);
        }
    }
    Task::none()
}

fn activate_button(state: &mut AppState, button_idx: usize) -> Task<Message> {
    match state.step {
        WizardStep::Welcome => match button_idx {
            0 => {
                if let Some(next) = state.step.next(&state.step_config) {
                    state.step = next;
                    state.focused_index = default_focus_index(state);
                }
            }
            _ => std::process::exit(0),
        },
        WizardStep::License => match button_idx {
            0 => {
                if let Some(prev) = state.step.prev(&state.step_config) {
                    state.step = prev;
                    state.focused_index = default_focus_index(state);
                }
            }
            1 => {
                if state.license_accepted {
                    if let Some(next) = state.step.next(&state.step_config) {
                        state.step = next;
                        state.focused_index = default_focus_index(state);
                    }
                }
            }
            _ => std::process::exit(0),
        },
        WizardStep::Directory => match button_idx {
            0 => {
                if let Some(prev) = state.step.prev(&state.step_config) {
                    state.step = prev;
                    state.focused_index = default_focus_index(state);
                }
            }
            1 => {
                if !state.install_dir.is_empty() {
                    if let Some(next) = state.step.next(&state.step_config) {
                        state.step = next;
                        state.focused_index = default_focus_index(state);
                    }
                }
            }
            _ => std::process::exit(0),
        },
        WizardStep::Components => match button_idx {
            0 => {
                if let Some(prev) = state.step.prev(&state.step_config) {
                    state.step = prev;
                    state.focused_index = default_focus_index(state);
                }
            }
            1 => {
                if let Some(next) = state.step.next(&state.step_config) {
                    state.step = next;
                    state.focused_index = default_focus_index(state);
                }
            }
            _ => std::process::exit(0),
        },
        WizardStep::Summary => match button_idx {
            0 => {
                if let Some(prev) = state.step.prev(&state.step_config) {
                    state.step = prev;
                    state.focused_index = default_focus_index(state);
                }
            }
            1 => {
                state.step = WizardStep::Installing;
                state.start_install();
                state.focused_index = default_focus_index(state);
            }
            _ => std::process::exit(0),
        },
        WizardStep::Complete | WizardStep::UninstallComplete => {
            std::process::exit(if state.result.as_ref().is_some_and(|r| r.is_ok()) {
                0
            } else {
                1
            });
        }
        WizardStep::UninstallConfirm => match button_idx {
            0 => {
                state.step = WizardStep::Uninstalling;
                state.start_uninstall();
                state.focused_index = default_focus_index(state);
            }
            _ => std::process::exit(0),
        },
        _ => {}
    }
    Task::none()
}

fn view(state: &AppState) -> Element<'_, Message> {
    let content: Element<Message> = match state.step {
        WizardStep::Welcome => screens::welcome::view(state),
        WizardStep::License => screens::license::view(state),
        WizardStep::Directory => screens::directory::view(state),
        WizardStep::Components => screens::components::view(state),
        WizardStep::Summary => screens::summary::view(state),
        WizardStep::Installing => screens::progress::view(state),
        WizardStep::Complete => screens::complete::view(state),
        WizardStep::UninstallConfirm => screens::uninstall_confirm::view(state),
        WizardStep::Uninstalling => screens::uninstall_progress::view(state),
        WizardStep::UninstallComplete => screens::uninstall_complete::view(state),
    };

    let button_bar = view_button_bar(state);

    build_shell(state, content, button_bar)
}

#[cfg(not(target_os = "macos"))]
fn build_shell<'a>(
    state: &'a AppState,
    content: Element<'a, Message>,
    button_bar: Element<'a, Message>,
) -> Element<'a, Message> {
    let header = container(
        text(format!("{} Setup", state.config.package.name))
            .size(theme::FONT_HEADER)
            .color(theme::HEADER_TEXT),
    )
    .width(Fill)
    .height(theme::HEADER_HEIGHT)
    .padding([0.0, theme::PADDING])
    .center_y(theme::HEADER_HEIGHT)
    .style(theme::header_style);
    column![header, content, button_bar].into()
}

#[cfg(target_os = "macos")]
fn build_shell<'a>(
    state: &'a AppState,
    content: Element<'a, Message>,
    button_bar: Element<'a, Message>,
) -> Element<'a, Message> {
    use crate::layout;
    // macOS layout: a rounded sidebar inset panel anchored to the top-left
    // (subsumes the traffic-light area) and a content region on the right
    // that paints directly onto the outer window glass — no inset panel.
    //
    // iced positions:
    //   - Sidebar column reserves SIDEBAR_WIDTH + PANEL_MARGIN so its
    //     contents float above the NSVisualEffectView panel rect.
    //   - Content column gets its own internal padding instead of relying
    //     on a wrapping container's padding (since there's no card to
    //     align with on the right side).
    let sidebar_column = container(crate::sidebar::view(state))
        .width(layout::SIDEBAR_WIDTH + layout::PANEL_MARGIN)
        .height(Fill);
    let content_pane = container(content)
        .width(Fill)
        .height(Fill)
        .padding([layout::CONTENT_INSET_Y, layout::CONTENT_INSET_X])
        .style(theme::content_pane_style);
    let content_column = column![content_pane, button_bar].width(Fill).height(Fill);

    row![sidebar_column, content_column].into()
}

#[cfg(target_os = "macos")]
const NAV_BUTTON_WIDTH: f32 = 108.0;
#[cfg(target_os = "macos")]
const NAV_BUTTON_PADDING: [f32; 2] = [10.0, 22.0];
#[cfg(not(target_os = "macos"))]
const NAV_BUTTON_WIDTH: f32 = 100.0;
#[cfg(not(target_os = "macos"))]
const NAV_BUTTON_PADDING: [f32; 2] = [8.0, 16.0];

fn nav_label_font() -> iced::Font {
    // Semibold reads more like a Tahoe capsule button label than Regular.
    #[cfg(target_os = "macos")]
    {
        theme::semibold_font()
    }
    #[cfg(not(target_os = "macos"))]
    {
        theme::default_font()
    }
}

fn primary_nav_button(label: &str, focused: bool) -> iced::widget::Button<'_, Message> {
    let style = if focused {
        theme::primary_button_focused as fn(&_, _) -> _
    } else {
        theme::primary_button as fn(&_, _) -> _
    };
    let text_color = {
        #[cfg(target_os = "macos")]
        {
            Some(iced::Color::WHITE)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None::<iced::Color>
        }
    };
    let label_widget = if let Some(c) = text_color {
        text(label)
            .size(theme::FONT_SECONDARY)
            .font(nav_label_font())
            .center()
            .color(c)
    } else {
        text(label)
            .size(theme::FONT_SECONDARY)
            .font(nav_label_font())
            .center()
    };
    button(label_widget)
        .width(NAV_BUTTON_WIDTH)
        .padding(NAV_BUTTON_PADDING)
        .style(style)
}

fn secondary_nav_button(label: &str, focused: bool) -> iced::widget::Button<'_, Message> {
    let style = if focused {
        theme::secondary_button_focused as fn(&_, _) -> _
    } else {
        theme::secondary_button as fn(&_, _) -> _
    };
    button(
        text(label)
            .size(theme::FONT_SECONDARY)
            .font(nav_label_font())
            .center(),
    )
    .width(NAV_BUTTON_WIDTH)
    .padding(NAV_BUTTON_PADDING)
    .style(style)
}

#[cfg(target_os = "macos")]
fn view_button_bar(_state: &AppState) -> Element<'_, Message> {
    // Buttons are real NSButtons installed via `native_buttons::install`, so
    // iced just reserves the 50 px strip at the bottom that AppKit draws
    // into. Keeping the container empty avoids any iced-rendered widgets
    // fighting with the overlaid NSButtons.
    container(space::vertical()).width(Fill).height(50).into()
}

#[cfg(not(target_os = "macos"))]
fn view_button_bar(state: &AppState) -> Element<'_, Message> {
    let mut bar = row![].spacing(10).padding([12.0, theme::PADDING]);
    bar = bar.push(space::horizontal());

    let focus = current_focus_target(state);
    let bf = |idx: usize| focus == Some(FocusTarget::Button(idx));
    let next = theme::next_label();
    let back = theme::back_label();

    match state.step {
        WizardStep::Welcome => {
            bar = bar.push(primary_nav_button(next, bf(0)).on_press(Message::NextStep));
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(1)).on_press(Message::Cancel));
            }
        }
        WizardStep::License => {
            bar = bar.push(secondary_nav_button(back, bf(0)).on_press(Message::PrevStep));
            if state.license_accepted {
                bar = bar.push(primary_nav_button(next, bf(1)).on_press(Message::NextStep));
            } else {
                bar = bar.push(primary_nav_button(next, false));
            }
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(2)).on_press(Message::Cancel));
            }
        }
        WizardStep::Directory => {
            bar = bar.push(secondary_nav_button(back, bf(0)).on_press(Message::PrevStep));
            if !state.install_dir.is_empty() {
                bar = bar.push(primary_nav_button(next, bf(1)).on_press(Message::NextStep));
            } else {
                bar = bar.push(primary_nav_button(next, false));
            }
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(2)).on_press(Message::Cancel));
            }
        }
        WizardStep::Components => {
            bar = bar.push(secondary_nav_button(back, bf(0)).on_press(Message::PrevStep));
            bar = bar.push(primary_nav_button(next, bf(1)).on_press(Message::NextStep));
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(2)).on_press(Message::Cancel));
            }
        }
        WizardStep::Summary => {
            bar = bar.push(secondary_nav_button(back, bf(0)).on_press(Message::PrevStep));
            bar = bar.push(primary_nav_button("Install", bf(1)).on_press(Message::StartInstall));
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(2)).on_press(Message::Cancel));
            }
        }
        WizardStep::Installing | WizardStep::Uninstalling => {
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(0)).on_press(Message::Cancel));
            }
        }
        WizardStep::Complete | WizardStep::UninstallComplete => {
            bar = bar.push(primary_nav_button("Finish", bf(0)).on_press(Message::Finish));
        }
        WizardStep::UninstallConfirm => {
            bar =
                bar.push(primary_nav_button("Uninstall", bf(0)).on_press(Message::StartUninstall));
            if !state.cancel_disabled() {
                bar = bar.push(secondary_nav_button("Cancel", bf(1)).on_press(Message::Cancel));
            }
        }
    }

    container(bar).width(Fill).height(50).center_y(50).into()
}

// ---- macOS native button glue ----------------------------------------

#[cfg(target_os = "macos")]
fn compute_button_layout(state: &AppState) -> crate::native_buttons::Layout {
    use crate::native_buttons::{ButtonAction, ButtonSpec, Layout};
    let mut buttons: Vec<ButtonSpec> = Vec::new();
    let no_cancel = state.cancel_disabled();

    let back = ButtonSpec {
        label: "Go Back".into(),
        primary: false,
        enabled: true,
        action: ButtonAction::Prev,
    };
    let cancel = ButtonSpec {
        label: "Cancel".into(),
        primary: false,
        enabled: true,
        action: ButtonAction::Cancel,
    };
    let continue_button = |primary_label: &str, enabled: bool, action: ButtonAction| ButtonSpec {
        label: primary_label.into(),
        primary: true,
        enabled,
        action,
    };

    match state.step {
        WizardStep::Welcome => {
            if !no_cancel {
                buttons.push(cancel);
            }
            buttons.push(continue_button("Continue", true, ButtonAction::Next));
        }
        WizardStep::License => {
            if !no_cancel {
                buttons.push(cancel);
            }
            buttons.push(back);
            buttons.push(continue_button(
                "Continue",
                state.license_accepted,
                ButtonAction::Next,
            ));
        }
        WizardStep::Directory => {
            if !no_cancel {
                buttons.push(cancel);
            }
            buttons.push(back);
            buttons.push(continue_button(
                "Continue",
                !state.install_dir.is_empty(),
                ButtonAction::Next,
            ));
        }
        WizardStep::Components => {
            if !no_cancel {
                buttons.push(cancel);
            }
            buttons.push(back);
            buttons.push(continue_button("Continue", true, ButtonAction::Next));
        }
        WizardStep::Summary => {
            if !no_cancel {
                buttons.push(cancel);
            }
            buttons.push(back);
            buttons.push(continue_button("Install", true, ButtonAction::StartInstall));
        }
        WizardStep::Installing | WizardStep::Uninstalling => {
            if !no_cancel {
                buttons.push(cancel);
            }
        }
        WizardStep::Complete | WizardStep::UninstallComplete => {
            buttons.push(continue_button("Finish", true, ButtonAction::Finish));
        }
        WizardStep::UninstallConfirm => {
            if !no_cancel {
                buttons.push(cancel);
            }
            buttons.push(continue_button(
                "Uninstall",
                true,
                ButtonAction::StartUninstall,
            ));
        }
    }

    Layout { buttons }
}

#[cfg(target_os = "macos")]
fn map_native_button(
    _state: &AppState,
    action: crate::native_buttons::ButtonAction,
) -> Option<Message> {
    use crate::native_buttons::ButtonAction;
    Some(match action {
        ButtonAction::Next => Message::NextStep,
        ButtonAction::Prev => Message::PrevStep,
        ButtonAction::Cancel => Message::Cancel,
        ButtonAction::StartInstall => Message::StartInstall,
        ButtonAction::StartUninstall => Message::StartUninstall,
        ButtonAction::Finish => Message::Finish,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(license: bool) -> AppState {
        let config =
            Config::from_toml(
                "[package]\nid = \"no.divvun.test\"\nname = \"Test\"\nversion = \"1.0.0\"\n",
            )
            .unwrap();
        AppState::new(
            AppMode::Install,
            config,
            PathBuf::new(),
            CliFlags::default(),
            license.then(|| "license".to_string()),
            PathBuf::new(),
            None,
            "/tmp/test".to_string(),
            None,
        )
    }

    #[test]
    fn test_enter_targets_primary_button() {
        let mut state = test_state(false);
        // Welcome: primary is Next at index 0.
        assert_eq!(current_focus_target(&state), Some(FocusTarget::Button(0)));

        // Summary: buttons are [Back, Install, Cancel] — focus must default
        // to Install, not Back.
        state.step = WizardStep::Summary;
        state.focused_index = default_focus_index(&state);
        assert_eq!(current_focus_target(&state), Some(FocusTarget::Button(1)));

        // Installing: Enter must not implicitly hit anything.
        state.step = WizardStep::Installing;
        assert_eq!(primary_button_index(&state), None);
    }

    #[test]
    fn test_license_focus_follows_acceptance() {
        let mut state = test_state(true);
        state.step = WizardStep::License;
        state.focused_index = default_focus_index(&state);
        // Unaccepted: Continue is disabled; fall back to the checkbox.
        assert_eq!(
            current_focus_target(&state),
            Some(FocusTarget::LicenseCheckbox)
        );

        state.license_accepted = true;
        state.focused_index = default_focus_index(&state);
        assert_eq!(current_focus_target(&state), Some(FocusTarget::Button(1)));
    }
}

fn subscription(state: &AppState) -> Subscription<Message> {
    use std::time::Duration;

    let mut subs = vec![];

    // On macOS, hook every window-open event to install the NSVisualEffectView
    // behind the iced surface — this is what gives us real Liquid Glass.
    #[cfg(target_os = "macos")]
    subs.push(iced::window::open_events().map(Message::InstallGlass));

    // Drain clicks from the native NSButton overlay every frame. Cheap;
    // just a Mutex lock + empty Vec check when idle.
    #[cfg(target_os = "macos")]
    subs.push(iced::time::every(Duration::from_millis(16)).map(|_| Message::NativeButtonsTick));

    // Keyboard events
    subs.push(keyboard::listen().map(|event| {
        if let keyboard::Event::KeyPressed { key, modifiers, .. } = event {
            match key.as_ref() {
                keyboard::Key::Named(keyboard::key::Named::Tab) => {
                    if modifiers.shift() {
                        return Message::FocusPrev;
                    } else {
                        return Message::FocusNext;
                    }
                }
                keyboard::Key::Named(keyboard::key::Named::Enter) => {
                    return Message::ActivateFocused;
                }
                keyboard::Key::Named(keyboard::key::Named::Space) => {
                    return Message::ActivateFocused;
                }
                keyboard::Key::Named(keyboard::key::Named::Escape) => return Message::Cancel,
                _ => {}
            }
        }
        Message::BridgeUpdate // no-op fallback
    }));

    // Bridge polling (only when installing/uninstalling)
    let is_active = matches!(
        state.step,
        WizardStep::Installing | WizardStep::Uninstalling
    );

    if is_active
        && (BRIDGE_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) || state.pending_finish)
    {
        subs.push(iced::time::every(Duration::from_millis(16)).map(|_| Message::BridgeUpdate));
    }

    Subscription::batch(subs)
}
