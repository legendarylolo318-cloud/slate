//! A small MPD protocol server, so MPD clients (M.A.L.P., MaximumMPD, Rigelian, `mpc`) can
//! control Slate. No crate: the protocol is line based text over TCP.
//!
//! The server knows nothing of Slate's player: everything goes through the [`Host`] trait,
//! which the app implements on the same action path the MPRIS and MCP controls use. Nothing
//! exists (no socket, no task, no timer) until [`server::start`] is called.
//!
//! The MPD "playlist" is the playing track followed by the upcoming queue.

pub mod proto;
pub mod server;

use std::future::Future;
use std::pin::Pin;

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One queue entry or search result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Song {
    /// TIDAL track id (also the MPD song id).
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub duration: f64,
    pub track: Option<u32>,
    pub date: String,
}

impl Song {
    pub fn uri(&self) -> String {
        format!("tidal://track/{}", self.id)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlayState {
    Play,
    Pause,
    #[default]
    Stop,
}

impl PlayState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
        }
    }
}

/// What `status` and `currentsong` report, read in one go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub state: PlayState,
    /// 0–100.
    pub volume: u8,
    pub repeat: bool,
    pub random: bool,
    pub elapsed: f64,
    pub duration: f64,
    pub queue_len: usize,
    /// Changes whenever the queue's tracks or order change.
    pub queue_hash: u64,
    /// Position of the playing track in the queue.
    pub pos: Option<usize>,
    pub current: Option<Song>,
    /// Position and id of the track after it.
    pub next: Option<(usize, u64)>,
}

/// What a client asked the player to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// `play` without a position: resume, or start the queue.
    Resume,
    Pause(bool),
    Stop,
    Next,
    Previous,
    /// Play the queue entry at this position.
    PlayPos(usize),
    /// Seek the playing track to this many seconds.
    Seek(f64),
    SetVolume(u8),
    SetRandom(bool),
    SetRepeat(bool),
    Clear,
    Delete(usize),
    Move {
        from: usize,
        to: usize,
    },
    /// Add the track behind `uri` to the end of the queue (or at `pos`).
    Add {
        uri: String,
        pos: Option<usize>,
    },
    /// Append a stored playlist (a TIDAL playlist, by name).
    Load(String),
    /// Shuffle the upcoming tracks once.
    Shuffle,
}

/// Why the player refused: an MPD error code and a message.
#[derive(Clone, Debug, PartialEq)]
pub struct HostError {
    pub code: u32,
    pub msg: String,
}

impl HostError {
    pub fn no_exist(msg: impl Into<String>) -> Self {
        Self {
            code: proto::ACK_NO_EXIST,
            msg: msg.into(),
        }
    }
    pub fn arg(msg: impl Into<String>) -> Self {
        Self {
            code: proto::ACK_ARG,
            msg: msg.into(),
        }
    }
    pub fn system(msg: impl Into<String>) -> Self {
        Self {
            code: proto::ACK_SYSTEM,
            msg: msg.into(),
        }
    }
}

/// The player, as the server sees it.
pub trait Host: Send + Sync + 'static {
    fn status(&self) -> BoxFut<'_, Status>;
    fn queue(&self) -> BoxFut<'_, Vec<Song>>;
    fn act(&self, action: Action) -> BoxFut<'_, Result<(), HostError>>;
    /// TIDAL track search for the words in `text` (the server filters the answer further).
    fn search(&self, text: String) -> BoxFut<'_, Result<Vec<Song>, HostError>>;
    /// Names of the user's TIDAL playlists.
    fn playlists(&self) -> BoxFut<'_, Result<Vec<String>, HostError>>;
    fn playlist_songs(&self, name: String) -> BoxFut<'_, Result<Vec<Song>, HostError>>;
    /// Cover art of a song in the queue (or the playing one when `None`), as image bytes.
    fn cover(&self, song_id: Option<u64>) -> BoxFut<'_, Option<Vec<u8>>>;
}

/// A password that prints as `<set>` / `<empty>`, so no `{:?}` of the settings can leak it.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Secret(pub String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "<empty>"
        } else {
            "<set>"
        })
    }
}

impl Secret {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for Secret {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Server settings.
#[derive(Clone, PartialEq, Eq)]
pub struct Config {
    pub port: u16,
    /// Listen on every interface instead of loopback only. Needs a password.
    pub lan: bool,
    pub password: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("port", &self.port)
            .field("lan", &self.lan)
            .field("password", &Secret(self.password.clone()))
            .finish()
    }
}

impl Config {
    pub fn check(&self) -> Result<(), String> {
        if self.lan && self.password.is_empty() {
            return Err("LAN access needs a password".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_password_never_shows_in_debug_output() {
        let c = Config {
            port: 6600,
            lan: true,
            password: "hunter2hunter2".into(),
        };
        assert!(!format!("{c:?}").contains("hunter2"));
        assert!(format!("{c:?}").contains("<set>"));
        let s = Secret::from("hunter2");
        assert_eq!(format!("{s:?}"), "<set>");
        assert_eq!(format!("{:?}", Secret::default()), "<empty>");
        // Stored as a plain string, as before.
        assert_eq!(serde_json::to_string(&s).expect("json"), "\"hunter2\"");
        assert_eq!(
            serde_json::from_str::<Secret>("\"x\"").expect("json"),
            Secret::from("x")
        );
    }
}
