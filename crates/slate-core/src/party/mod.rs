//! Listening party: playback sync between friends who each play from their own account.
//! `proto` is the pure protocol, `requests` the pure request rules; the iroh transport
//! (feature `party`) lives in `net`.

pub mod chat;
#[cfg(feature = "party")]
pub mod net;
pub mod proto;
pub mod reactions;
pub mod requests;
