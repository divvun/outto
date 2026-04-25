use iced::widget::{column, container, space, text};
use iced::{Element, Fill};

use crate::app::{AppState, Message};
use crate::theme;

pub fn view(state: &AppState) -> Element<'_, Message> {
    let pkg = &state.config.package;

    let mut col = column![].spacing(theme::SPACING).padding(theme::PADDING);

    col = col.push(
        text(format!(
            "Welcome to the {}",
            welcome_subject(pkg.name.as_str())
        ))
        .size(theme::FONT_TITLE)
        .font(theme::semibold_font()),
    );

    col = col.push(space::Space::new().height(10));

    col = col.push(
        text(format!(
            "This will install {} version {} on your computer.",
            pkg.name, pkg.version
        ))
        .size(theme::FONT_BODY),
    );

    if let Some(ref publisher) = pkg.publisher {
        col = col.push(
            text(format!("Publisher: {publisher}"))
                .font(theme::semibold_font())
                .size(theme::FONT_SECONDARY),
        );
    }

    col = col.push(space::vertical());

    col = col.push(text(continue_instruction()).size(theme::FONT_SECONDARY));

    container(col).width(Fill).height(Fill).into()
}

#[cfg(target_os = "macos")]
fn welcome_subject(name: &str) -> String {
    format!("{name} Installer")
}

#[cfg(not(target_os = "macos"))]
fn welcome_subject(name: &str) -> String {
    format!("{name} Setup Wizard")
}

#[cfg(target_os = "macos")]
fn continue_instruction() -> &'static str {
    "Click Continue to proceed, or Cancel to exit."
}

#[cfg(not(target_os = "macos"))]
fn continue_instruction() -> &'static str {
    "Click Next to continue, or Cancel to exit Setup."
}
