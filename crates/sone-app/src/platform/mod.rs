//! OS integration. Linux: MPRIS + StatusNotifier tray. Other platforms are
//! tracked in PORTING_PLAN.md (SMTC via souvlaki, tray-icon on Windows).

#[cfg(target_os = "linux")]
pub mod mpris;
#[cfg(target_os = "linux")]
pub mod tray;
