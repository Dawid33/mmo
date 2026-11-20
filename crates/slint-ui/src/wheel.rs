//! The fortress tool wheel's open/pick/close rules, apart from any engine
//! so they test cheaply. The client feeds one [`WheelInput`] per frame
//! (`crates/client/src/renderer/slint_overlay.rs`) and the `RadialMenu` in
//! `ui/game.slint` reports what is under the pointer.
//!
//! Holding the wheel key (Q) opens it at the pointer. Released over a
//! wedge, the key picks that wedge; released in the middle, the wheel stays
//! open for a click. A left click on a wedge picks it at any time, and in
//! the middle closes a wheel left open. A right press, or the key again on
//! a wheel left open, closes it; Esc closes it too (the client's screen
//! handling). A key, not Alt + click: desktops such as Cinnamon, Xfce and
//! KDE take Alt + click for moving and resizing windows.

/// Whether the wheel is open, and how.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WheelPhase {
    #[default]
    Closed,
    /// Open, the key still held from opening it.
    Held,
    /// Open after the key came up in the middle: waits for a click.
    Sticky,
}

impl WheelPhase {
    pub fn is_open(self) -> bool {
        self != WheelPhase::Closed
    }
}

/// One frame's input, as the wheel sees it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WheelInput {
    /// The key went down where the wheel may open.
    pub open_press: bool,
    /// The key went down, and came up.
    pub hold_press: bool,
    pub hold_release: bool,
    /// The right button went down.
    pub cancel_press: bool,
    pub left_release: bool,
    /// The wedge under the pointer, -1 for none (the middle, or disabled).
    pub hovered: i32,
}

/// The phase after `input`, and the wedge picked, if any.
pub fn step(phase: WheelPhase, input: WheelInput) -> (WheelPhase, Option<usize>) {
    let picked = usize::try_from(input.hovered).ok();
    match phase {
        WheelPhase::Closed if input.open_press => (WheelPhase::Held, None),
        WheelPhase::Closed => (WheelPhase::Closed, None),
        _ if input.cancel_press => (WheelPhase::Closed, None),
        _ if input.left_release && picked.is_some() => (WheelPhase::Closed, picked),
        WheelPhase::Held if input.hold_release => match picked {
            Some(i) => (WheelPhase::Closed, Some(i)),
            None => (WheelPhase::Sticky, None),
        },
        WheelPhase::Held => (WheelPhase::Held, None),
        WheelPhase::Sticky if input.hold_press || input.left_release => (WheelPhase::Closed, None),
        WheelPhase::Sticky => (WheelPhase::Sticky, None),
    }
}
