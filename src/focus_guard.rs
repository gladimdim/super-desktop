//! Keep the keyboard on the overlay when a background window opens.
//!
//! While no card is expanded the overlay's layer surface takes the keyboard
//! only on demand, so an idle overlay does not eat desktop keys. Hyprland
//! focuses every window it maps, and an on-demand layer gives the keyboard up
//! to it: a game or app that starts in the background takes the keys typed
//! into a card, although its window is hidden under the overlay.
//!
//! The daemon follows Hyprland's `openwindow` events. When the overlay loses
//! the keyboard within `WINDOW` of a window opening, in either order (the
//! event socket and the Wayland connection are not ordered against each
//! other), the overlay takes the keyboard back. It does not when the pointer
//! had left the overlay, which is how a click on another monitor's window
//! looks, nor while the overlay is hidden or sliding out.

use std::time::{Duration, Instant};

/// How close a window opening and the overlay losing the keyboard must be
/// for the one to count as the cause of the other.
pub const WINDOW: Duration = Duration::from_millis(1500);

/// Windows of this class never trigger the guard: the daemon's own windows,
/// and the screensaver, which hides the overlay itself.
pub fn ignores_class(class: &str) -> bool {
    class == "org.omarchy.superdesktop" || class == crate::screensaver::SCREENSAVER_CLASS
}

/// Pairs window openings with keyboard losses.
#[derive(Debug, Default)]
pub struct FocusGuard {
    opened_at: Option<Instant>,
    lost_at: Option<Instant>,
}

fn recent(at: Option<Instant>, now: Instant) -> bool {
    at.is_some_and(|at| now.saturating_duration_since(at) <= WINDOW)
}

impl FocusGuard {
    /// A window opened. `overlay_has_keyboard` is whether the overlay holds
    /// the keyboard right now. Returns true when the overlay should take the
    /// keyboard back at once: it lost it just before this window opened.
    pub fn window_opened(&mut self, now: Instant, overlay_has_keyboard: bool) -> bool {
        if overlay_has_keyboard {
            // The loss, if this window takes focus, is still to come.
            self.opened_at = Some(now);
            self.lost_at = None;
            return false;
        }
        let reclaim = recent(self.lost_at.take(), now);
        if !reclaim {
            self.opened_at = None;
        }
        reclaim
    }

    /// The overlay lost the keyboard. `pointer_left` is whether the pointer
    /// was outside the overlay at that moment. Returns true when the overlay
    /// should take the keyboard back.
    pub fn keyboard_lost(&mut self, now: Instant, pointer_left: bool) -> bool {
        if pointer_left {
            self.reset();
            return false;
        }
        if recent(self.opened_at.take(), now) {
            self.lost_at = None;
            return true;
        }
        self.lost_at = Some(now);
        false
    }

    /// The overlay has the keyboard again, or was hidden or shown: anything
    /// pending is over.
    pub fn reset(&mut self) {
        self.opened_at = None;
        self.lost_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn later(at: Instant, ms: u64) -> Instant {
        at + Duration::from_millis(ms)
    }

    #[test]
    fn focus_guard_reclaims_when_the_loss_follows_the_open() {
        let mut guard = FocusGuard::default();
        let t = Instant::now();
        assert!(!guard.window_opened(t, true));
        assert!(guard.keyboard_lost(later(t, 40), false));
        // Taken back once: a second loss with no new window is the user's.
        assert!(!guard.keyboard_lost(later(t, 80), false));
    }

    #[test]
    fn focus_guard_reclaims_when_the_loss_comes_first() {
        let mut guard = FocusGuard::default();
        let t = Instant::now();
        assert!(!guard.keyboard_lost(t, false));
        assert!(guard.window_opened(later(t, 30), false));
        assert!(!guard.window_opened(later(t, 60), false));
    }

    #[test]
    fn focus_guard_ignores_events_far_apart() {
        let mut guard = FocusGuard::default();
        let t = Instant::now();
        assert!(!guard.window_opened(t, true));
        assert!(!guard.keyboard_lost(later(t, 2000), false));
        let t = later(t, 5000);
        assert!(!guard.keyboard_lost(t, false));
        assert!(!guard.window_opened(later(t, 2000), false));
    }

    #[test]
    fn focus_guard_leaves_a_pointer_move_elsewhere_alone() {
        let mut guard = FocusGuard::default();
        let t = Instant::now();
        assert!(!guard.window_opened(t, true));
        assert!(!guard.keyboard_lost(later(t, 40), true));
        // Nor does a loss with the pointer outside arm the other order.
        assert!(!guard.keyboard_lost(later(t, 100), true));
        assert!(!guard.window_opened(later(t, 150), false));
    }

    #[test]
    fn focus_guard_does_nothing_when_the_overlay_never_had_the_keyboard() {
        let mut guard = FocusGuard::default();
        let t = Instant::now();
        assert!(!guard.window_opened(t, false));
        assert!(!guard.window_opened(later(t, 10), false));
    }

    #[test]
    fn focus_guard_reset_drops_what_is_pending() {
        let mut guard = FocusGuard::default();
        let t = Instant::now();
        assert!(!guard.window_opened(t, true));
        guard.reset();
        assert!(!guard.keyboard_lost(later(t, 40), false));
        guard.reset();
        assert!(!guard.window_opened(later(t, 60), false));
    }

    #[test]
    fn focus_guard_ignores_its_own_and_screensaver_windows() {
        assert!(ignores_class("org.omarchy.superdesktop"));
        assert!(ignores_class(crate::screensaver::SCREENSAVER_CLASS));
        assert!(!ignores_class("steam_app_12345"));
        assert!(!ignores_class(""));
    }
}
