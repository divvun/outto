use iced::widget::{column, container, progress_bar, scrollable, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
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
            .height(200)
            .width(Fill)
            .style(container::bordered_box),
    );

    col = col.push(space::vertical());

    container(col).width(Fill).height(Fill).into()
}
