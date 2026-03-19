use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("Select Destination Location").size(theme::FONT_TITLE));
    col = col.push(text(format!(
        "Where should {} be installed?",
        state.config.package.name
    )));

    col = col.push(
        row![
            text_input("Installation directory...", &state.install_dir)
                .on_input(Message::InstallDirChanged)
                .width(Fill),
            button("Browse...").on_press(Message::BrowseDirectory),
        ]
        .spacing(8),
    );

    col = col.push(space::vertical());

    container(col).width(Fill).height(Fill).into()
}
