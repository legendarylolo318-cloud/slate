//! The compact now-playing strip: a small frameless window (title – artist, a thin progress
//! line, play / pause, next, close). It is fed by the same UI updates as the mini player
//! (the 2 Hz progress tick and track changes), so it adds no timer or thread, and when it is
//! closed the window is dropped and nothing of it remains.
//!
//! Kept on top where the platform allows it (X11, Windows). On Wayland a client cannot do
//! that: the strip is a normal small window and says so.

use std::cell::RefCell;

use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, StripWindow};

thread_local! {
    // Slint windows close when their last handle drops; UI-thread only.
    static STRIP: RefCell<Option<StripWindow>> = const { RefCell::new(None) };
}

/// What the user is told when the strip cannot stay on top.
pub const WAYLAND_NOTE: &str = "On Wayland the strip is a normal window: the compositor does \
                                not allow an app to keep a window on top.";

/// Whether a window of this platform can be kept above others. Wayland cannot.
pub fn can_stay_on_top(is_wayland: bool) -> bool {
    !is_wayland
}

/// Copies playback state from the main window into the strip, if open.
pub fn sync(main: &AppWindow) {
    STRIP.with(|s| {
        if let Some(strip) = s.borrow().as_ref() {
            strip.set_track_title(main.get_track_title());
            strip.set_track_artist(main.get_track_artist());
            strip.set_playing(main.get_playing());
            strip.set_has_track(main.get_has_track());
            strip.set_progress(main.get_progress());
        }
    });
}

fn is_wayland(window: &slint::Window) -> bool {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    window
        .with_winit_window(|w| {
            w.window_handle()
                .is_ok_and(|h| matches!(h.as_raw(), RawWindowHandle::Wayland(_)))
        })
        .unwrap_or(false)
}

/// Closes the strip if it is open. A shown Slint window keeps itself alive until it is
/// hidden, so dropping the handle alone left the window on screen, no longer updated, and
/// the next toggle opened a second one (#148). Hide first; drop the handle once the
/// current event (possibly the strip's own button or close request) has finished.
fn close() -> bool {
    let Some(strip) = STRIP.with(|s| s.borrow_mut().take()) else {
        return false;
    };
    let _ = strip.hide();
    slint::Timer::single_shot(std::time::Duration::ZERO, move || drop(strip));
    true
}

impl Backend {
    /// Opens the strip, or closes it if it is open (the menu entry toggles).
    pub fn toggle_strip(&self) {
        let this = self.clone();
        self.ui(move |main| {
            if close() {
                return;
            }
            let Ok(strip) = StripWindow::new() else {
                main.set_error("Could not open the strip.".into());
                return;
            };
            let b = this.clone();
            strip.on_toggle_play(move || b.toggle_play());
            let b = this.clone();
            strip.on_next(move || b.next());
            strip.on_close_strip(|| {
                close();
            });
            let weak = strip.as_weak();
            strip.on_drag(move || {
                if let Some(w) = weak.upgrade() {
                    w.window().with_winit_window(|win| {
                        let _ = win.drag_window();
                    });
                }
            });
            // Closed by the window manager (Alt+F4): the same as the close button, so the
            // next toggle opens it again instead of dropping a hidden window.
            strip.window().on_close_requested(|| {
                close();
                slint::CloseRequestResponse::HideWindow
            });
            let _ = strip.show();
            let wayland = is_wayland(strip.window());
            strip.set_on_top(can_stay_on_top(wayland));
            if wayland {
                main.set_notice(WAYLAND_NOTE.into());
            }
            STRIP.with(|s| *s.borrow_mut() = Some(strip));
            sync(&main);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_wayland_cannot_stay_on_top() {
        assert!(can_stay_on_top(false));
        assert!(!can_stay_on_top(true));
        assert!(WAYLAND_NOTE.contains("Wayland"));
    }
}
