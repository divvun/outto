use iced::widget::{checkbox, column, container, scrollable, text};
use iced::{Element, Fill};

use crate::app::{AppState, FocusTarget, Message, current_focus_target};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let license_text = state
        .license_text
        .as_deref()
        .unwrap_or("No license text available.");

    let focused = current_focus_target(state) == Some(FocusTarget::LicenseCheckbox);

    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("License Agreement").size(theme::FONT_TITLE));
    col = col.push(
        text("Please read the following license agreement carefully.").size(theme::FONT_SECONDARY),
    );

    col = col.push(
        container(scrollable(text(license_text).size(theme::FONT_SECONDARY)))
            .height(Fill)
            .width(Fill)
            .style(container::bordered_box),
    );

    let cb = checkbox(state.license_accepted)
        .label("I accept the terms in the License Agreement")
        .size(20)
        .text_size(theme::FONT_BODY)
        .spacing(8)
        .on_toggle(Message::LicenseAccepted);

    let ring = if focused {
        theme::focus_ring
    } else {
        theme::no_focus_ring
    };
    col = col.push(container(cb).padding(2).style(ring));

    container(col).width(Fill).height(Fill).into()
}
