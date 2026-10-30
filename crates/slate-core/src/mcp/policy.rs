//! Which MCP tools change things, which are destructive, and what an agent is allowed to do:
//! read-only mode, `confirm: true`, the in-app approval, and the optional bearer token.

/// How much a tool can change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Only reads.
    Read,
    /// Changes playback, the queue or the library, in a way that is easy to put back.
    Write,
    /// Deletes or rewrites things; needs `confirm: true` (and the in-app approval).
    Destructive,
}

use Kind::{Destructive, Read, Write};

/// Every tool by name. A name missing here is refused, so a new tool must be classified.
pub const TOOLS: &[(&str, Kind)] = &[
    // state
    ("get_now_playing", Read),
    ("get_queue", Read),
    ("get_history", Read),
    ("get_signal_path", Read),
    ("get_current_lyric_line", Read),
    ("get_listening_stats", Read),
    ("get_party_status", Read),
    // catalogue
    ("search_tracks", Read),
    ("search_albums", Read),
    ("search_artists", Read),
    ("search_playlists", Read),
    ("get_track_radio", Read),
    ("get_artist_top_tracks", Read),
    ("get_album_tracks", Read),
    ("get_track_lyrics", Read),
    ("get_mixes", Read),
    ("get_explore", Read),
    ("get_similar_artists", Read),
    ("get_track_credits", Read),
    ("get_album_info", Read),
    ("get_artist_info", Read),
    ("get_other_versions", Read),
    ("match_tracks", Read),
    ("get_playlist_suggestions", Read),
    ("compare_playlists", Read),
    ("get_smart_playlists", Read),
    // library reads
    ("get_user_playlists", Read),
    ("get_playlist_tracks", Read),
    ("get_favorite_tracks", Read),
    ("get_favorite_albums", Read),
    ("get_favorite_artists", Read),
    ("is_track_favorited", Read),
    // playback and queue
    ("play_tracks", Write),
    ("play_source", Write),
    ("shuffle_source", Write),
    ("play_query", Write),
    ("play_smart_playlist", Write),
    ("pause", Write),
    ("resume", Write),
    ("skip_next", Write),
    ("skip_previous", Write),
    ("toggle_shuffle", Write),
    ("seek_to", Write),
    ("set_volume", Write),
    ("set_repeat", Write),
    ("add_to_queue", Write),
    ("play_next", Write),
    ("move_queue_item", Write),
    ("remove_from_queue", Write),
    ("clear_queue", Destructive),
    // library writes
    ("create_playlist", Write),
    ("create_playlist_from_tracks", Write),
    ("update_playlist", Write),
    ("add_to_playlist", Write),
    ("favorite_track", Write),
    ("favorite_album", Write),
    ("favorite_artist", Write),
    ("remove_track_from_playlist", Destructive),
    ("delete_playlist", Destructive),
    ("dedupe_playlist", Destructive),
    ("reorder_playlist", Destructive),
    // meta
    ("batch", Write),
];

pub fn kind_of(tool: &str) -> Option<Kind> {
    TOOLS.iter().find(|(n, _)| *n == tool).map(|(_, k)| *k)
}

pub fn is_mutating(tool: &str) -> bool {
    !matches!(kind_of(tool), Some(Read))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Hide and reject every tool that is not `Read`.
    pub read_only: bool,
    /// Destructive calls wait for the user in the app ("Ask before destructive agent actions").
    pub ask_destructive: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            read_only: false,
            ask_destructive: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// Not allowed at all; the text is the reason for the agent.
    Reject(String),
    /// A destructive tool called without `confirm: true`: describe, do nothing.
    Describe,
    /// Allowed once the user says yes in the app.
    AskUser,
}

