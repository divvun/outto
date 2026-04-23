use iced::font::Weight;
use iced::widget::{column, container, space, text};
use iced::{Element, Fill, Font};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let pkg = &state.config.package;

    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col =
        col.push(text(format!("Welcome to the {} Setup Wizard", pkg.name)).size(theme::FONT_TITLE));

    col = col.push(space::Space::new().height(10));

    col = col.push(text(format!(
        "This will install {} version {} on your computer.",
        pkg.name, pkg.version
    )));

    if let Some(ref publisher) = pkg.publisher {
        let bold = Font {
            weight: Weight::Bold,
            ..Font::DEFAULT
        };
        col = col.push(
            text(format!("Publisher: {publisher}"))
                .font(bold)
                .size(theme::FONT_SECONDARY),
        );
    }

    col = col.push(space::vertical());

    col = col
        .push(text("Click Next to continue, or Cancel to exit Setup.").size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}
