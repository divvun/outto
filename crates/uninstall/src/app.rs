use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{atomic::AtomicBool, Mutex};

use iced::widget::{button, column, container, progress_bar, row, scrollable, space, text};
use iced::{Element, Fill, Subscription, Task};

use outto_core::LogLevel;

/// Return the display names of packages that will be cascade-uninstalled.
/// On Windows, walks the Add/Remove Programs registry. On macOS, walks the
/// receipt-file layout under `~/Library/no.divvun.install/packages/` and
/// `/Library/no.divvun.install/packages/`.
#[cfg(windows)]
fn collect_cascade_names(package_id: &str) -> Vec<String> {
    let cascade = outto_windows::uninstall::collect_cascade_order(package_id);
    cascade
        .iter()
        .map(|p| {
            outto_windows::detect::detect_existing_install(&p.package_id)
                .ok()
                .flatten()
                .and_then(|e| e.display_name)
                .unwrap_or_else(|| p.package_id.clone())
        })
        .collect()
}

#[cfg(target_os = "macos")]
fn collect_cascade_names(package_id: &str) -> Vec<String> {
    let cascade = outto_macos::uninstall::collect_cascade_order(package_id);
    cascade
        .iter()
        .map(|p| {
            outto_macos::detect::detect_existing_install(&p.package_id)
                .ok()
                .flatten()
                .and_then(|e| e.display_name)
                .unwrap_or_else(|| p.package_id.clone())
        })
        .collect()
}

#[cfg(not(any(windows, target_os = "macos")))]
fn collect_cascade_names(_package_id: &str) -> Vec<String> {
    Vec::new()
}

use crate::bridge::{self, BridgeEvent, BridgeQueue};
use crate::theme;

static BRIDGE_ACTIVE: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Confirm,
    Uninstalling,
    Complete,
}

pub struct LogLine {
    pub level: LogLevel,
    pub message: String,
}

pub struct ProgressState {
    pub phase: String,
    pub percent: f32,
    pub log_lines: Vec<LogLine>,
}

impl Default for ProgressState {
    fn default() -> Self {
        Self {
            phase: String::new(),
            percent: 0.0,
            log_lines: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    StartUninstall,
    Cancel,
    BridgeUpdate,
    Finish,
}

pub struct AppState {
    pub step: Step,
    pub app_name: String,
    pub app_version: String,
    pub install_dir: PathBuf,
    pub package_id: String,
    pub cascade_names: Vec<String>,
    pub progress: ProgressState,
    pub result: Option<Result<(), String>>,
    pub bridge_queue: BridgeQueue,
    pub no_cancel: bool,
}

impl AppState {
    pub fn new(
        app_name: String,
        app_version: String,
        install_dir: PathBuf,
        package_id: String,
        silent: bool,
        no_cancel: bool,
    ) -> Self {
        // Find packages that will be cascade-uninstalled (Windows-only for now —
        // the macOS backend has no cascade detection yet, so its list is empty).
        let cascade_names: Vec<String> = collect_cascade_names(&package_id);

        Self {
            step: if silent {
                Step::Uninstalling
            } else {
                Step::Confirm
            },
            app_name,
            app_version,
            install_dir,
            package_id,
            cascade_names,
            progress: ProgressState::default(),
            result: None,
            bridge_queue: std::sync::Arc::new(Mutex::new(VecDeque::new())),
            no_cancel,
        }
    }

    fn start_uninstall(&mut self) {
        BRIDGE_ACTIVE.store(true, std::sync::atomic::Ordering::SeqCst);
        bridge::spawn_uninstall(
            self.install_dir.clone(),
            self.package_id.clone(),
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
                BridgeEvent::Finished(result) => {
                    self.result = Some(result);
                    self.step = Step::Complete;
                }
            }
        }
    }
}

pub fn run(state: AppState) -> iced::Result {
    let auto_start = state.step == Step::Uninstalling;
    let state_cell = Mutex::new(Some(state));

    iced::application(
        move || {
            let state = state_cell
                .lock()
                .unwrap()
                .take()
                .expect("boot called twice");
            let task = if auto_start {
                Task::done(Message::StartUninstall)
            } else {
                Task::none()
            };
            (state, task)
        },
        update,
        view,
    )
    .subscription(subscription)
    .title(|state: &AppState| format!("{} Uninstall", state.app_name))
    .theme(theme::windows11_theme())
    .default_font(iced::Font::DEFAULT)
    .window_size(iced::Size::new(theme::WINDOW_WIDTH, theme::WINDOW_HEIGHT))
    .resizable(false)
    .run()
}

fn update(state: &mut AppState, message: Message) -> Task<Message> {
    match message {
        Message::StartUninstall => {
            crate::relocate_self();
            state.step = Step::Uninstalling;
            state.start_uninstall();
            Task::none()
        }
        Message::Cancel => {
            std::process::exit(0);
        }
        Message::BridgeUpdate => {
            state.drain_bridge_queue();
            Task::none()
        }
        Message::Finish => {
            if state.result.as_ref().is_some_and(|r| r.is_ok()) {
                crate::cleanup_after_uninstall(&state.install_dir);
                std::process::exit(0);
            } else {
                std::process::exit(1);
            }
        }
    }
}

fn view(state: &AppState) -> Element<'_, Message> {
    let header = container(
        text(format!("{} Uninstall", state.app_name))
            .size(theme::FONT_HEADER)
            .color(theme::HEADER_TEXT),
    )
    .width(Fill)
    .height(theme::HEADER_HEIGHT)
    .padding([0.0, theme::PADDING])
    .center_y(theme::HEADER_HEIGHT)
    .style(theme::header_style);

