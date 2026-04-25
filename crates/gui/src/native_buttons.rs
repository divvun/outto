//! Native `NSButton`s overlaid on the installer window so the button bar can
//! use the real macOS 26 "Liquid Glass" bezel — lensing, specular highlights,
//! vibrant adaptive text, and all the interactive behaviors the OS provides
//! that we cannot reproduce with iced's tiny-skia renderer.
//!
//! - Up to 3 slots (left → right). iced drives the labels and semantic action
//!   by pushing a new `Layout` each time the wizard step changes.
//! - Clicks are routed out via a lock-free queue; iced polls it on every tick
//!   and translates them into normal `Message` variants.
//! - Bezel style is the raw `NSBezelStyle::Glass = 16` (macOS 26 API). On
//!   older OSes AppKit falls back to the default push bezel with no fuss.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use objc2::declare_class;
use objc2::mutability::MainThreadOnly;
use objc2::rc::{Allocated, Id};
use objc2::runtime::{NSObject, Sel};
use objc2::{msg_send, msg_send_id, sel, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBezelStyle, NSButton, NSColor, NSControl, NSControlSize, NSView,
};
use objc2_foundation::{CGFloat, MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use raw_window_handle::{RawWindowHandle, WindowHandle};

/// Semantic identifiers for each button slot. The iced side maps these back
/// to concrete `Message` values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonAction {
    Next,
    Prev,
    Cancel,
    StartInstall,
    StartUninstall,
    Finish,
}

#[derive(Clone, Debug)]
pub struct ButtonSpec {
    pub label: String,
    pub primary: bool,
    pub enabled: bool,
    pub action: ButtonAction,
}

/// The layout of the whole button bar for a single wizard step.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub buttons: Vec<ButtonSpec>,
}

struct State {
    // Concrete NSButton instances, kept alive via `Retained`.
    buttons: Vec<Id<NSButton>>,
    // Semantic action per live button, index-matched to `buttons`.
    actions: Vec<ButtonAction>,
    // ObjC target object receiving button clicks.
    _target: Id<ButtonTarget>,
    // Parent view the buttons are installed on (the window's frame view).
    parent: Id<NSView>,
}

/// AppKit objects are main-thread-only. We promise to only read/write this
/// cell from the main thread — `install` / `apply` / `clear` all check with
/// `MainThreadMarker::new()` before touching anything.
struct MainThreadState(State);
unsafe impl Send for MainThreadState {}
unsafe impl Sync for MainThreadState {}

/// Singleton — only one installer window, so one state slot is enough.
static INSTALLED: OnceLock<Mutex<Option<MainThreadState>>> = OnceLock::new();

fn state_cell() -> &'static Mutex<Option<MainThreadState>> {
    INSTALLED.get_or_init(|| Mutex::new(None))
}

/// Click queue populated by ObjC, drained by iced's subscription.
static CLICKS: OnceLock<Mutex<VecDeque<ButtonAction>>> = OnceLock::new();

fn clicks() -> &'static Mutex<VecDeque<ButtonAction>> {
    CLICKS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn push_click(action: ButtonAction) {
    if let Ok(mut q) = clicks().lock() {
        q.push_back(action);
    }
}

/// Drain any pending clicks. Safe to call from the iced update thread.
pub fn drain_clicks() -> Vec<ButtonAction> {
    match clicks().lock() {
        Ok(mut q) => q.drain(..).collect(),
        Err(_) => Vec::new(),
    }
}

// ---- ObjC target class ------------------------------------------------

declare_class!(
    struct ButtonTarget;

    unsafe impl ClassType for ButtonTarget {
        type Super = NSObject;
        type Mutability = MainThreadOnly;
        const NAME: &'static str = "OuttoNativeButtonTarget";
    }

    impl DeclaredClass for ButtonTarget {}

    unsafe impl ButtonTarget {
        #[method_id(init)]
        fn init(this: Allocated<Self>) -> Option<Id<Self>> {
            let this = this.set_ivars(());
            unsafe { msg_send_id![super(this), init] }
        }

        #[method(onClicked:)]
        fn on_clicked(&self, sender: &NSButton) {
            let tag = unsafe { sender.tag() };
            if let Ok(guard) = state_cell().lock() {
                if let Some(s) = guard.as_ref() {
                    if let Some(action) = s.0.actions.get(tag as usize).copied() {
                        push_click(action);
                    }
                }
            }
        }
    }
);

impl ButtonTarget {
    fn new(mtm: MainThreadMarker) -> Id<Self> {
        let alloc = mtm.alloc();
        unsafe { msg_send_id![alloc, init] }
    }
}

// ---- Public API -------------------------------------------------------

/// Install the native button overlay. Call after the window has opened and
/// the glass effect view is in place. Subsequent calls no-op.
pub fn install(handle: &WindowHandle<'_>) -> bool {
    let raw = handle.as_raw();
    let RawWindowHandle::AppKit(app_kit) = raw else {
        return false;
    };
    let ns_view_ptr = app_kit.ns_view.as_ptr() as *mut NSView;

    // SAFETY: winit owns the NSView for the window's lifetime.
    unsafe {
        let Some(content_view) = ns_view_ptr.as_ref() else {
            return false;
        };
        // Put the buttons on winit's content view. AppKit's Liquid Glass
        // rendering seems to check the view hierarchy — buttons on
        // NSThemeFrame (private superview) fall back to legacy push style,
        // whereas subviews of the content view render with the proper
        // glass material.
        let parent: Id<NSView> = content_view.retain();
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };

        let mut guard = state_cell().lock().unwrap();
        if guard.is_some() {
            return true;
        }

        let target = ButtonTarget::new(mtm);
        *guard = Some(MainThreadState(State {
            buttons: Vec::new(),
            actions: Vec::new(),
            _target: target,
            parent,
        }));
    }

    true
}

