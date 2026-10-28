//! Platform media-control bridge (MPRIS on Linux, SMTC on Windows).
//!
//! The core only describes *what* the OS media surface should show; the
//! frontend registers a [`MediaSink`] that knows *how* to show it.

use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub enum MediaCommand {
    SetMetadata {
        track_id: u64,
        title: String,
        artist: String,
        album: String,
        art_url: String,
        duration_secs: f64,
        url: Option<String>,
        album_artist: Option<String>,
        track_number: Option<u32>,
        disc_number: Option<u32>,
        content_created: Option<String>,
        user_rating: Option<f64>,
    },
    SetPlaybackStatus {
        is_playing: bool,
    },
    SetVolume {
        volume: f64,
    },
    Seeked {
        position_secs: f64,
    },
    SetShuffle {
        enabled: bool,
    },
    SetLoopStatus {
        mode: u8,
    },
    SetFullscreen {
        fullscreen: bool,
    },
    Stop,
}

pub trait MediaSink: Send + Sync + 'static {
    fn send(&self, cmd: MediaCommand);
}

/// Fan-in point held by `AppState`. Commands sent before a sink is registered
/// are dropped, which matches running without OS media controls.
#[derive(Default)]
pub struct MediaHub {
    sink: RwLock<Option<Arc<dyn MediaSink>>>,
}

impl MediaHub {
    pub fn set_sink(&self, sink: Arc<dyn MediaSink>) {
        if let Ok(mut g) = self.sink.write() {
            *g = Some(sink);
        }
    }

    pub fn send(&self, cmd: MediaCommand) {
        if let Ok(g) = self.sink.read() {
            if let Some(sink) = g.as_ref() {
                sink.send(cmd);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Rec(Mutex<Vec<String>>);
    impl MediaSink for Rec {
        fn send(&self, cmd: MediaCommand) {
            if let Ok(mut g) = self.0.lock() {
                g.push(format!("{cmd:?}"));
            }
        }
    }

    #[test]
    fn drops_without_sink_and_forwards_with_one() {
        let hub = MediaHub::default();
        hub.send(MediaCommand::Stop);
        let rec = Arc::new(Rec(Mutex::new(Vec::new())));
        hub.set_sink(rec.clone());
        hub.send(MediaCommand::Stop);
        let n = rec.0.lock().map(|g| g.len()).unwrap_or(0);
        assert_eq!(n, 1);
    }
}
