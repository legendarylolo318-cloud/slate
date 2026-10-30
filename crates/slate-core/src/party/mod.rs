//! Listening party: playback sync between friends who each play from their own account.
//! `proto` is the pure protocol; the iroh transport (feature `party`) lives in `net`.

#[cfg(feature = "party")]
pub mod net;
pub mod proto;
