//! System-wide shortcuts, off by default (Settings → General).
//!
//! Linux: the XDG GlobalShortcuts portal. Slate proposes triggers; the desktop
//! decides, and users rebind them in their desktop's shortcut settings.
//! Windows: `RegisterHotKey` via `global-hotkey`, owned by the UI thread.

use crate::control::{Control, ControlFn};

/// Shortcut id, what it does, proposed trigger (XDG shortcut syntax), label.
const SHORTCUTS: [(&str, &str, &str, &str); 3] = [
    (
        "play-pause",
        "Play / pause",
        "CTRL+ALT+space",
        "Ctrl+Alt+Space",
    ),
    ("next", "Next track", "CTRL+ALT+Right", "Ctrl+Alt+→"),
    ("previous", "Previous track", "CTRL+ALT+Left", "Ctrl+Alt+←"),
];

/// "Ctrl+Alt+Space play / pause, …" for the settings hint.
pub fn describe() -> String {
    SHORTCUTS
        .iter()
        .map(|(_, what, _, label)| format!("{label} {}", what.to_lowercase()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn control_for(id: &str) -> Option<Control> {
    match id {
        "play-pause" => Some(Control::PlayPause),
        "next" => Some(Control::Next),
        "previous" => Some(Control::Previous),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
pub mod portal {
    use super::*;
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    use futures_util::StreamExt;

    /// Binds the shortcuts and serves activations until the task is aborted
    /// (which drops the session and so releases them).
    pub async fn run(control: ControlFn) {
        if let Err(e) = serve(control).await {
            tracing::warn!("global shortcuts unavailable: {e}");
        }
    }

    /// The portal checks that `<APP_ID>.desktop` is installed.
    pub(super) use crate::platform::APP_ID;

    async fn serve(control: ControlFn) -> Result<(), ashpd::Error> {
        // Unsandboxed apps must name themselves first, or the portal refuses
        // with "An app id is required" (xdg-desktop-portal 1.19+). It also
        // needs `<APP_ID>.desktop` installed, so a bare binary or a
        // non-integrated AppImage gets no portal shortcuts.
        if let Err(e) = ashpd::register_host_app(APP_ID.parse()?).await {
            tracing::debug!("portal host registry: {e}");
        }
        let portal = GlobalShortcuts::new().await?;
        let session = portal.create_session().await?;
        let wanted: Vec<NewShortcut> = SHORTCUTS
            .iter()
            .map(|(id, what, trigger, _)| NewShortcut::new(*id, *what).preferred_trigger(*trigger))
            .collect();
        portal
            .bind_shortcuts(&session, &wanted, None)
            .await?
            .response()?;
        tracing::info!("global shortcuts bound through the desktop portal");
        let mut activated = portal.receive_activated().await?;
        while let Some(a) = activated.next().await {
            if let Some(c) = control_for(a.shortcut_id()) {
                control(c);
            }
        }
        drop(session);
        Ok(())
    }
}

#[cfg(windows)]
pub mod hotkeys {
    use super::*;
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};
    use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
    use std::cell::RefCell;

    thread_local! {
        static MANAGER: RefCell<Option<GlobalHotKeyManager>> = const { RefCell::new(None) };
    }

    /// Registers or releases the hotkeys. Must run on the UI thread.
    pub fn set(enabled: bool, control: ControlFn) {
        MANAGER.with(|m| {
            // Dropping the manager unregisters everything it holds.
            *m.borrow_mut() = None;
            if !enabled {
                return;
            }
            let manager = match GlobalHotKeyManager::new() {
                Ok(manager) => manager,
                Err(e) => {
                    tracing::warn!("global shortcuts unavailable: {e}");
                    return;
                }
            };
            let mods = Some(Modifiers::CONTROL | Modifiers::ALT);
            let keys = [Code::Space, Code::ArrowRight, Code::ArrowLeft];
            let mut ids = Vec::new();
            for ((id, ..), code) in SHORTCUTS.iter().zip(keys) {
                let hotkey = HotKey::new(mods, code);
                match manager.register(hotkey) {
                    Ok(()) => ids.push((hotkey.id(), *id)),
                    Err(e) => tracing::warn!("shortcut {id} not registered: {e}"),
                }
            }
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                if e.state() != HotKeyState::Pressed {
                    return;
                }
                if let Some(c) = ids
                    .iter()
                    .find(|(hid, _)| *hid == e.id())
                    .and_then(|(_, id)| control_for(id))
                {
                    control(c);
                }
            }));
            *m.borrow_mut() = Some(manager);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn the_portal_app_id_is_valid() {
        assert!(portal::APP_ID.parse::<ashpd::AppID>().is_ok());
    }

    /// The desktop file, metainfo and code must agree on the id.
    #[test]
    fn packaging_uses_the_same_app_id() {
        let id = crate::platform::APP_ID;
        let desktop = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data")
            .join(format!("{id}.desktop"));
        let text = std::fs::read_to_string(&desktop).expect("desktop file named after APP_ID");
        assert!(text.contains(&format!("StartupWMClass={id}")));
        let meta = std::fs::read_to_string(desktop.with_file_name(format!("{id}.metainfo.xml")))
            .expect("metainfo named after APP_ID");
        assert!(meta.contains(&format!("<id>{id}</id>")));
        assert!(meta.contains(&format!(">{id}.desktop</launchable>")));
    }

    #[test]
    fn every_shortcut_maps_to_a_control_and_is_described() {
        for (id, ..) in SHORTCUTS {
            assert!(control_for(id).is_some(), "{id}");
        }
        assert!(control_for("unknown").is_none());
        let d = describe();
        assert!(
            d.contains("Ctrl+Alt+Space play / pause") && d.contains("next track"),
            "{d}"
        );
    }
}
