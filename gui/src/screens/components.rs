use iced::widget::{checkbox, column, container, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("Select Components").size(theme::FONT_TITLE));
    col = col.push(text("Select the components you want to install."));

    for comp in &state.config.components {
        let label = comp.display_name.as_deref().unwrap_or(&comp.name);
        let checked = state
            .selected_components
            .get(&comp.name)
            .copied()
            .unwrap_or(comp.required || comp.default);

        let cb = checkbox(checked)
            .label(label.to_string())
            .size(20)
            .text_size(theme::FONT_BODY)
            .spacing(8);
        if comp.required {
            // Required components are always checked and not toggleable
            col = col.push(cb);
        } else {
            col = col.push(
                cb.on_toggle(|checked| Message::ComponentToggled(comp.name.clone(), checked)),
            );
        }
    }

    col = col.push(space::vertical());

    container(col).width(Fill).height(Fill).into()
}
