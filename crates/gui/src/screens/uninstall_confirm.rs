use iced::widget::{column, container, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("Uninstall").size(theme::FONT_TITLE));
    col = col.push(text(format!(
        "Are you sure you want to completely remove {} and all of its components?",
        state.config.package.name,
    )));

    col = col.push(space::vertical());
    col = col
        .push(text("Click Uninstall to proceed, or Cancel to exit.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}
