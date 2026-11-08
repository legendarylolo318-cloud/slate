//! The mini player: a second, resizable window kept in sync with the main one.

use std::cell::RefCell;

use slate_core::window_state::Which;
use slint::winit_030::WinitWindowAccessor;
use slint::ComponentHandle;

use crate::backend::Backend;
use crate::bootstrap::{AppWindow, MiniPlayerWindow};

thread_local! {
    // Slint windows close when their last handle drops; UI-thread only.
    static MINI: RefCell<Option<MiniPlayerWindow>> = const { RefCell::new(None) };
}

/// Copies playback state from the main window into the mini player, if open.
pub fn sync(main: &AppWindow) {
    MINI.with(|m| {
        if let Some(mini) = m.borrow().as_ref() {
            mini.set_track_title(main.get_track_title());
            mini.set_track_artist(main.get_track_artist());
            mini.set_track_cover(main.get_track_cover());
            mini.set_playing(main.get_playing());
            mini.set_has_track(main.get_has_track());
            mini.set_progress(main.get_progress());
            mini.set_position_text(main.get_position_text());
            mini.set_duration_text(main.get_duration_text());
        }
    });
}

impl Backend {
    pub fn open_miniplayer(&self) {
        let this = self.clone();
        self.ui(move |main| {
            if MINI.with(|m| m.borrow().is_some()) {
                MINI.with(|m| {
                    if let Some(w) = m.borrow().as_ref() {
                        let _ = w.show();
                    }
                });
                return;
            }
            let Ok(mini) = MiniPlayerWindow::new() else {
                main.set_error("Could not open the mini player.".into());
                return;
            };
            let b = this.clone();
            mini.on_toggle_play(move || b.toggle_play());
            let b = this.clone();
            mini.on_next(move || b.next());
            let b = this.clone();
            mini.on_previous(move || b.previous());
            let b = this.clone();
            mini.on_seek(move |f| b.seek(f));
            let b = this.clone();
            mini.on_go_album(move || b.go_current("album"));
            let b = this.clone();
            mini.on_go_artist(move || b.go_current("artist"));
            let (weak, rt) = (mini.as_weak(), this.rt.clone());
            crate::window_state::install(Which::Mini, move || {
                if let Some(w) = weak.upgrade() {
                    crate::window_state::save_now(&rt, w.window(), Which::Mini);
                }
            });
            mini.window().on_winit_window_event(|w, event| {
                if crate::window_state::is_geometry_event(event) {
                    crate::window_state::after_first_show(w, Which::Mini);
                    crate::window_state::touch(Which::Mini);
                }
                slint::winit_030::EventResult::Propagate
            });
            let b = this.clone();
            mini.on_expand(move || {
                let mini = MINI.with(|m| m.borrow_mut().take());
                if let Some(mini) = mini {
                    crate::window_state::save_now(&b.rt, mini.window(), Which::Mini);
                    // A shown window keeps itself alive until hidden: dropping the handle
                    // alone left the mini player on screen, and the next open made a second
                    // one (#149). Drop it after this click has been handled.
                    let _ = mini.hide();
                    slint::Timer::single_shot(std::time::Duration::ZERO, move || drop(mini));
                }
                b.ui(|w| {
                    let _ = w.show();
                });
            });
            let rt = this.rt.clone();
            mini.window().on_close_requested(move || {
                MINI.with(|m| {
                    if let Some(mini) = m.borrow_mut().take() {
                        crate::window_state::save_now(&rt, mini.window(), Which::Mini);
                    }
                });
                slint::CloseRequestResponse::HideWindow
            });
            crate::window_state::restore(mini.window(), Which::Mini);
            let _ = mini.show();
            crate::window_state::after_show(mini.window(), Which::Mini);
            crate::window_state::check_soon(mini.as_weak(), Which::Mini);
            MINI.with(|m| *m.borrow_mut() = Some(mini));
            sync(&main);
        });
    }
}
