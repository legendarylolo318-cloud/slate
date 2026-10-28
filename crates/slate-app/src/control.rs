//! Commands that arrive from outside the window (OS media keys, MPRIS clients,
//! the tray menu) and are routed to the backend.

use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum Control {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Relative seek in seconds (may be negative).
    SeekRelative(f64),
    SetPosition(f64),
    SetVolume(f64),
    SetShuffle(bool),
    /// 0 = off, 1 = all, 2 = one.
    SetLoop(u8),
    Raise,
    Quit,
}

pub type ControlFn = Arc<dyn Fn(Control) + Send + Sync>;
/// Latest playback position in seconds, if a track is loaded.
pub type PositionFn = Arc<dyn Fn() -> Option<f32> + Send + Sync>;
