//! Attach native `NSVisualEffectView`s behind winit's content view.
//!
//! Two effect surfaces, both parented to the window's private NSThemeFrame
//! and ordered below winit's contentView so winit's instance-vars are never
//! disturbed (replacing the contentView is what caused the
//! "uninitialized instance variable" panic in earlier attempts):
//!
//! 1. **Outer**, `.windowBackground` — fills the whole content frame; this
//!    is what shows through on the right side where there's no panel.
//! 2. **Sidebar inset panel**, `.sidebar` — rounded card on the left with a
//!    hairline border and a subtle drop shadow (wrapper-view pattern so the
//!    shadow can escape the rounded clip). Positioned tight to the
//!    top-left corner so it visually subsumes the traffic-light area.
//!
//! Right side has no panel — the iced content paints directly onto the
//! outer glass.

use std::sync::{Mutex, OnceLock};

use objc2::ClassType;
use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Id;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAppearance, NSAppearanceNameVibrantDark, NSAutoresizingMaskOptions, NSBezierPath, NSColor,
    NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindow, NSWindowButton, NSWindowOrderingMode, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{CGFloat, MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize};
use raw_window_handle::{RawWindowHandle, WindowHandle};

use crate::layout;

/// Wrap NSWindow so we can park it in a `Mutex` behind a `OnceLock`. AppKit
/// objects are main-thread-only; we uphold that by only reading this cell
/// from `tick_nudge`, which checks `MainThreadMarker::new()` first.
struct WindowCell(Id<NSWindow>);
unsafe impl Send for WindowCell {}
unsafe impl Sync for WindowCell {}

static WINDOW: OnceLock<Mutex<Option<WindowCell>>> = OnceLock::new();

fn window_cell() -> &'static Mutex<Option<WindowCell>> {
    WINDOW.get_or_init(|| Mutex::new(None))
}

/// Re-apply the traffic-light nudge. AppKit re-lays out the standard window
/// buttons on later layout passes and silently resets the frames we set in
/// `configure_window`, so we reapply the offset on every iced tick.
pub fn tick_nudge_traffic_lights() {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let Ok(guard) = window_cell().lock() else {
        return;
    };
    if let Some(cell) = guard.as_ref() {
        unsafe { nudge_traffic_lights(&cell.0, 10.0, -10.0) };
    }
}

/// Opaque CGColorRef. CoreGraphics types aren't part of objc2-app-kit's
/// generated bindings, so we declare transparent newtypes with the right
/// ObjC encoding so `msg_send!` can pass them through.
#[repr(transparent)]
#[derive(Copy, Clone)]
struct CGColorRef(*mut std::ffi::c_void);

unsafe impl Encode for CGColorRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGColor", &[]));
}

unsafe impl RefEncode for CGColorRef {
    const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
}

#[repr(transparent)]
#[derive(Copy, Clone)]
struct CGPathRef(*mut std::ffi::c_void);

unsafe impl Encode for CGPathRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("CGPath", &[]));
}

unsafe impl RefEncode for CGPathRef {
    const ENCODING_REF: Encoding = Encoding::Pointer(&<Self as Encode>::ENCODING);
}

#[repr(C)]
#[derive(Copy, Clone)]
struct CGSize {
    width: CGFloat,
    height: CGFloat,
}

unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[CGFloat::ENCODING, CGFloat::ENCODING]);
}