/// What may happen for a call to `tool`. `confirm` is the argument the agent passed.
pub fn decide(policy: &Policy, tool: &str, confirm: bool) -> Decision {
    let Some(kind) = kind_of(tool) else {
        return Decision::Reject(format!("Unknown tool “{tool}”."));
    };
    match kind {
        Read => Decision::Allow,
        _ if policy.read_only => Decision::Reject(format!(
            "Slate is in read-only mode for agents, so “{tool}” is not available."
        )),
        Write => Decision::Allow,
        Destructive if !confirm => Decision::Describe,
        Destructive if policy.ask_destructive => Decision::AskUser,
        Destructive => Decision::Allow,
    }
}

/// Tool names an agent should see: all, or only the readers in read-only mode.
pub fn visible(policy: &Policy, tool: &str) -> bool {
    match kind_of(tool) {
        Some(Read) => true,
        Some(_) => !policy.read_only,
        None => false,
    }
}

/// Constant-time comparison of the `Authorization: Bearer …` header with the token.
pub fn bearer_ok(authorization: Option<&str>, expected: &str) -> bool {
    let Some(given) = authorization.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let (a, b) = (given.trim().as_bytes(), expected.as_bytes());
    if expected.is_empty() {
        return false;
    }
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(1));
    }
    diff == 0
}

/// A fresh random bearer token.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tool_is_classified_once() {
        let mut names: Vec<&str> = TOOLS.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a tool is listed twice");
        assert!(is_mutating("play_tracks") && !is_mutating("search_tracks"));
        assert_eq!(kind_of("delete_playlist"), Some(Destructive));
        assert_eq!(kind_of("nope"), None);
    }

    #[test]
    fn the_five_destructive_tools_from_the_brief() {
        for t in [
            "delete_playlist",
            "clear_queue",
            "remove_track_from_playlist",
            "dedupe_playlist",
            "reorder_playlist",
        ] {
            assert_eq!(kind_of(t), Some(Destructive), "{t}");
        }
    }

    #[test]
    fn reads_always_pass_and_writes_follow_read_only() {
        let open = Policy::default();
        let ro = Policy {
            read_only: true,
            ..open
        };
        assert_eq!(decide(&ro, "search_tracks", false), Decision::Allow);
        assert_eq!(decide(&open, "play_tracks", false), Decision::Allow);
        assert!(matches!(
            decide(&ro, "play_tracks", false),
            Decision::Reject(_)
        ));
        assert!(matches!(
            decide(&ro, "delete_playlist", true),
            Decision::Reject(_)
        ));
        assert!(matches!(
            decide(&open, "mystery", false),
            Decision::Reject(_)
        ));
    }

    #[test]
    fn destructive_needs_confirm_then_the_user() {
        let ask = Policy::default();
        let trust = Policy {
            ask_destructive: false,
            ..ask
        };
        assert_eq!(decide(&ask, "delete_playlist", false), Decision::Describe);
        assert_eq!(decide(&trust, "delete_playlist", false), Decision::Describe);
        assert_eq!(decide(&ask, "delete_playlist", true), Decision::AskUser);
        assert_eq!(decide(&trust, "delete_playlist", true), Decision::Allow);
    }

    #[test]
    fn read_only_hides_everything_that_changes_things() {
        let ro = Policy {
            read_only: true,
            ask_destructive: true,
        };
        assert!(visible(&ro, "get_queue"));
        assert!(!visible(&ro, "add_to_queue"));
        assert!(!visible(&ro, "clear_queue"));
        assert!(visible(&Policy::default(), "add_to_queue"));
        assert!(!visible(&Policy::default(), "not_a_tool"));
    }

    #[test]
    fn bearer_tokens() {
        assert!(bearer_ok(Some("Bearer abc"), "abc"));
        assert!(bearer_ok(Some("Bearer abc "), "abc"));
        assert!(!bearer_ok(Some("Bearer abd"), "abc"));
        assert!(!bearer_ok(Some("Bearer abcd"), "abc"));
        assert!(!bearer_ok(Some("abc"), "abc"));
        assert!(!bearer_ok(None, "abc"));
        assert!(!bearer_ok(Some("Bearer "), ""));
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert_ne!(t, generate_token());
    }
}
