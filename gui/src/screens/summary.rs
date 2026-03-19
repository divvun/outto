use iced::widget::{column, container, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let pkg = &state.config.package;
    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(text("Ready to Install").size(theme::FONT_TITLE));
    col = col.push(text("Setup is now ready to begin installing on your computer.").size(theme::FONT_SECONDARY));
    col = col.push(space::Space::new().height(8));

    col = col.push(text(format!("Application: {} v{}", pkg.name, pkg.version)));
    col = col.push(text(format!("Install directory: {}", theme::normalize_path(&state.install_dir))));

    if !state.config.components.is_empty() {
        let selected: Vec<&str> = state
            .config
            .components
            .iter()
            .filter(|c| {
                state
                    .selected_components
                    .get(&c.name)
                    .copied()
                    .unwrap_or(c.required || c.default)
            })
            .map(|c| c.display_name.as_deref().unwrap_or(&c.name))
            .collect();
        col = col.push(text(format!("Components: {}", selected.join(", "))));
    }

    col = col.push(space::vertical());

    col = col.push(text("Click Install to begin the installation.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}