pub fn install(handle: &WindowHandle<'_>) -> bool {
    let raw = handle.as_raw();
    let RawWindowHandle::AppKit(app_kit) = raw else {
        return false;
    };

    let ns_view_ptr = app_kit.ns_view.as_ptr() as *mut NSView;

    // SAFETY: winit keeps the NSView alive for the lifetime of the window.
    unsafe {
        let Some(content_view) = ns_view_ptr.as_ref() else {
            return false;
        };
        let content: Id<NSView> = content_view.retain();

        let Some(window) = content.window() else {
            return false;
        };

        let Some(frame_view) = content.superview() else {
            return false;
        };

        // Idempotency: if any sibling of the content view is already an
        // NSVisualEffectView we placed, do nothing.
        let siblings = frame_view.subviews();
        for i in 0..siblings.count() {
            let view = siblings.objectAtIndex(i);
            if view.is_kind_of::<NSVisualEffectView>() {
                return true;
            }
        }

        configure_window(&window);
        // Keep the window reference so the iced tick can re-apply the
        // traffic-light nudge each frame (AppKit otherwise resets the frames
        // on subsequent layout passes).
        if let Ok(mut cell) = window_cell().lock() {
            *cell = Some(WindowCell(window.clone()));
        }

        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };

        let content_frame = content.frame();
        let win_size = (
            content_frame.size.width as f32,
            content_frame.size.height as f32,
        );
        let origin = (content_frame.origin.x as f32, content_frame.origin.y as f32);

        // ---- 1) Outer: full-window glass. Visible on the right side
        //         where there's no inset panel.
        let outer = NSVisualEffectView::new(mtm);
        outer.setMaterial(NSVisualEffectMaterial::WindowBackground);
        outer.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        outer.setState(NSVisualEffectState::Active);
        outer.setFrame(content_frame);
        outer.setAutoresizingMask(
            NSAutoresizingMaskOptions::NSViewWidthSizable
                | NSAutoresizingMaskOptions::NSViewHeightSizable,
        );
        let outer_view: &NSView = &outer;
        frame_view.addSubview_positioned_relativeTo(
            outer_view,
            NSWindowOrderingMode::NSWindowBelow,
            Some(&content),
        );

        // ---- 2) Sidebar inset panel: rounded card, top-left aligned.
        let (sx, sy, sw, sh) = layout::sidebar_panel_rect(win_size);
        install_inset_panel(
            mtm,
            &frame_view,
            &content,
            NSRect::new(
                NSPoint::new((origin.0 + sx) as CGFloat, (origin.1 + sy) as CGFloat),
                NSSize::new(sw as CGFloat, sh as CGFloat),
            ),
            NSAutoresizingMaskOptions::NSViewMaxXMargin
                | NSAutoresizingMaskOptions::NSViewHeightSizable,
            // `.Sidebar` — the same material macOS System Settings uses
            // for its sidebar split-view item. Translucent, lets the
            // desktop bleed through the blur.
            NSVisualEffectMaterial::Sidebar,
        );
    }

    true
}

/// NSVisualEffectView doesn't expose corner/mask properties itself; we
/// enable layer backing and set them on the CALayer via msg_send.
///
/// Also paints a hairline border using `NSColor.separatorColor` (a dynamic
/// color that resolves correctly in light/dark) so the panel reads as a
/// discrete card edge rather than blending into the window chrome.
unsafe fn apply_rounded_corners(view: &NSView, radius: CGFloat) {
    view.setWantsLayer(true);
    let layer: *mut AnyObject = msg_send![view, layer];
    if layer.is_null() {
        return;
    }
    let _: () = msg_send![layer, setCornerRadius: radius];
    let _: () = msg_send![layer, setMasksToBounds: true];
    let curve_str = objc2_foundation::NSString::from_str("continuous");
    let _: () = msg_send![layer, setCornerCurve: &*curve_str];

    let border_width: CGFloat = 1.0;
    let _: () = msg_send![layer, setBorderWidth: border_width];
    // `NSColor.separatorColor` resolves too subtly inside the forced
    // VibrantDark appearance of the sidebar effect view — hardcode a
    // light hairline so the panel edge is actually visible.
    let border_color = NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 0.18);
    let cg_color: CGColorRef = msg_send![&*border_color, CGColor];
    let _: () = msg_send![layer, setBorderColor: cg_color];
}

