//! StatusNotifierItem tray (Linux) via ksni.

use ksni::menu::{MenuItem, StandardItem};
use ksni::TrayMethods;

use crate::control::{Control, ControlFn};

const ICON_PNG: &[u8] = include_bytes!("../../assets/tray.png");

struct SoneTray {
    control: ControlFn,
    tooltip: String,
    icon: ksni::Icon,
}

impl std::fmt::Debug for SoneTray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoneTray")
            .field("tooltip", &self.tooltip)
            .finish()
    }
}

/// RGBA8 → ARGB32 in network byte order, as StatusNotifierItem requires.
fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgba.len());
    let (pixels, _) = rgba.as_chunks::<4>();
    for [r, g, b, a] in pixels {
        out.extend_from_slice(&[*a, *r, *g, *b]);
    }
    out
}

fn item(label: &str, ctl: Control) -> MenuItem<SoneTray> {
    StandardItem {
        label: label.into(),
        activate: Box::new(move |this: &mut SoneTray| (this.control)(ctl.clone())),
        ..Default::default()
    }
    .into()
}

impl ksni::Tray for SoneTray {
    fn id(&self) -> String {
        "sone".into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![self.icon.clone()]
    }

    fn title(&self) -> String {
        self.tooltip.clone()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.tooltip.clone(),
            description: String::new(),
            icon_name: String::new(),
            icon_pixmap: Vec::new(),
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        (self.control)(Control::Raise);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            item("Show", Control::Raise),
            MenuItem::Separator,
            item("Play / Pause", Control::PlayPause),
            item("Next Track", Control::Next),
            item("Previous Track", Control::Previous),
            MenuItem::Separator,
            item("Quit", Control::Quit),
        ]
    }
}

pub struct TrayHandle(ksni::Handle<SoneTray>);

impl TrayHandle {
    pub async fn set_tooltip(&self, text: String) {
        self.0.update(move |t| t.tooltip = text).await;
    }
}

/// Registers the tray. `None` when the icon cannot be decoded or no
/// StatusNotifier host answers.
pub async fn start(control: ControlFn) -> Option<TrayHandle> {
    let rgba = match image::load_from_memory(ICON_PNG) {
        Ok(img) => img.to_rgba8(),
        Err(e) => {
            tracing::warn!("tray icon undecodable: {e}");
            return None;
        }
    };
    let (w, h) = rgba.dimensions();
    let tray = SoneTray {
        control,
        tooltip: "SONE".into(),
        icon: ksni::Icon {
            width: w as i32,
            height: h as i32,
            data: rgba_to_argb(rgba.as_raw()),
        },
    };
    // Inside Flatpak/Snap a well-known bus name cannot be owned.
    let sandboxed = std::env::var("FLATPAK_ID").is_ok() || std::env::var("SNAP").is_ok();
    match tray.disable_dbus_name(sandboxed).spawn().await {
        Ok(handle) => {
            tracing::info!("tray registered");
            Some(TrayHandle(handle))
        }
        Err(e) => {
            tracing::warn!("tray unavailable: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn converts_rgba_to_argb() {
        assert_eq!(super::rgba_to_argb(&[1, 2, 3, 4]), vec![4, 1, 2, 3]);
    }
}
