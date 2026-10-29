//! OS integration. Linux: MPRIS + StatusNotifier tray. Windows: notification
//! area icon and media transport controls.

/// Slate's application id: the desktop file's name (`<APP_ID>.desktop`), the
/// Wayland app_id / X11 WM_CLASS, the MPRIS desktop entry and the id the XDG
/// portals require. Must match `data/` and the metainfo `<id>`.
pub const APP_ID: &str = "io.github.legendarylolo318_cloud.slate";

pub mod global_keys;
#[cfg(target_os = "linux")]
pub mod mpris;
#[cfg(target_os = "linux")]
pub mod tray;
#[cfg(windows)]
pub mod win_smtc;
#[cfg(windows)]
pub mod win_tray;
