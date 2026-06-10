use iced::widget::{button, container};
use iced::{Border, Color, Font, Theme};

// --- Shared metrics ---

pub const WINDOW_WIDTH: f32 = 800.0;
pub const WINDOW_HEIGHT: f32 = 500.0;
pub const PADDING: f32 = 30.0;
pub const SPACING: f32 = 16.0;

// Font sizes are tuned to native-ish expectations on each platform.
// macOS uses Apple's typographic scale, bumped a hair above the strict Tahoe
// spec so installer copy reads comfortably at viewing distance. Windows keeps
// the heavier Win11 sizing it already had.
#[cfg(target_os = "macos")]
pub const FONT_TITLE: f32 = 26.0;
#[cfg(target_os = "macos")]
pub const FONT_HEADLINE: f32 = 17.0;
#[cfg(target_os = "macos")]
pub const FONT_BODY: f32 = 14.0;
#[cfg(target_os = "macos")]
pub const FONT_SECONDARY: f32 = 12.0;
#[cfg(target_os = "macos")]
pub const FONT_LOG: f32 = 12.0;

#[cfg(not(target_os = "macos"))]
pub const FONT_TITLE: f32 = 24.0;
#[cfg(not(target_os = "macos"))]
pub const FONT_HEADLINE: f32 = 18.0;
#[cfg(not(target_os = "macos"))]
pub const FONT_BODY: f32 = 16.0;
#[cfg(not(target_os = "macos"))]
pub const FONT_SECONDARY: f32 = 14.0;
#[cfg(not(target_os = "macos"))]
pub const FONT_LOG: f32 = 12.0;

// Header-bar metrics are Windows-only — macOS uses the native title bar.
#[cfg(not(target_os = "macos"))]
pub const HEADER_HEIGHT: f32 = 60.0;
#[cfg(not(target_os = "macos"))]
pub const FONT_HEADER: f32 = 20.0;

// --- Windows 11 appearance ---

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub const HEADER_BG: Color = Color::from_rgb(0.0, 0.471, 0.831); // #0078D4
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

    pub fn secondary_button_focused(_theme: &Theme, status: button::Status) -> button::Style {
        let focused_border = Border {
            radius: 4.0.into(),
            width: 2.0,
            color: Color::from_rgb(0.0, 0.471, 0.831),
        };
        let base = button::Style {
            background: Some(Color::from_rgb(0.898, 0.898, 0.898).into()),
            text_color: Color::from_rgb(0.122, 0.122, 0.122),
            border: focused_border,
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
                    width: 2.0,
                    color: Color::from_rgb(0.600, 0.600, 0.600),
                },
                shadow: Default::default(),
                snap: false,
            },
        }
    }

    // On Windows, every "important" button is just the same Windows 11 rectangle.
    // Primary and secondary are visually identical — focus indicates the default.
    pub fn primary_button(theme: &Theme, status: button::Status) -> button::Style {
        secondary_button(theme, status)
    }

    pub fn primary_button_focused(theme: &Theme, status: button::Status) -> button::Style {
        secondary_button_focused(theme, status)
    }

    pub fn focus_ring(_theme: &Theme) -> container::Style {
        container::Style {
            border: Border {
                radius: 2.0.into(),
                width: 2.0,
                color: Color::from_rgb(0.0, 0.471, 0.831),
            },
            ..Default::default()
        }
    }

    pub fn no_focus_ring(_theme: &Theme) -> container::Style {
        container::Style {
            border: Border {
                radius: 2.0.into(),
                width: 2.0,
                color: Color::TRANSPARENT,
            },
            ..Default::default()
        }
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

    pub fn next_label() -> &'static str {
        "Next >"
    }
    pub fn back_label() -> &'static str {
        "< Back"
    }

    pub fn default_font() -> Font {
        Font::DEFAULT
    }

    pub fn default_font_bytes() -> Option<Vec<u8>> {
        None
    }

    pub fn semibold_font() -> Font {
        Font {
            weight: iced::font::Weight::Semibold,
            ..default_font()
        }
    }

    pub fn bold_font() -> Font {
        Font {
            weight: iced::font::Weight::Bold,
            ..default_font()
        }
    }
}

