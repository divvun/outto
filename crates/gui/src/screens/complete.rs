use iced::widget::{column, container, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    match &state.result {
        Some(Ok(())) => {
            col = col.push(text("Installation Complete").size(theme::FONT_TITLE));
            col = col.push(text(format!(
                "{} has been successfully installed on your computer.",
                state.config.package.name,
            )));
        }
        Some(Err(e)) => {
            col = col.push(text("Installation Failed").size(theme::FONT_TITLE));
            col = col.push(text(format!("Error: {e}")));
        }
        None => {
            col = col.push(text("Installation Complete").size(theme::FONT_TITLE));
        }
    }

    col = col.push(space::vertical());
    col = col.push(text("Click Finish to exit Setup.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}
