//! Windows notification-area icon. Lives on the UI thread, whose event loop
//! pumps the Win32 messages the icon needs; menu and click events arrive on
//! that thread and are routed like MPRIS commands.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::control::{Control, ControlFn};

thread_local! {
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

static PRESENT: AtomicBool = AtomicBool::new(false);

const ICON_PNG: &[u8] = include_bytes!("../../../../assets/icons/32x32.png");

/// Whether the icon exists (so closing the window can hide to it).
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Creates the icon. Must run on the UI thread.
pub fn start(control: ControlFn) {
    let icon = match icon() {
        Some(i) => i,
        None => {
            tracing::warn!("tray icon image could not be decoded");
            return;
        }
    };
    let play = MenuItem::new("Play / Pause", true, None);
    let next = MenuItem::new("Next", true, None);
    let previous = MenuItem::new("Previous", true, None);
    let show = MenuItem::new("Show Slate", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    let appended = menu.append_items(&[
        &show,
        &PredefinedMenuItem::separator(),
        &play,
        &previous,
        &next,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    if let Err(e) = appended {
        tracing::warn!("tray menu: {e}");
        return;
    }
    let ids = [
        (play.id().clone(), Control::PlayPause),
        (next.id().clone(), Control::Next),
        (previous.id().clone(), Control::Previous),
        (show.id().clone(), Control::Raise),
        (quit.id().clone(), Control::Quit),
    ];
    let on_menu = control.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if let Some((_, c)) = ids.iter().find(|(id, _)| *id == event.id) {
            on_menu(c.clone());
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            control(Control::Raise);
        }
    }));
    match TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Slate")
        .with_icon(icon)
        .build()
    {
        Ok(tray) => {
            TRAY.with(|t| *t.borrow_mut() = Some(tray));
            PRESENT.store(true, Ordering::Relaxed);
        }
        Err(e) => tracing::warn!("tray icon unavailable: {e}"),
    }
}

/// Current track in the tooltip. Must run on the UI thread.
pub fn set_tooltip(text: &str) {
    TRAY.with(|t| {
        if let Some(tray) = t.borrow().as_ref() {
            // Windows caps tooltips at 127 characters.
            let text: String = text.chars().take(120).collect();
            let _ = tray.set_tooltip(Some(text));
        }
    });
}

fn icon() -> Option<Icon> {
    let rgba = image::load_from_memory(ICON_PNG).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    Icon::from_rgba(rgba.into_raw(), w, h).ok()
}
