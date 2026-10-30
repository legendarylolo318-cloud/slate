use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{tool_router, ErrorData};
use serde::Deserialize;

use crate::mcp::server::SlateMcpServer;
use crate::AppState;

use super::util::NoArgs;

#[derive(Deserialize, JsonSchema)]
pub struct GetQueueArgs {
    /// Max items to return (default 20, max 50).
    pub limit: Option<u32>,
    /// Items to skip (default 0).
    pub offset: Option<u32>,
}

#[tool_router(router = state_tools, vis = "pub(crate)")]
impl SlateMcpServer {
    #[rmcp::tool(
        name = "get_now_playing",
        description = "Get the currently-playing track, position, duration, play state, remaining_seconds and ends_at (local time, HH:MM). Returns nowPlaying:null if nothing is playing. Example: {}."
    )]
    async fn get_now_playing(
        &self,
        Parameters(_): Parameters<NoArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let json = self.now_playing_json(&state).await;
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "get_queue",
        description = "Get the upcoming tracks in the play queue with track_count, total_duration_seconds and remaining_seconds (rest of the current track plus the queue). Params: limit (≤50, default 20), offset. Example: {\"limit\":10}."
    )]
    async fn get_queue(
        &self,
        Parameters(args): Parameters<GetQueueArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(20).clamp(1, 50) as usize;
        let state = self.app_handle.state::<AppState>();
        let json = self
            .queue_json(&state, limit, args.offset.unwrap_or(0) as usize)
            .await;
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }
}
