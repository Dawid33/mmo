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

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> WheelInput {
        WheelInput { hovered: -1, ..Default::default() }
    }

    #[test]
    fn closed_opens_on_the_hold_key() {
        let open = WheelInput { open_press: true, hold_press: true, ..input() };
        assert_eq!(step(WheelPhase::Closed, open), (WheelPhase::Held, None));
    }

    #[test]
    fn closed_ignores_clicks_and_a_key_it_may_not_open_on() {
        for i in [
            WheelInput { hold_press: true, ..input() },
            WheelInput { cancel_press: true, ..input() },
            WheelInput { hold_release: true, hovered: 2, ..input() },
            WheelInput { left_release: true, hovered: 2, ..input() },
        ] {
            assert_eq!(step(WheelPhase::Closed, i), (WheelPhase::Closed, None));
        }
    }

    #[test]
    fn held_release_over_a_wedge_picks_it() {
        let release = WheelInput { hold_release: true, hovered: 3, ..input() };
        assert_eq!(step(WheelPhase::Held, release), (WheelPhase::Closed, Some(3)));
    }

    #[test]
    fn held_release_in_the_middle_keeps_it_open() {
        let release = WheelInput { hold_release: true, ..input() };
        assert_eq!(step(WheelPhase::Held, release), (WheelPhase::Sticky, None));
    }

    #[test]
    fn held_left_click_on_a_wedge_picks_it() {
        let click = WheelInput { left_release: true, hovered: 1, ..input() };
        assert_eq!(step(WheelPhase::Held, click), (WheelPhase::Closed, Some(1)));
    }

    #[test]
    fn held_left_click_in_the_middle_does_nothing() {
        let click = WheelInput { left_release: true, ..input() };
        assert_eq!(step(WheelPhase::Held, click), (WheelPhase::Held, None));
    }

    #[test]
    fn held_right_press_closes() {
        let press = WheelInput { cancel_press: true, hovered: 2, ..input() };
        assert_eq!(step(WheelPhase::Held, press), (WheelPhase::Closed, None));
    }

    #[test]
    fn sticky_left_click_on_a_wedge_picks_it() {
        let click = WheelInput { left_release: true, hovered: 0, ..input() };
        assert_eq!(step(WheelPhase::Sticky, click), (WheelPhase::Closed, Some(0)));
    }

    #[test]
    fn sticky_left_click_in_the_middle_closes() {
        let click = WheelInput { left_release: true, ..input() };
        assert_eq!(step(WheelPhase::Sticky, click), (WheelPhase::Closed, None));
    }

    #[test]
    fn sticky_closes_on_a_right_press_or_the_key_again() {
        for i in [WheelInput { cancel_press: true, hovered: 2, ..input() }, WheelInput { hold_press: true, ..input() }]
        {
            assert_eq!(step(WheelPhase::Sticky, i), (WheelPhase::Closed, None));
        }
    }
}
