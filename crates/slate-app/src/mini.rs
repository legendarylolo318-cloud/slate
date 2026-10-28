//! The mini player: a second, resizable window kept in sync with the main one.

use std::cell::RefCell;

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
            mini.on_expand(move || {
                MINI.with(|m| {
                    m.borrow_mut().take();
                });
                b.ui(|w| {
                    let _ = w.show();
                });
            });
            mini.window().on_close_requested(|| {
                MINI.with(|m| {
                    m.borrow_mut().take();
                });
                slint::CloseRequestResponse::HideWindow
            });
            let _ = mini.show();
            MINI.with(|m| *m.borrow_mut() = Some(mini));
            sync(&main);
        });
    }
}
