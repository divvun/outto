use iced::widget::{column, container, row, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let pkg = &state.config.package;
    let semibold = theme::semibold_font();

    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(
        text("Ready to Install")
            .size(theme::FONT_TITLE)
            .font(semibold),
    );
    col = col.push(
        text("Setup is now ready to begin installing on your computer.").size(theme::FONT_BODY),
    );
    col = col.push(space::Space::new().height(8));

    let label_width = 140.0;

    col = col.push(
        row![
            text("Application")
                .font(semibold)
                .size(theme::FONT_BODY)
                .width(label_width),
            text(format!("{} v{}", pkg.name, pkg.version)).size(theme::FONT_BODY),
        ]
        .spacing(8),
    );

    col = col.push(
        row![
            text("Install to")
                .font(semibold)
                .size(theme::FONT_BODY)
                .width(label_width),
            text(theme::normalize_path(&state.install_dir)).size(theme::FONT_BODY),
        ]
        .spacing(8),
    );

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
        col = col.push(
            row![
                text("Components")
                    .font(semibold)
                    .size(theme::FONT_BODY)
                    .width(label_width),
                text(selected.join(", ")).size(theme::FONT_BODY),
            ]
            .spacing(8),
        );
    }

    col = col.push(space::vertical());

    col = col.push(text("Click Install to begin the installation.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}
