//! Decides which mouse messages the game must not see.
//!
//! When the cursor is over one of Blish's controls, a click belongs to Blish and has to be kept
//! from the game underneath. The subtle part is that a press and its release must be treated
//! as one unit: handing the game a button-up it never saw a button-down for desyncs its input
//! state. With the right button that is especially bad — the game tries to leave a camera look
//! it never entered and restores a stale cursor position, flinging the pointer into a corner of
//! the screen. So blocking is decided at the press, and the matching release follows that
//! decision even if the cursor has left the overlay in between.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::protocol::wm;

/// A mouse button whose presses are routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Button {
    /// The left button.
    Left,
    /// The right button.
    Right,
    /// The middle button.
    Middle,
}

impl Button {
    const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Middle => 2,
        }
    }
}

/// The mouse messages this library cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMessage {
    /// The cursor moved.
    Move,
    /// A button went down.
    Press(Button),
    /// A button came up.
    Release(Button),
    /// The wheel turned.
    Wheel,
}

impl MouseMessage {
    /// Classifies a Win32 message id, or returns `None` for anything that is not a mouse
    /// message we forward.
    pub const fn from_wm(message: u32) -> Option<Self> {
        Some(match message {
            wm::MOUSEMOVE => Self::Move,
            wm::LBUTTONDOWN => Self::Press(Button::Left),
            wm::LBUTTONUP => Self::Release(Button::Left),
            wm::RBUTTONDOWN => Self::Press(Button::Right),
            wm::RBUTTONUP => Self::Release(Button::Right),
            wm::MBUTTONDOWN => Self::Press(Button::Middle),
            wm::MBUTTONUP => Self::Release(Button::Middle),
            wm::MOUSEWHEEL => Self::Wheel,
            _ => return None,
        })
    }
}

/// Tracks which button presses were kept from the game, so their releases are too.
///
/// Uses atomics so it can live in a `static`; in practice it is only touched from the game's
/// window procedure.
#[derive(Debug)]
pub struct ClickRouter {
    blocked_presses: [AtomicBool; 3],
}

impl ClickRouter {
    /// A router with no presses in flight.
    pub const fn new() -> Self {
        Self {
            blocked_presses: [
                AtomicBool::new(false),
                AtomicBool::new(false),
                AtomicBool::new(false),
            ],
        }
    }

    /// Whether `message` should be kept from the game.
    ///
    /// `over_overlay` is whether the cursor is currently over one of Blish's controls. It only
    /// matters for presses and the wheel: a release follows whatever its press was decided.
    /// Movement always reaches the game, so it never loses track of the cursor.
    pub fn should_block(&self, message: MouseMessage, over_overlay: bool) -> bool {
        match message {
            MouseMessage::Move => false,
            MouseMessage::Wheel => over_overlay,
            MouseMessage::Press(button) => {
                self.blocked_presses[button.index()].store(over_overlay, Ordering::Relaxed);
                over_overlay
            }
            MouseMessage::Release(button) => {
                self.blocked_presses[button.index()].swap(false, Ordering::Relaxed)
            }
        }
    }
}

impl Default for ClickRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{Button::*, MouseMessage::*, *};

    #[test]
    fn classifies_forwarded_messages() {
        assert_eq!(MouseMessage::from_wm(wm::MOUSEMOVE), Some(Move));
        assert_eq!(MouseMessage::from_wm(wm::RBUTTONDOWN), Some(Press(Right)));
        assert_eq!(MouseMessage::from_wm(wm::MBUTTONUP), Some(Release(Middle)));
        assert_eq!(MouseMessage::from_wm(wm::MOUSEWHEEL), Some(Wheel));
    }

    #[test]
    fn ignores_other_messages() {
        const WM_KEYDOWN: u32 = 0x0100;
        const WM_LBUTTONDBLCLK: u32 = 0x0203;

        assert_eq!(MouseMessage::from_wm(WM_KEYDOWN), None);
        assert_eq!(MouseMessage::from_wm(WM_LBUTTONDBLCLK), None);
    }

    #[test]
    fn movement_is_never_blocked() {
        let router = ClickRouter::new();
        assert!(!router.should_block(Move, true));
    }

    #[test]
    fn wheel_follows_the_overlay() {
        let router = ClickRouter::new();
        assert!(router.should_block(Wheel, true));
        assert!(!router.should_block(Wheel, false));
    }

    #[test]
    fn click_on_the_overlay_is_blocked_in_full() {
        let router = ClickRouter::new();
        assert!(router.should_block(Press(Left), true));
        assert!(router.should_block(Release(Left), true));
    }

    #[test]
    fn release_follows_its_press_after_leaving_the_overlay() {
        let router = ClickRouter::new();
        assert!(router.should_block(Press(Right), true));
        // The cursor left the overlay while the button was held.
        assert!(router.should_block(Release(Right), false));
    }

    #[test]
    fn release_follows_its_press_after_entering_the_overlay() {
        // A camera drag started on the world and ended over a Blish window: the game saw the
        // press, so it must see the release, or it restores a stale cursor position.
        let router = ClickRouter::new();
        assert!(!router.should_block(Press(Right), false));
        assert!(!router.should_block(Release(Right), true));
    }

    #[test]
    fn buttons_are_tracked_independently() {
        let router = ClickRouter::new();
        assert!(router.should_block(Press(Left), true));
        assert!(!router.should_block(Press(Right), false));

        assert!(!router.should_block(Release(Right), true));
        assert!(router.should_block(Release(Left), false));
    }

    #[test]
    fn a_release_only_consumes_its_own_press() {
        let router = ClickRouter::new();
        assert!(router.should_block(Press(Left), true));
        assert!(router.should_block(Release(Left), false));
        // A stray second release has no press left to follow.
        assert!(!router.should_block(Release(Left), true));
    }
}
