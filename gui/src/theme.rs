use iced::Color;

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

pub const HEADER_BG: Color = Color::from_rgb(0.15, 0.35, 0.60);
pub const HEADER_TEXT: Color = Color::WHITE;

/// Normalize path separators for display on Windows.
pub fn normalize_path(path: &str) -> String {
    path.replace('/', "\\")
}