const BUTTON_WIDTH: CGFloat = 108.0;
const BUTTON_HEIGHT: CGFloat = 30.0;
const BUTTON_SPACING: CGFloat = 10.0;

// Buttons sit in the bottom-right of the content area (right of the sidebar).
// `CONTENT_INSET_*` matches the iced content padding so they line up with
// whatever screen content is rendered above.
const BAR_PADDING_RIGHT: CGFloat = crate::layout::CONTENT_INSET_X as CGFloat;
const BAR_PADDING_BOTTOM: CGFloat = crate::layout::CONTENT_INSET_Y as CGFloat;

/// Install / update / tear down buttons to match the requested layout.
/// Call from iced's `update` on each wizard-step transition. Safe to call
/// before `install` — it's a no-op then.
pub fn apply(layout: Layout) {
    let mut guard = match state_cell().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(state) = guard.as_mut().map(|s| &mut s.0) else {
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    unsafe {
        // Grow/shrink the button pool to match the layout count.
        while state.buttons.len() < layout.buttons.len() {
            let idx = state.buttons.len() as isize;
            // Use AppKit's factory so the button comes configured as a
            // standard push-style button (bordered, momentary-light), which
            // is a prerequisite for `NSBezelStyle::Glass` to render as the
            // new macOS 26 Liquid Glass pill. Initializing a bare NSButton
            // and only calling setBezelStyle doesn't propagate correctly.
            let placeholder = NSString::from_str("");
            let button: Id<NSButton> = NSButton::buttonWithTitle_target_action(
                &placeholder,
                Some(state._target.as_ref().as_ref()),
                Some(sel!(onClicked:)),
                mtm,
            );
            button.setBordered(true);
            // NSBezelStyle::Glass (raw 16) — macOS 26 Liquid Glass. Rendered
            // as Tahoe capsule pills when the app is linked against the 26
            // SDK and the button is in the content-view hierarchy (parenting
            // on NSThemeFrame falls back to legacy push style).
            button.setBezelStyle(NSBezelStyle(16));
            button.setControlSize(NSControlSize::Large);
            button.setTag(idx);
            state.parent.addSubview(&button);
            state.buttons.push(button);
        }
        while state.buttons.len() > layout.buttons.len() {
            if let Some(btn) = state.buttons.pop() {
                btn.removeFromSuperview();
            }
            state.actions.pop();
        }
        while state.actions.len() < layout.buttons.len() {
            state.actions.push(ButtonAction::Cancel);
        }

        // Lay out from the right edge: last button in the spec sits furthest right.
        // winit flips its contentView so iced can use a top-down coord system —
        // we have to mirror the Y anchor when the parent is flipped.
        let parent_bounds = state.parent.bounds();
        let flipped = state.parent.isFlipped();
        let bar_right = parent_bounds.size.width - BAR_PADDING_RIGHT;
        let bar_y = if flipped {
            parent_bounds.size.height - BAR_PADDING_BOTTOM - BUTTON_HEIGHT
        } else {
            BAR_PADDING_BOTTOM
        };

        for (i, (spec, button)) in layout.buttons.iter().zip(state.buttons.iter()).enumerate() {
            let from_right = (layout.buttons.len() - 1 - i) as CGFloat;
            let x = bar_right - BUTTON_WIDTH - from_right * (BUTTON_WIDTH + BUTTON_SPACING);
            let frame = NSRect::new(
                NSPoint::new(x, bar_y),
                NSSize::new(BUTTON_WIDTH, BUTTON_HEIGHT),
            );
            button.setFrame(frame);
            button.setAutoresizingMask(
                NSAutoresizingMaskOptions::NSViewMinXMargin
                    | NSAutoresizingMaskOptions::NSViewMaxYMargin,
            );

            let title = NSString::from_str(&spec.label);
            button.setTitle(&title);
            button.setEnabled(spec.enabled);
            button.setHidden(false);

            // Primary actions get `bezelColor = controlAccentColor` so AppKit
            // renders the `glassProminent` variant.
            if spec.primary {
                let accent = NSColor::controlAccentColor();
                button.setBezelColor(Some(&accent));
            } else {
                button.setBezelColor(None);
            }

            state.actions[i] = spec.action;
        }
    }
}

/// Wipe all native buttons from the window. Call when the window is closing
/// or on uninstall routes that don't use a button bar.
#[allow(dead_code)]
pub fn clear() {
    let mut guard = match state_cell().lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let Some(state) = guard.as_mut().map(|s| &mut s.0) else {
        return;
    };
    unsafe {
        while let Some(btn) = state.buttons.pop() {
            btn.removeFromSuperview();
        }
    }
    state.actions.clear();
}
