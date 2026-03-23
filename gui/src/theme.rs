use iced::widget::{button, container};
use iced::{Border, Color, Theme};

pub const WINDOW_WIDTH: f32 = 620.0;
pub const WINDOW_HEIGHT: f32 = 480.0;
pub const HEADER_HEIGHT: f32 = 60.0;
pub const PADDING: f32 = 30.0;
pub const SPACING: f32 = 16.0;

// Font sizes
pub const FONT_TITLE: f32 = 24.0;
pub const FONT_HEADER: f32 = 20.0;
pub const FONT_BODY: f32 = 16.0;
pub const FONT_SECONDARY: f32 = 14.0;
pub const FONT_LOG: f32 = 12.0;

// Windows 11 colors
pub const HEADER_BG: Color = Color::from_rgb(0.0, 0.471, 0.831); // #0078D4
pub const HEADER_TEXT: Color = Color::WHITE;

/// Windows 11 light theme.
pub fn windows11_theme() -> Theme {
    Theme::custom(
        "Windows 11".into(),
        iced::theme::Palette {
            background: Color::from_rgb(0.953, 0.953, 0.953), // #F3F3F3
            text: Color::from_rgb(0.122, 0.122, 0.122),       // #1F1F1F
            primary: Color::from_rgb(0.0, 0.471, 0.831),      // #0078D4
            success: Color::from_rgb(0.063, 0.486, 0.063),    // #107C10
            warning: Color::from_rgb(1.0, 0.725, 0.0),        // #FFB900
            danger: Color::from_rgb(0.820, 0.204, 0.220),     // #D13438
        },
    )
}

/// Windows 11-style button.
pub fn win11_button(_theme: &Theme, status: button::Status) -> button::Style {
    let base = button::Style {
        background: Some(Color::from_rgb(0.898, 0.898, 0.898).into()), // #E5E5E5
        text_color: Color::from_rgb(0.122, 0.122, 0.122),
        border: Border {
            radius: 4.0.into(),
            width: 1.0,
            color: Color::from_rgb(0.800, 0.800, 0.800), // #CCCCCC
        },
        shadow: Default::default(),
    };

    match status {
        button::Status::Active => base,
        button::Status::Hovered => button::Style {
            background: Some(Color::from_rgb(0.847, 0.847, 0.847).into()), // #D8D8D8
            ..base
        },
        button::Status::Pressed => button::Style {
            background: Some(Color::from_rgb(0.780, 0.780, 0.780).into()), // #C7C7C7
            ..base
        },
        button::Status::Disabled => button::Style {
            background: Some(Color::from_rgb(0.940, 0.940, 0.940).into()), // #F0F0F0
            text_color: Color::from_rgb(0.600, 0.600, 0.600),
            border: Border {
                radius: 4.0.into(),
                width: 1.0,
                color: Color::from_rgb(0.880, 0.880, 0.880),
            },
            shadow: Default::default(),
        },
    }
}

/// Header container style.
pub fn header_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(HEADER_BG.into()),
        ..Default::default()
    }
}

/// Normalize path separators for display on Windows.
pub fn normalize_path(path: &str) -> String {
    path.replace('/', "\\")
}