// --- macOS appearance (Aqua, light/dark) ---

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
        text: Color,
        muted: Color,
        primary: Color,
        primary_hover: Color,
        primary_pressed: Color,
        primary_text: Color,
        border: Color,
        /// Opaque-ish fill for the right-hand content pane — sits over the
        /// NSVisualEffectView so body text stays readable. Alpha < 1 lets a
        /// hint of the glass bleed through.
        content_bg: Color,
        secondary_bg: Color,
        secondary_hover: Color,
        secondary_pressed: Color,
    }

    fn palette() -> &'static Palette {
        static CACHED: OnceLock<Palette> = OnceLock::new();
        CACHED.get_or_init(|| {
            if dark_mode() {
                Palette {
                    text: from_hex(0xF2, 0xF2, 0xF7),
                    muted: from_hex(0x98, 0x98, 0xA0),
                    primary: from_hex(0x2E, 0x82, 0xFF),
                    primary_hover: from_hex(0x4F, 0x97, 0xFF),
                    primary_pressed: from_hex(0x1E, 0x6C, 0xE1),
                    primary_text: Color::WHITE,
                    border: Color {
                        a: 0.25,
                        ..from_hex(0x8E, 0x8E, 0x93)
                    },
                    content_bg: Color {
                        a: 0.88,
                        ..from_hex(0x1E, 0x1E, 0x20)
                    },
                    secondary_bg: Color {
                        a: 0.55,
                        ..from_hex(0x48, 0x48, 0x4A)
                    },
                    secondary_hover: Color {
                        a: 0.72,
                        ..from_hex(0x5A, 0x5A, 0x5C)
                    },
                    secondary_pressed: Color {
                        a: 0.45,
                        ..from_hex(0x3A, 0x3A, 0x3C)
                    },
                }
            } else {
                Palette {
                    text: from_hex(0x1C, 0x1C, 0x1E),
                    muted: from_hex(0x6E, 0x6E, 0x73),
                    primary: from_hex(0x00, 0x63, 0xE1),
                    primary_hover: from_hex(0x1E, 0x79, 0xEF),
                    primary_pressed: from_hex(0x00, 0x4C, 0xB8),
                    primary_text: Color::WHITE,
                    border: Color {
                        a: 0.22,
                        ..from_hex(0x3A, 0x3A, 0x3C)
                    },
                    content_bg: Color {
                        a: 0.94,
                        ..Color::WHITE
                    },
                    secondary_bg: Color {
                        a: 0.70,
                        ..Color::WHITE
                    },
                    secondary_hover: Color {
                        a: 0.90,
                        ..Color::WHITE
                    },
                    secondary_pressed: Color {
                        a: 0.55,
                        ..from_hex(0xE5, 0xE5, 0xE5)
                    },
                }
            }
        })
    }

    // Exposed so sidebar.rs and the app shell can color text against the bg.
    pub fn text_color() -> Color {
        palette().text
    }
    pub fn muted_text_color() -> Color {
        palette().muted
    }

    pub fn make_theme() -> Theme {
        let p = palette();
        let name = if dark_mode() {
            "Liquid Glass Dark"
        } else {
            "Liquid Glass"
        };
        // Window canvas is fully transparent so the NSVisualEffectView
        // attached behind the iced layer shows through everywhere we don't
        // explicitly paint.
        Theme::custom(
            String::from(name),
            iced::theme::Palette {
                background: Color::TRANSPARENT,
                text: p.text,
                primary: p.primary,
                success: from_hex(0x30, 0xD1, 0x58),
                warning: from_hex(0xFF, 0x9F, 0x0A),
                danger: from_hex(0xFF, 0x45, 0x3A),
            },
        )
    }

    // Liquid Glass capsule buttons: full-pill radius, soft drop shadow beneath
    // so the button "floats" over the canvas rather than sitting flush.
    // tiny-skia can't do real material blur/refraction, so we lean on shape,
    // shadow, and subtle translucency to imply glass.
    const BUTTON_RADIUS: f32 = 16.0;

    // Shadows sit on top of the NSVisualEffectView, so keep them feather-light —
    // a real Tahoe button's "lift" comes from material lensing, not a drop shadow.
    fn button_shadow(alpha: f32) -> iced::Shadow {
        iced::Shadow {
            color: Color {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: alpha,
            },
            offset: iced::Vector::new(0.0, 0.5),
            blur_radius: 3.0,
        }
    }

    pub fn primary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let p = palette();
        // Glass-tinted primary: accent blue at reduced alpha so the window's
        // NSVisualEffectView lenses through the pill. Tahoe-style.
        let tint = |alpha: f32, color: Color| Color { a: alpha, ..color };
        let base = button::Style {
            background: Some(tint(0.88, p.primary).into()),
            text_color: p.primary_text,
            border: Border {
                radius: BUTTON_RADIUS.into(),
                width: 0.5,
                color: tint(0.35, Color::WHITE),
            },
            shadow: button_shadow(0.06),
            snap: false,
        };
        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(tint(0.96, p.primary_hover).into()),
                shadow: button_shadow(0.10),
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(tint(0.75, p.primary_pressed).into()),
                shadow: button_shadow(0.04),
                ..base
            },
            button::Status::Disabled => button::Style {
                background: Some(tint(0.30, p.primary).into()),
                text_color: Color {
                    a: 0.6,
                    ..p.primary_text
                },
                shadow: Default::default(),
                ..base
            },
        }
    }

    pub fn primary_button_focused(_theme: &Theme, status: button::Status) -> button::Style {
        // Keyboard-focus ring: a thicker accent halo outside the pill, matched
        // to the system accent at reduced alpha.
        let mut s = primary_button(_theme, status);
        s.border.width = 3.0;
        s.border.color = Color {
            a: 0.55,
            ..palette().primary
        };
        s
    }

    pub fn secondary_button(_theme: &Theme, status: button::Status) -> button::Style {
        let p = palette();
        // Proper glass capsule — fill is almost invisible; the NSVisualEffectView
        // behind the button bar provides the actual material. The subtle white
        // border fakes the specular highlight Tahoe renders on real glass
        // pills.
        let tint = |alpha: f32, color: Color| Color { a: alpha, ..color };
        let base_fill = if dark_mode() {
            tint(0.22, Color::WHITE)
        } else {
            tint(0.38, Color::WHITE)
        };
        let base = button::Style {
            background: Some(base_fill.into()),
            text_color: p.text,
            border: Border {
                radius: BUTTON_RADIUS.into(),
                width: 0.5,
                color: if dark_mode() {
                    tint(0.18, Color::WHITE)
                } else {
                    tint(0.28, Color::BLACK)
                },
            },
            shadow: button_shadow(0.04),
            snap: false,
        };
        match status {
            button::Status::Active => base,
            button::Status::Hovered => button::Style {
                background: Some(
                    if dark_mode() {
                        tint(0.34, Color::WHITE)
                    } else {
                        tint(0.55, Color::WHITE)
                    }
                    .into(),
                ),
                shadow: button_shadow(0.06),
                ..base
            },
            button::Status::Pressed => button::Style {
                background: Some(
                    if dark_mode() {
                        tint(0.14, Color::WHITE)
                    } else {
                        tint(0.26, Color::WHITE)
                    }
                    .into(),
                ),
                shadow: button_shadow(0.02),
                ..base
            },
            button::Status::Disabled => button::Style {
                background: Some(tint(0.10, Color::WHITE).into()),
                text_color: Color { a: 0.4, ..p.text },
                shadow: Default::default(),
                ..base
            },
        }
    }

    pub fn secondary_button_focused(_theme: &Theme, status: button::Status) -> button::Style {
        let mut s = secondary_button(_theme, status);
        s.border.width = 3.0;
        s.border.color = Color {
            a: 0.55,
            ..palette().primary
        };
        s
    }

    pub fn focus_ring(_theme: &Theme) -> container::Style {
        container::Style {
            border: Border {
                radius: 6.0.into(),
                width: 3.0,
                color: Color {
                    a: 0.6,
                    ..palette().primary
                },
            },
            ..Default::default()
        }
    }

    pub fn no_focus_ring(_theme: &Theme) -> container::Style {
        container::Style {
            border: Border {
                radius: 6.0.into(),
                width: 3.0,
                color: Color::TRANSPARENT,
            },
            ..Default::default()
        }
    }

    pub fn sidebar_style(_theme: &Theme) -> container::Style {
        // Transparent. The sidebar's tint comes from the underlying
        // NSVisualEffectView material + forced VibrantDark appearance in
        // `glass.rs`, not an iced overlay.
        container::Style::default()
    }

    /// Whole-window glass: the content pane has no fill at all — the
    /// NSVisualEffectView behind the iced layer provides the material, and
    /// the text is rendered at full opacity on top. Relies on AppKit's
    /// vibrant material to keep body text legible.
    pub fn content_pane_style(_theme: &Theme) -> container::Style {
        container::Style::default()
    }

    pub fn sidebar_item_active(_theme: &Theme) -> container::Style {
        // Neutral dim pill — the sidebar items aren't clickable (wizard
        // position indicator, not navigation) so accent blue was reading
        // as "click me". A low-alpha neutral overlay signals "this is the
        // current step" without implying an interactive affordance.
        let fill = if dark_mode() {
            Color::from_rgba(1.0, 1.0, 1.0, 0.10)
        } else {
            Color::from_rgba(0.0, 0.0, 0.0, 0.08)
        };
        container::Style {
            background: Some(fill.into()),
            border: Border {
                radius: 9.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    pub fn accent_color() -> Color {
        palette().primary
    }

    pub fn normalize_path(path: &str) -> String {
        path.to_string()
    }

    pub fn next_label() -> &'static str {
        "Continue"
    }
    pub fn back_label() -> &'static str {
        "Go Back"
    }

    /// Apple's SF Pro (variable font). Shipped on every macOS host at
    /// `/System/Library/Fonts/SFNS.ttf`; registers in fontdb under the
    /// family "System Font". Requires the vendored cosmic-text fix
    /// (`normalized_coords`) so bold rendering actually picks up the
    /// wght axis — see `vendor/cosmic-text/src/swash.rs`.
    ///
    /// Default weight is Medium (500) rather than Normal (400): SF Pro
    /// Regular reads noticeably thinner than macOS's UI conventions
    /// suggest at body sizes, and the whole app looks washed out with it.
    pub fn default_font() -> Font {
        Font {
            weight: iced::font::Weight::Medium,
            ..Font::with_name("System Font")
        }
    }

    pub fn default_font_bytes() -> Option<Vec<u8>> {
        std::fs::read("/System/Library/Fonts/SFNS.ttf").ok()
    }

    pub fn semibold_font() -> Font {
        Font {
            weight: iced::font::Weight::Semibold,
            ..default_font()
        }
    }

    pub fn bold_font() -> Font {
        Font {
            weight: iced::font::Weight::Bold,
            ..default_font()
        }
    }
}

// --- Public API (platform-neutral facade) ---

pub use platform::{
    back_label, default_font, default_font_bytes, focus_ring, make_theme, next_label,
    no_focus_ring, normalize_path, primary_button, primary_button_focused, secondary_button,
    secondary_button_focused, semibold_font,
};

#[allow(dead_code)]
pub use platform::bold_font;

#[cfg(not(target_os = "macos"))]
pub use platform::{HEADER_BG, HEADER_TEXT, header_style};

#[cfg(target_os = "macos")]
pub use platform::{
    accent_color, content_pane_style, muted_text_color, sidebar_item_active, sidebar_style,
    text_color,
};
