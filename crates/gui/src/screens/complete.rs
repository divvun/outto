use iced::widget::{column, container, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    let mut needs_restart_notice = false;
    match &state.result {
        Some(Ok(())) => {
            col = col.push(text("Installation Complete").size(theme::FONT_TITLE));
            col = col.push(text(format!(
                "{} has been successfully installed on your computer.",
                state.config.package.name,
            )));
            needs_restart_notice = state.reboot_required;
        }
        Some(Err(e)) => {
            col = col.push(text("Installation Failed").size(theme::FONT_TITLE));
            col = col.push(text(format!("Error: {e}")));
        }
        None => {
            col = col.push(text("Installation Complete").size(theme::FONT_TITLE));
        }
    }

    if needs_restart_notice {
        col = col.push(
            text("A system restart is required to complete the installation.")
                .size(theme::FONT_BODY),
        );
    }

    col = col.push(space::vertical());
    let footer = if state.offer_restart() {
        "Restart now to finish, or restart later."
    } else {
        "Click Finish to exit Setup."
    };
    col = col.push(text(footer).size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}
