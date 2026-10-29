//! Windows media controls (SMTC: media keys, the volume flyout, the lock
//! screen) through `souvlaki`. A dedicated thread owns the controls and applies
//! the core's media commands; button presses are routed like MPRIS commands.

use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

use slate_core::media::{MediaCommand, MediaSink};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::control::{Control, ControlFn};

/// The main window's HWND, recorded on the UI thread once it is shown.
static HWND: OnceLock<isize> = OnceLock::new();

/// Records the window handle SMTC binds to. Call on the UI thread after show.
pub fn remember_window(window: &slint::Window) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = window.window_handle().window_handle() {
        if let RawWindowHandle::Win32(h) = handle.as_raw() {
            let _ = HWND.set(h.hwnd.get());
        }
    }
}

pub struct SmtcSink {
    tx: std::sync::Mutex<mpsc::Sender<MediaCommand>>,
}

impl MediaSink for SmtcSink {
    fn send(&self, cmd: MediaCommand) {
        if let Ok(tx) = self.tx.lock() {
            let _ = tx.send(cmd);
        }
    }
}

/// Starts the controls; `None` when there is no window handle or SMTC refuses.
pub fn start(control: ControlFn) -> Option<Arc<SmtcSink>> {
    let hwnd = *HWND.get()?;
    let (tx, rx) = mpsc::channel::<MediaCommand>();
    let (ready_tx, ready_rx) = mpsc::channel::<bool>();
    std::thread::Builder::new()
        .name("slate-smtc".into())
        .spawn(move || {
            let config = PlatformConfig {
                dbus_name: "slate",
                display_name: "Slate",
                hwnd: Some(hwnd as *mut std::ffi::c_void),
            };
            let mut controls = match MediaControls::new(config) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("media controls unavailable: {e:?}");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            if let Err(e) = controls.attach(move |event| {
                if let Some(c) = to_control(event) {
                    control(c);
                }
            }) {
                tracing::warn!("media controls: {e:?}");
                let _ = ready_tx.send(false);
                return;
            }
            let _ = ready_tx.send(true);
            serve(&mut controls, rx);
        })
        .ok()?;
    ready_rx
        .recv_timeout(Duration::from_secs(5))
        .ok()
        .filter(|ok| *ok)
        .map(|_| {
            Arc::new(SmtcSink {
                tx: std::sync::Mutex::new(tx),
            })
        })
}

fn serve(controls: &mut MediaControls, rx: mpsc::Receiver<MediaCommand>) {
    let mut playing = false;
    for cmd in rx {
        let result = match cmd {
            MediaCommand::SetMetadata {
                title,
                artist,
                album,
                art_url,
                duration_secs,
                ..
            } => controls.set_metadata(MediaMetadata {
                title: Some(&title),
                artist: Some(&artist),
                album: Some(&album),
                cover_url: (!art_url.is_empty()).then_some(art_url.as_str()),
                duration: (duration_secs > 0.0).then(|| Duration::from_secs_f64(duration_secs)),
            }),
            MediaCommand::SetPlaybackStatus { is_playing } => {
                playing = is_playing;
                controls.set_playback(playback(playing, None))
            }
            MediaCommand::Seeked { position_secs } => controls.set_playback(playback(
                playing,
                Some(Duration::from_secs_f64(position_secs.max(0.0))),
            )),
            MediaCommand::Stop => controls.set_playback(MediaPlayback::Stopped),
            _ => Ok(()),
        };
        if let Err(e) = result {
            tracing::debug!("media controls update failed: {e:?}");
        }
    }
}

fn playback(playing: bool, at: Option<Duration>) -> MediaPlayback {
    let progress = at.map(MediaPosition);
    if playing {
        MediaPlayback::Playing { progress }
    } else {
        MediaPlayback::Paused { progress }
    }
}

fn to_control(event: MediaControlEvent) -> Option<Control> {
    let step = |dir: SeekDirection, secs: f64| match dir {
        SeekDirection::Forward => secs,
        SeekDirection::Backward => -secs,
    };
    Some(match event {
        MediaControlEvent::Play => Control::Play,
        MediaControlEvent::Pause => Control::Pause,
        MediaControlEvent::Toggle => Control::PlayPause,
        MediaControlEvent::Next => Control::Next,
        MediaControlEvent::Previous => Control::Previous,
        MediaControlEvent::Stop => Control::Stop,
        MediaControlEvent::Seek(dir) => Control::SeekRelative(step(dir, 10.0)),
        MediaControlEvent::SeekBy(dir, d) => Control::SeekRelative(step(dir, d.as_secs_f64())),
        MediaControlEvent::SetPosition(MediaPosition(d)) => Control::SetPosition(d.as_secs_f64()),
        MediaControlEvent::SetVolume(v) => Control::SetVolume(v),
        MediaControlEvent::Raise => Control::Raise,
        MediaControlEvent::Quit => Control::Quit,
        MediaControlEvent::OpenUri(_) => return None,
        _ => return None,
    })
}
