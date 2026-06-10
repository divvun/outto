use iced::widget::button;
#[cfg(not(target_os = "macos"))]
use iced::widget::container;
use iced::{Border, Color, Font, Theme};

// --- Shared metrics ---

pub const WINDOW_WIDTH: f32 = 800.0;
pub const WINDOW_HEIGHT: f32 = 500.0;
pub const PADDING: f32 = 30.0;
pub const SPACING: f32 = 16.0;

pub const FONT_TITLE: f32 = 24.0;
pub const FONT_BODY: f32 = 16.0;
pub const FONT_SECONDARY: f32 = 14.0;
pub const FONT_LOG: f32 = 12.0;

#[cfg(not(target_os = "macos"))]
pub const HEADER_HEIGHT: f32 = 60.0;
#[cfg(not(target_os = "macos"))]
pub const FONT_HEADER: f32 = 20.0;

// --- Windows 11 ---

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub const HEADER_BG: Color = Color::from_rgb(0.0, 0.471, 0.831);
    pub const HEADER_TEXT: Color = Color::WHITE;

    pub fn make_theme() -> Theme {
        Theme::custom(
            String::from("Windows 11"),
            iced::theme::Palette {
                background: Color::from_rgb(0.953, 0.953, 0.953),
                text: Color::from_rgb(0.122, 0.122, 0.122),
                primary: Color::from_rgb(0.0, 0.471, 0.831),
                success: Color::from_rgb(0.063, 0.486, 0.063),
                warning: Color::from_rgb(1.0, 0.725, 0.0),
                danger: Color::from_rgb(0.820, 0.204, 0.220),
            },
        )
    }

    pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let base = button::Style {
            background: Some(Color::from_rgb(0.898, 0.898, 0.898).into()),
            text_color: Color::from_rgb(0.122, 0.122, 0.122),
            border: Border {
                radius: 4.0.into(),
                width: 1.0,
                color: Color::from_rgb(0.800, 0.800, 0.800),
            },
            shadow: Default::default(),
            snap: false,
        };
        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(Color::from_rgb(0.847, 0.847, 0.847).into()),
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(Color::from_rgb(0.780, 0.780, 0.780).into()),
                ..base
            },
            button::Status::Disabled => button::Style {
                background: Some(Color::from_rgb(0.940, 0.940, 0.940).into()),
                text_color: Color::from_rgb(0.600, 0.600, 0.600),
                border: Border {
                    radius: 4.0.into(),
                    width: 1.0,
                    color: Color::from_rgb(0.880, 0.880, 0.880),
                },
                shadow: Default::default(),
                snap: false,
            },
        }
    }

    // Windows 11 primary == secondary visually; the difference is just which button gets focus.
    pub fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
        secondary_button(theme, status)
    }

    pub fn header_style(_theme: &Theme) -> container::Style {
        container::Style {
            background: Some(HEADER_BG.into()),
            ..Default::default()
        }
    }

    pub fn normalize_path(path: &str) -> String {
        path.replace('/', "\\")
    }

    pub fn default_font() -> Font {
        Font::DEFAULT
    }

    pub fn default_font_bytes() -> Option<Vec<u8>> {
        None
    }
}