    let content: Element<'_, Message> = match state.step {
        Step::Confirm => view_confirm(state),
        Step::Uninstalling => view_progress(state),
        Step::Complete => view_complete(state),
    };

    let button_bar = view_button_bar(state);

    column![header, content, button_bar].into()
}

fn view_confirm(state: &AppState) -> Element<'_, Message> {
    let mut col = column![
        text("Uninstall").size(theme::FONT_TITLE),
        text(format!(
            "Are you sure you want to completely remove {} v{} and all of its components?",
            state.app_name, state.app_version,
        ))
        .size(theme::FONT_BODY),
    ]
    .spacing(theme::SPACING)
    .padding(theme::PADDING);

    if !state.cascade_names.is_empty() {
        col = col.push(
            text("The following dependent packages will also be removed:").size(theme::FONT_BODY),
        );
        let mut list_col = column![].spacing(2);
        for name in &state.cascade_names {
            list_col =
                list_col.push(text(format!("  \u{2022} {name}")).size(theme::FONT_SECONDARY));
        }
        col = col.push(
            container(scrollable(list_col))
                .height(Fill)
                .width(Fill)
                .style(container::bordered_box),
        );
    } else {
        col = col.push(space::vertical());
    }

    col = col
        .push(text("Click Uninstall to proceed, or Cancel to exit.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}

fn view_progress(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);
    col = col.push(text("Uninstalling...").size(theme::FONT_TITLE));
    col = col.push(text(&state.progress.phase).size(theme::FONT_SECONDARY));
    col = col.push(container(progress_bar(0.0..=100.0, state.progress.percent)).height(20));

    let mut log_col = column![].spacing(2);
    for line in &state.progress.log_lines {
        log_col = log_col
            .push(text(format!("[{:?}] {}", line.level, line.message)).size(theme::FONT_LOG));
    }
    col = col.push(
        container(scrollable(log_col))
            .height(150)
            .width(Fill)
            .style(container::bordered_box),
    );

    col = col.push(space::vertical());
    container(col).width(Fill).height(Fill).into()
}

fn view_complete(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    match &state.result {
        Some(Ok(())) => {
            col = col.push(text("Uninstall Complete").size(theme::FONT_TITLE));
            col = col.push(
                text(format!(
                    "{} has been successfully removed from your computer.",
                    state.app_name,
                ))
                .size(theme::FONT_BODY),
            );
        }
        Some(Err(e)) => {
            col = col.push(text("Uninstall Failed").size(theme::FONT_TITLE));
            col = col.push(text(format!("Error: {e}")).size(theme::FONT_BODY));
        }
        None => {
            col = col.push(text("Uninstall Complete").size(theme::FONT_TITLE));
        }
    }

    col = col.push(space::vertical());
    col = col.push(text("Click Finish to exit.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
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
        Step::Confirm => {
            bar = bar.push(nav_button("Uninstall").on_press(Message::StartUninstall));
            if !state.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        Step::Uninstalling => {
            if !state.no_cancel {
                bar = bar.push(nav_button("Cancel").on_press(Message::Cancel));
            }
        }
        Step::Complete => {
            bar = bar.push(nav_button("Finish").on_press(Message::Finish));
        }
    }

    container(bar).width(Fill).height(50).center_y(50).into()
}

fn subscription(state: &AppState) -> Subscription<Message> {
    if state.step != Step::Uninstalling || !BRIDGE_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
    {
        return Subscription::none();
    }

    iced::time::every(std::time::Duration::from_millis(16)).map(|_| Message::BridgeUpdate)
}
