use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::schemars::JsonSchema;
use rmcp::{tool_router, ErrorData};
use serde::Deserialize;

use crate::mcp::events::{FavoriteChangedPayload, EV_FAVORITE_CHANGED};
use crate::mcp::sanitizer::{backfill_and_sanitize_tracks, SanitizedAlbum, SanitizedArtist};
use crate::mcp::server::SlateMcpServer;
use crate::AppState;

use super::util::require_user_id;

#[derive(Deserialize, JsonSchema)]
pub struct PaginatedArgs {
    /// Max items to return (default 50, max 100).
    pub limit: Option<u32>,
    /// Offset for pagination (default 0).
    pub offset: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TrackIdArgs {
    pub track_id: u64,
}

#[derive(Deserialize, JsonSchema)]
pub struct FavoriteArgs {
    /// The item's Tidal ID.
    pub id: u64,
    /// Either "add" or "remove".
    pub action: String,
}

#[tool_router(router = favorites_tools, vis = "pub(crate)")]
impl SlateMcpServer {
    #[rmcp::tool(
        name = "get_favorite_tracks",
        description = "The user's favourite tracks, newest first. Params: limit (≤100, default 50), offset. Example: {\"limit\":25}."
    )]
    async fn get_favorite_tracks(
        &self,
        Parameters(args): Parameters<PaginatedArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(50).min(100);
        let offset = args.offset.unwrap_or(0);
        let state = self.app_handle.state::<AppState>();
        let mut client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        let resp = client
            .get_favorite_tracks(user_id, offset, limit, "DATE", "DESC")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let total = resp.total_number_of_items;
        let tracks = backfill_and_sanitize_tracks(resp.items);
        let json = serde_json::json!({
            "tracks": tracks,
            "total": total,
            "offset": offset,
            "limit": limit,
        });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "get_favorite_albums",
        description = "The user's favourite albums. Params: limit (≤100, default 50), offset. Example: {\"limit\":25}."
    )]
    async fn get_favorite_albums(
        &self,
        Parameters(args): Parameters<PaginatedArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(50).min(100);
        let offset = args.offset.unwrap_or(0);
        let state = self.app_handle.state::<AppState>();
        let mut client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        let resp = client
            .get_favorite_albums(user_id, offset, limit, "DATE", "DESC")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let total = resp.total_number_of_items;
        let albums: Vec<SanitizedAlbum> =
            resp.items.iter().map(SanitizedAlbum::from_tidal).collect();
        let json = serde_json::json!({
            "albums": albums,
            "total": total,
            "offset": offset,
            "limit": limit,
        });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "get_favorite_artists",
        description = "The user's favourite artists. Params: limit (≤100, default 50), offset. Example: {\"limit\":25}."
    )]
    async fn get_favorite_artists(
        &self,
        Parameters(args): Parameters<PaginatedArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let limit = args.limit.unwrap_or(50).min(100);
        let offset = args.offset.unwrap_or(0);
        let state = self.app_handle.state::<AppState>();
        let mut client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        let resp = client
            .get_favorite_artists(user_id, offset, limit, "DATE", "DESC")
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let total = resp.total_number_of_items;
        let artists: Vec<SanitizedArtist> = resp
            .items
            .iter()
            .map(SanitizedArtist::from_tidal_detail)
            .collect();
        let json = serde_json::json!({
            "artists": artists,
            "total": total,
            "offset": offset,
            "limit": limit,
        });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "is_track_favorited",
        description = "Whether a track is in the user's favourites. Params: track_id. Example: {\"track_id\":123}."
    )]
    async fn is_track_favorited(
        &self,
        Parameters(args): Parameters<TrackIdArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        let favorited = client
            .is_track_favorited(user_id, args.track_id)
            .await
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let json = serde_json::json!({ "favorited": favorited });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "favorite_track",
        description = "Add or remove a track from the favourites. Params: id (track id), action = \"add\" or \"remove\". Example: {\"id\":123,\"action\":\"add\"}."
    )]
    async fn favorite_track(
        &self,
        Parameters(args): Parameters<FavoriteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.action != "add" && args.action != "remove" {
            return Err(ErrorData::invalid_params(
                "action must be \"add\" or \"remove\"",
                None,
            ));
        }
        let state = self.app_handle.state::<AppState>();
        let client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        if args.action == "add" {
            client
                .add_favorite_track(user_id, args.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        } else {
            client
                .remove_favorite_track(user_id, args.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }
        drop(client);
        state.disk_cache.invalidate_tag("fav-tracks").await;
        self.app_handle
            .emit(
                EV_FAVORITE_CHANGED,
                FavoriteChangedPayload {
                    kind: "track".to_string(),
                    id: args.id,
                    action: args.action.clone(),
                },
            )
            .map_err(|e| ErrorData::internal_error(format!("emit failed: {e}"), None))?;
        let json = serde_json::json!({ "status": args.action, "id": args.id });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "favorite_album",
        description = "Add or remove an album from the favourites. Params: id (album id), action = \"add\" or \"remove\". Example: {\"id\":77646168,\"action\":\"add\"}."
    )]
    async fn favorite_album(
        &self,
        Parameters(args): Parameters<FavoriteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.action != "add" && args.action != "remove" {
            return Err(ErrorData::invalid_params(
                "action must be \"add\" or \"remove\"",
                None,
            ));
        }
        let state = self.app_handle.state::<AppState>();
        let client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        if args.action == "add" {
            client
                .add_favorite_album(user_id, args.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        } else {
            client
                .remove_favorite_album(user_id, args.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }
        drop(client);
        state.disk_cache.invalidate_tag("fav-albums").await;
        self.app_handle
            .emit(
                EV_FAVORITE_CHANGED,
                FavoriteChangedPayload {
                    kind: "album".to_string(),
                    id: args.id,
                    action: args.action.clone(),
                },
            )
            .map_err(|e| ErrorData::internal_error(format!("emit failed: {e}"), None))?;
        let json = serde_json::json!({ "status": args.action, "id": args.id });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }

    #[rmcp::tool(
        name = "favorite_artist",
        description = "Add or remove an artist from the favourites. Params: id (artist id), action = \"add\" or \"remove\". Example: {\"id\":1566,\"action\":\"add\"}."
    )]
    async fn favorite_artist(
        &self,
        Parameters(args): Parameters<FavoriteArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        if args.action != "add" && args.action != "remove" {
            return Err(ErrorData::invalid_params(
                "action must be \"add\" or \"remove\"",
                None,
            ));
        }
        let state = self.app_handle.state::<AppState>();
        let client = state.tidal_client.lock().await;
        let user_id = require_user_id(&client)?;
        if args.action == "add" {
            client
                .add_favorite_artist(user_id, args.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        } else {
            client
                .remove_favorite_artist(user_id, args.id)
                .await
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        }
        drop(client);
        state.disk_cache.invalidate_tag("fav-artists").await;
        self.app_handle
            .emit(
                EV_FAVORITE_CHANGED,
                FavoriteChangedPayload {
                    kind: "artist".to_string(),
                    id: args.id,
                    action: args.action.clone(),
                },
            )
            .map_err(|e| ErrorData::internal_error(format!("emit failed: {e}"), None))?;
        let json = serde_json::json!({ "status": args.action, "id": args.id });
        Ok(CallToolResult::success(vec![
            rmcp::model::ContentBlock::text(json.to_string()),
        ]))
    }
}