// --- macOS (Aqua, light/dark) ---

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::process::Command;
    use std::sync::OnceLock;

    fn from_hex(r: u8, g: u8, b: u8) -> Color {
        Color::from_rgb(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
    }

    fn dark_mode() -> bool {
        static CACHED: OnceLock<bool> = OnceLock::new();
        *CACHED.get_or_init(|| {
            Command::new("defaults")
                .args(["read", "-g", "AppleInterfaceStyle"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .eq_ignore_ascii_case("Dark")
                })
                .unwrap_or(false)
        })
    }

    struct Palette {
        bg: Color,
        text: Color,
        primary: Color,
        primary_hover: Color,
        primary_pressed: Color,
        border: Color,
        secondary_bg: Color,
        secondary_hover: Color,
        secondary_pressed: Color,
    }

    fn palette() -> &'static Palette {
        static CACHED: OnceLock<Palette> = OnceLock::new();
        CACHED.get_or_init(|| {
            if dark_mode() {
                Palette {
                    bg: from_hex(0x1E, 0x1E, 0x1E),
                    text: from_hex(0xF2, 0xF2, 0xF7),
                    primary: from_hex(0x0A, 0x84, 0xFF),
                    primary_hover: from_hex(0x33, 0x99, 0xFF),
                    primary_pressed: from_hex(0x00, 0x6A, 0xDD),
                    border: from_hex(0x3A, 0x3A, 0x3C),
                    secondary_bg: from_hex(0x3A, 0x3A, 0x3C),
                    secondary_hover: from_hex(0x48, 0x48, 0x4A),
                    secondary_pressed: from_hex(0x2C, 0x2C, 0x2E),
                }
            } else {
                Palette {
                    bg: Color::WHITE,
                    text: from_hex(0x1C, 0x1C, 0x1E),
                    primary: from_hex(0x00, 0x7A, 0xFF),
                    primary_hover: from_hex(0x33, 0x95, 0xFF),
                    primary_pressed: from_hex(0x00, 0x60, 0xD0),
                    border: from_hex(0xD1, 0xD1, 0xD6),
                    secondary_bg: Color::WHITE,
                    secondary_hover: from_hex(0xF2, 0xF2, 0xF2),
                    secondary_pressed: from_hex(0xE5, 0xE5, 0xE5),
                }
            }
        })
    }

    pub fn make_theme() -> Theme {
        let p = palette();
        let name = if dark_mode() { "Aqua Dark" } else { "Aqua" };
        Theme::custom(
            String::from(name),
            iced::theme::Palette {
                background: p.bg,
                text: p.text,
                primary: p.primary,
                success: from_hex(0x30, 0xD1, 0x58),
                warning: from_hex(0xFF, 0x9F, 0x0A),
                danger: from_hex(0xFF, 0x45, 0x3A),
            },
        )
    }

    pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let p = palette();
        let base = button::Style {
            background: Some(p.primary.into()),
            text_color: Color::WHITE,
            border: Border {
                radius: 6.0.into(),
                width: 0.0,
                color: Color::TRANSPARENT,
            },
            shadow: Default::default(),
            snap: false,
        };
        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(p.primary_hover.into()),
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(p.primary_pressed.into()),
                ..base
            },
            button::Status::Disabled => button::Style {
                background: Some(
                    Color {
                        a: 0.4,
                        ..p.primary
                    }
                    .into(),
                ),
                text_color: Color {
                    a: 0.7,
                    ..Color::WHITE
                },
                ..base
            },
        }
    }

    pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let p = palette();
        let base = button::Style {
            background: Some(p.secondary_bg.into()),
            text_color: p.text,
            border: Border {
                radius: 6.0.into(),
                width: 1.0,
                color: p.border,
            },
            shadow: Default::default(),
            snap: false,
        };
        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(p.secondary_hover.into()),
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(p.secondary_pressed.into()),
                ..base
            },
            button::Status::Disabled => button::Style {
                background: Some(
                    Color {
                        a: 0.5,
                        ..p.secondary_bg
                    }
                    .into(),
                ),
                text_color: Color { a: 0.5, ..p.text },
                ..base
            },
        }
    }

    pub fn normalize_path(path: &str) -> String {
        path.to_string()
    }

    /// SF Pro via `/System/Library/Fonts/SFNS.ttf`. Bold rendering requires the
    /// vendored cosmic-text fix — see `vendor/cosmic-text/src/swash.rs`.
    pub fn default_font() -> Font {
        Font::with_name("System Font")
    }

    pub fn default_font_bytes() -> Option<Vec<u8>> {
        std::fs::read("/System/Library/Fonts/SFNS.ttf").ok()
    }
}

pub use platform::{
    default_font, default_font_bytes, make_theme, normalize_path, primary_button, secondary_button,
};

#[cfg(not(target_os = "macos"))]
pub use platform::{HEADER_BG, HEADER_TEXT, header_style};