/// Build a single inset card: a wrapper `NSView` that carries the drop
/// shadow (its CALayer has `cornerRadius` + `shadowPath` but `masksToBounds`
/// is left off so the shadow can escape) containing an `NSVisualEffectView`
/// that does the actual blur and clips itself to the rounded shape.
///
/// Wrapper-view pattern is the canonical AppKit fix for the
/// shadow-vs-mask conflict: a single layer can't both clip its content
/// (`masksToBounds = true`) and emit a shadow outside its bounds, so we
/// split those responsibilities across two layers.
unsafe fn install_inset_panel(
    mtm: MainThreadMarker,
    frame_view: &NSView,
    content_anchor: &NSView,
    panel_frame: NSRect,
    autoresize_mask: NSAutoresizingMaskOptions,
    material: NSVisualEffectMaterial,
) {
    let wrapper = NSView::new(mtm);
    wrapper.setFrame(panel_frame);
    wrapper.setAutoresizingMask(autoresize_mask);
    wrapper.setWantsLayer(true);
    let wrapper_layer: *mut AnyObject = msg_send![&*wrapper, layer];
    if !wrapper_layer.is_null() {
        let radius: CGFloat = layout::PANEL_RADIUS as CGFloat;
        let _: () = msg_send![wrapper_layer, setCornerRadius: radius];
        let curve_str = objc2_foundation::NSString::from_str("continuous");
        let _: () = msg_send![wrapper_layer, setCornerCurve: &*curve_str];

        let shadow_opacity: f32 = 0.18;
        let _: () = msg_send![wrapper_layer, setShadowOpacity: shadow_opacity];
        let shadow_radius: CGFloat = 8.0;
        let _: () = msg_send![wrapper_layer, setShadowRadius: shadow_radius];
        let shadow_offset = CGSize {
            width: 0.0,
            height: -2.0,
        };
        let _: () = msg_send![wrapper_layer, setShadowOffset: shadow_offset];

        let bounds_rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(panel_frame.size.width, panel_frame.size.height),
        );
        let path =
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds_rect, radius, radius);
        let cg_path: CGPathRef = msg_send![&*path, CGPath];
        let _: () = msg_send![wrapper_layer, setShadowPath: cg_path];
    }

    let effect = NSVisualEffectView::new(mtm);
    effect.setMaterial(material);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    // Force the vibrant-dark appearance on the sidebar panel so the material
    // renders as its darker tinted variant regardless of the rest of the
    // window's appearance — that's how System Settings gets its sidebar's
    // extra tint relative to the content area.
    let dark = NSAppearance::appearanceNamed(NSAppearanceNameVibrantDark);
    if let Some(ap) = dark.as_ref() {
        let _: () = msg_send![&*effect, setAppearance: &**ap];
    }
    effect.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), panel_frame.size));
    effect.setAutoresizingMask(
        NSAutoresizingMaskOptions::NSViewWidthSizable
            | NSAutoresizingMaskOptions::NSViewHeightSizable,
    );
    apply_rounded_corners(&effect, layout::PANEL_RADIUS as CGFloat);
    let effect_view: &NSView = &effect;
    wrapper.addSubview(effect_view);

    let wrapper_view: &NSView = &wrapper;
    frame_view.addSubview_positioned_relativeTo(
        wrapper_view,
        NSWindowOrderingMode::NSWindowBelow,
        Some(content_anchor),
    );
}

unsafe fn configure_window(window: &NSWindow) {
    window.setOpaque(false);
    let clear = NSColor::clearColor();
    window.setBackgroundColor(Some(&clear));

    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::NSWindowTitleHidden);
    let mask = window.styleMask() | NSWindowStyleMask::FullSizeContentView;
    window.setStyleMask(mask);

    // System Settings shifts the traffic-light cluster down/right (~8pt each
    // axis) so the buttons sit nicely inside the sidebar inset panel rather
    // than hugging the window corner. Replicate that nudge by offsetting each
    // standard window button's frame. AppKit Y is bottom-up, so "down" is
    // negative dy.
    nudge_traffic_lights(window, 10.0, -10.0);
}

/// Target origins per button, captured on first successful nudge call.
/// Indexed 0=close, 1=miniaturize, 2=zoom. Once populated, subsequent ticks
/// force the button frame back to the captured target rather than offsetting
/// again — that's how we remain idempotent across AppKit relayouts.
static NUDGE_TARGETS: OnceLock<Mutex<[Option<(CGFloat, CGFloat)>; 3]>> = OnceLock::new();

fn nudge_targets() -> &'static Mutex<[Option<(CGFloat, CGFloat)>; 3]> {
    NUDGE_TARGETS.get_or_init(|| Mutex::new([None, None, None]))
}

unsafe fn nudge_traffic_lights(window: &NSWindow, dx: CGFloat, dy: CGFloat) {
    let mut targets = match nudge_targets().lock() {
        Ok(t) => t,
        Err(_) => return,
    };
    for (i, button) in [
        NSWindowButton::NSWindowCloseButton,
        NSWindowButton::NSWindowMiniaturizeButton,
        NSWindowButton::NSWindowZoomButton,
    ]
    .iter()
    .copied()
    .enumerate()
    {
        if let Some(btn) = window.standardWindowButton(button) {
            let view: &NSView = btn.as_ref();
            let _: () = msg_send![view, setTranslatesAutoresizingMaskIntoConstraints: true];
            view.setAutoresizingMask(
                NSAutoresizingMaskOptions::NSViewMaxXMargin
                    | NSAutoresizingMaskOptions::NSViewMinYMargin,
            );

            let current = view.frame();
            let (target_x, target_y) = match targets[i] {
                Some(t) => t,
                None => {
                    // Skip capturing a target from an unlaid-out button
                    // (frame.size == 0) — wait for AppKit to position it
                    // first, then we'll offset from the real default.
                    if current.size.width <= 0.0 || current.size.height <= 0.0 {
                        continue;
                    }
                    let t = (current.origin.x + dx, current.origin.y + dy);
                    targets[i] = Some(t);
                    t
                }
            };

            if current.origin.x != target_x || current.origin.y != target_y {
                view.setFrame(NSRect::new(NSPoint::new(target_x, target_y), current.size));
            }
        }
    }
}
