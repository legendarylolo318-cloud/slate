use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NowPlayingSnapshot {
    pub track_id: Option<u64>,
    pub title: String,
    pub artist: String,
    pub album: Option<String>,
    pub duration_seconds: u32,
    pub position_seconds: u32,
    pub is_playing: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueueTrackSnapshot {
    pub id: u64,
    pub title: String,
    pub artist: String,
    #[serde(default)]
    pub duration_seconds: u32,
}

#[derive(Default, Debug)]
pub struct McpState {
    pub now_playing: Option<NowPlayingSnapshot>,
    pub queue: Vec<QueueTrackSnapshot>,
    /// Most recent first. Kept up to date only while the MCP server runs.
    pub history: Vec<QueueTrackSnapshot>,
    pub signal_path: Option<serde_json::Value>,
    pub lyric_line: Option<String>,
    pub party: Option<serde_json::Value>,
    /// Repeat mode "off" | "all" | "one" and shuffle, for `queue_low` and `get_queue`.
    pub repeat: String,
}

pub type McpStateRef = Arc<RwLock<McpState>>;

pub fn new_state() -> McpStateRef {
    Arc::new(RwLock::new(McpState::default()))
}
