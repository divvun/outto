use iced::widget::{checkbox, column, container, space, text};
use iced::{Element, Fill};

use crate::app::{current_focus_target, AppState, FocusTarget, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("Select Components").size(theme::FONT_TITLE));
    col = col.push(text("Select the components you want to install."));

    for (i, comp) in state.config.components.iter().enumerate() {
        let label = comp.display_name.as_deref().unwrap_or(&comp.name);
        let checked = state
            .selected_components
            .get(&comp.name)
            .copied()
            .unwrap_or(comp.required || comp.default);

        let focused = current_focus_target(state) == Some(FocusTarget::ComponentCheckbox(i));

        let cb = checkbox(checked)
            .label(label.to_string())
            .size(20)
            .text_size(theme::FONT_BODY)
            .spacing(8);

        if comp.required {
            col = col.push(container(cb).padding(2).style(theme::no_focus_ring));
        } else {
            let cb = cb.on_toggle({
                let name = comp.name.clone();
                move |checked| Message::ComponentToggled(name.clone(), checked)
            });
            let ring = if focused {
                theme::focus_ring
            } else {
                theme::no_focus_ring
            };
            col = col.push(container(cb).padding(2).style(ring));
        }
    }

    col = col.push(space::vertical());

    container(col).width(Fill).height(Fill).into()
}
