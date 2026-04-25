//! macOS-only left-hand sidebar that mirrors Apple's `.pkg` Installer layout.
//!
//! Steps come from the existing `WizardStep` + `StepConfig` state machine — this
//! module only builds the visual list, it doesn't duplicate navigation logic.

use iced::widget::{column, container, text, Space};
use iced::{Element, Fill, Font, Length};

use crate::app::{AppMode, AppState, Message, StepConfig, WizardStep};
use crate::layout::SIDEBAR_WIDTH;
use crate::theme;

const ITEM_HEIGHT: f32 = 24.0;
const TOP_PADDING: f32 = 18.0;
const SIDE_PADDING: f32 = 14.0;

/// Apple-style display label for each step.
fn label(step: WizardStep) -> &'static str {
    match step {
        WizardStep::Welcome => "Introduction",
        WizardStep::License => "License",
        WizardStep::Directory => "Destination Select",
        WizardStep::Components => "Installation Type",
        WizardStep::Summary => "Summary",
        WizardStep::Installing => "Installation",
        WizardStep::Complete => "Finish",
        WizardStep::UninstallConfirm => "Confirm",
        WizardStep::Uninstalling => "Uninstalling",
        WizardStep::UninstallComplete => "Finish",
    }
}

fn install_steps(cfg: &StepConfig) -> Vec<WizardStep> {
    let mut steps = vec![WizardStep::Welcome];
    if cfg.has_license {
        steps.push(WizardStep::License);
    }
    if cfg.has_directory {
        steps.push(WizardStep::Directory);
    }
    if cfg.has_components {
        steps.push(WizardStep::Components);
    }
    steps.push(WizardStep::Summary);
    steps.push(WizardStep::Installing);
    steps.push(WizardStep::Complete);
    steps
}

fn uninstall_steps() -> Vec<WizardStep> {
    vec![
        WizardStep::UninstallConfirm,
        WizardStep::Uninstalling,
        WizardStep::UninstallComplete,
    ]
}

pub fn view(state: &AppState) -> Element<'_, Message> {
    let steps = match state.mode {
        AppMode::Install => install_steps(&state.step_config),
        AppMode::Uninstall => uninstall_steps(),
    };

    let active_idx = steps.iter().position(|&s| s == state.step);

    let mut col = column![].spacing(2).padding([0.0, SIDE_PADDING]);

    for (i, step) in steps.iter().enumerate() {
        let is_active = Some(i) == active_idx;
        let is_future = active_idx.map(|a| i > a).unwrap_or(false);

        // Semibold for the active step matches Apple's Installer sidebar;
        // everything else stays in the base weight, dimmed for future steps.
        let font = if is_active {
            theme::semibold_font()
        } else {
            theme::default_font()
        };

        let color = if is_active {
            iced::Color::WHITE
        } else if is_future {
            theme::muted_text_color()
        } else {
            theme::text_color()
        };

        // Active row gets a leading chevron in the same text run as the
        // label so the two share a baseline cleanly. U+276F (❯, HEAVY
        // RIGHT-POINTING ANGLE QUOTATION MARK ORNAMENT) sits closer to
        // the alphabetic centreline than U+203A (›), which reads too high.
        let display = if is_active {
            format!("\u{276F}  {}", label(*step))
        } else {
            label(*step).to_string()
        };

        let inner: Element<'_, Message> = text(display)
            .size(theme::FONT_BODY)
            .font(font)
            .color(color)
            .into();

        // Asymmetric vertical padding (top 2, bottom 6) nudges the text
        // up ~2px so optically it sits centred in the pill — the glyph
        // metrics of SF Pro leave a bottom-heavy impression otherwise.
        let cell = container(inner)
            .width(Fill)
            .height(ITEM_HEIGHT)
            .padding(iced::Padding {
                top: 3.0,
                right: 10.0,
                bottom: 0.0,
                left: 10.0,
            });

        let line = if is_active {
            cell.style(theme::sidebar_item_active)
        } else {
            cell
        };

        col = col.push(line);
    }

    // Bottom-left footer showing the package identity. Menlo is always
    // available on macOS so we pin to it directly rather than going through
    // a theme helper. Dimmer than `muted_text_color` so it recedes into
    // the sidebar chrome.
    let footer = container(
        text(format!(
            "{}/{}",
            state.config.package.id, state.config.package.version
        ))
        .size(8.0)
        .font(Font::with_name("Menlo"))
        .color(iced::Color::from_rgba(1.0, 1.0, 1.0, 0.35))
        .width(Fill)
        .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Fill)
    .padding(iced::Padding {
        top: 0.0,
        right: SIDE_PADDING,
        bottom: 12.0,
        left: SIDE_PADDING,
    });

    // Position the pill list 1/6 from the top: 1-unit spacer above,
    // 5-unit spacer below, content in between. Footer is pinned to the
    // bottom after the flex spacer.
    let positioned = column![
        Space::new().height(Length::FillPortion(1)),
        col,
        Space::new().height(Length::FillPortion(5)),
        footer,
    ];

    container(positioned)
        .width(SIDEBAR_WIDTH)
        .height(Fill)
        .style(theme::sidebar_style)
        .into()
}
