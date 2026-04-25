//! Shared layout constants for the macOS window chrome.
//!
//! Layout: a single rounded sidebar inset panel hugging the top-left
//! corner of the window (subsuming the traffic-light area), and a
//! content area on the right with no separate panel — the right side
//! paints onto the outer window glass directly.

/// Margin between the sidebar inset panel and the window edges.
/// Small enough that the panel reads as anchored to the top-left,
/// big enough that its rounded corners don't fight the window's own
/// corner rounding.
pub const PANEL_MARGIN: f32 = 8.0;

/// Corner radius of the sidebar inset panel.
pub const PANEL_RADIUS: f32 = 12.0;

/// Width of the sidebar inset panel.
pub const SIDEBAR_WIDTH: f32 = 188.0;

/// Internal top padding inside the iced sidebar so its first row clears
/// the traffic-light buttons that float over the top-left corner.
pub const SIDEBAR_TOP_INSET: f32 = 36.0;

/// Internal horizontal padding around the content area on the right
/// (left of the content, right of the window edge).
pub const CONTENT_INSET_X: f32 = 24.0;

/// Internal vertical padding around the content area (top + bottom).
pub const CONTENT_INSET_Y: f32 = 24.0;

/// Sidebar inset panel rect in content-view coordinates. AppKit Y is
/// bottom-up: origin is bottom-left, so `y = PANEL_MARGIN` and the
/// panel extends from there to `h - PANEL_MARGIN`.
pub fn sidebar_panel_rect(window: (f32, f32)) -> (f32, f32, f32, f32) {
    let (_w, h) = window;
    (
        PANEL_MARGIN,
        PANEL_MARGIN,
        SIDEBAR_WIDTH,
        h - 2.0 * PANEL_MARGIN,
    )
}
