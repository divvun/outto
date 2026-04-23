use iced::widget::{button, column, container, progress_bar, row, scrollable, space, text};
use iced::{Element, Fill};
use outto::PromptResponse;

use crate::app::{AppState, Message};
use crate::bridge::PendingPrompt;
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("Installing...").size(theme::FONT_TITLE));
    col = col.push(text(&state.progress.phase).size(theme::FONT_SECONDARY));
    col = col.push(container(progress_bar(0.0..=100.0, state.progress.percent)).height(20));

    // Log area
    let mut log_col = column![].spacing(2);
    for line in &state.progress.log_lines {
        log_col = log_col
            .push(text(format!("[{:?}] {}", line.level, line.message)).size(theme::FONT_LOG));
    }
    col = col.push(
        container(scrollable(log_col))
            .height(200)
            .width(Fill)
            .style(container::bordered_box),
    );

    col = col.push(space::vertical());

    // Prompt overlay (inline, at bottom)
    if let Some(ref pending) = state.pending_prompt {
        col = col.push(prompt_view(pending));
    } else if let Some(ref pending) = state.pending_error {
        col = col.push(error_view(&pending.error_message));
    }

    container(col).width(Fill).height(Fill).into()
}

fn prompt_view(pending: &PendingPrompt) -> Element<'_, Message> {
    let prompt_text = match &pending.prompt {
        outto::Prompt::OverwriteFile { path } => {
            format!(
                "File already exists: {}\nOverwrite?",
                theme::normalize_path(&path.display().to_string())
            )
        }
        outto::Prompt::ExistingInstallDetected { existing } => {
            format!(
                "Existing installation detected: {} v{}\nContinue?",
                existing.display_name.as_deref().unwrap_or("unknown"),
                existing.version.as_deref().unwrap_or("unknown"),
            )
        }
    };

    let buttons = row![
        button("Yes").on_press(Message::PromptResponse(PromptResponse::Yes)),
        button("No").on_press(Message::PromptResponse(PromptResponse::No)),
        button("Yes to All").on_press(Message::PromptResponse(PromptResponse::YesToAll)),
        button("Cancel").on_press(Message::PromptResponse(PromptResponse::Cancel)),
    ]
    .spacing(8);

    container(column![text(prompt_text).size(theme::FONT_SECONDARY), buttons].spacing(8))
        .padding(12)
        .style(container::bordered_box)
        .width(Fill)
        .into()
}

fn error_view(error_message: &str) -> Element<'_, Message> {
    let buttons = row![
        button("Abort").on_press(Message::ErrorResponse(outto::ErrorAction::Abort)),
        button("Retry").on_press(Message::ErrorResponse(outto::ErrorAction::Retry)),
        button("Ignore").on_press(Message::ErrorResponse(outto::ErrorAction::Ignore)),
    ]
    .spacing(8);

    container(
        column![
            text(format!("Error: {error_message}")).size(theme::FONT_SECONDARY),
            buttons,
        ]
        .spacing(8),
    )
    .padding(12)
    .style(container::bordered_box)
    .width(Fill)
    .into()
}
