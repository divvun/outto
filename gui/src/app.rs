use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use iced::widget::{button, column, container, row, space, text};
use iced::{Element, Fill, Subscription, Task};

use outto::{Config, ErrorAction, LogLevel, PromptResponse};

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
}

// --- App state ---

pub struct AppState {
    pub mode: AppMode,
    pub step: WizardStep,
    pub config: Config,
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

    step_config: StepConfig,
}

impl AppState {
    pub fn new(
        mode: AppMode,
        config: Config,
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

        Self {
            mode,
            step: start_step,
            config,
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
        }
    }

    fn start_install(&mut self) {
        let selected: HashSet<String> = self
            .selected_components
            .iter()
            .filter(|(_, &v)| v)
            .map(|(k, _)| k.clone())
            .collect();

        let install_dir = if self.install_dir.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.install_dir))
        };

        BRIDGE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
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

        BRIDGE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
        bridge::spawn_uninstall(dir, self.flags.suppress_msgboxes, self.bridge_queue.clone());
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
                BridgeEvent::Finished(result) => {
                    self.result = Some(result);
                    match self.step {
                        WizardStep::Installing => self.step = WizardStep::Complete,
                        WizardStep::Uninstalling => self.step = WizardStep::UninstallComplete,
                        _ => {}
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

    iced::application(
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
    .theme(theme::windows11_theme())
    .window_size(iced::Size::new(theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT))
    .resizable(false)
    .run()
}

fn update(state: &mut AppState, message: Message) -> Task<Message> {
    match message {
        Message::NextStep => {
            if let Some(next) = state.step.next(&state.step_config) {
                state.step = next;
            }
            Task::none()
        }
        Message::PrevStep => {
            if let Some(prev) = state.step.prev(&state.step_config) {
                state.step = prev;
            }
            Task::none()
        }
        Message::Cancel => {
            std::process::exit(0);
        }
        Message::LicenseAccepted(accepted) => {
            state.license_accepted = accepted;
            Task::none()
        }
        Message::InstallDirChanged(dir) => {
            state.install_dir = dir;
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
            Task::none()
        }
        Message::StartUninstall => {
            state.step = WizardStep::Uninstalling;
            state.start_uninstall();
            Task::none()
        }
        Message::BridgeUpdate => {
            state.drain_bridge_queue();
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
    }
}

fn view(state: &AppState) -> Element<'_, Message> {
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

    column![header, content, button_bar].into()
}

fn nav_button(label: &str) -> iced::widget::Button<'_, Message> {
    button(text(label).size(theme::FONT_SECONDARY).center())
        .width(100)
        .padding([8, 16])
        .style(theme::win11_button)
}

fn view_button_bar(state: &AppState) -> Element<'_, Message> {
    let mut bar = row![].spacing(10).padding([12.0, theme::PADDING]);
    bar = bar.push(space::horizontal());

    match state.step {
        WizardStep::Welcome => {
            bar = bar.push(nav_button("Next >").on_press(Message::NextStep));
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        WizardStep::License => {
            bar = bar.push(nav_button("< Back").on_press(Message::PrevStep));
            if state.license_accepted {
                bar = bar.push(nav_button("Next >").on_press(Message::NextStep));
            } else {
                bar = bar.push(nav_button("Next >"));
            }
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        WizardStep::Directory => {
            bar = bar.push(nav_button("< Back").on_press(Message::PrevStep));
            if !state.install_dir.is_empty() {
                bar = bar.push(nav_button("Next >").on_press(Message::NextStep));
            } else {
                bar = bar.push(nav_button("Next >"));
            }
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        WizardStep::Components => {
            bar = bar.push(nav_button("< Back").on_press(Message::PrevStep));
            bar = bar.push(nav_button("Next >").on_press(Message::NextStep));
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        WizardStep::Summary => {
            bar = bar.push(nav_button("< Back").on_press(Message::PrevStep));
            bar = bar.push(nav_button("Install").on_press(Message::StartInstall));
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        WizardStep::Installing | WizardStep::Uninstalling => {
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        WizardStep::Complete | WizardStep::UninstallComplete => {
            bar = bar.push(nav_button("Finish").on_press(Message::Finish));
        }
        WizardStep::UninstallConfirm => {
            bar = bar.push(nav_button("Uninstall").on_press(Message::StartUninstall));
            if !state.flags.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
    }

    container(bar).width(Fill).height(50).center_y(50).into()
}

fn subscription(state: &AppState) -> Subscription<Message> {
    use std::time::Duration;

    let is_active = matches!(
        state.step,
        WizardStep::Installing | WizardStep::Uninstalling
    );

    if !is_active || !BRIDGE_ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
        return Subscription::none();
    }

    // Poll bridge queue at 60fps. The bridge_queue is populated by the background
    // install thread, and we drain it in update() on each BridgeUpdate tick.
    iced::time::every(Duration::from_millis(16)).map(|_| Message::BridgeUpdate)
}
