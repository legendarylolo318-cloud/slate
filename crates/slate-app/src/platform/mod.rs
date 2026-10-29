//! OS integration. Linux: MPRIS + StatusNotifier tray. Windows: notification
//! area icon and media transport controls.

#[cfg(target_os = "linux")]
pub mod mpris;
#[cfg(target_os = "linux")]
pub mod tray;
#[cfg(windows)]
pub mod win_tray;
