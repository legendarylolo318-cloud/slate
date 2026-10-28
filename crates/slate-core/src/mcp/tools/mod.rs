mod catalog;
mod favorites;
mod playback;
mod playlists;
mod state;
mod util;

use rmcp::handler::server::router::tool::ToolRouter;

use crate::mcp::server::SlateMcpServer;

pub fn build_router() -> ToolRouter<SlateMcpServer> {
    SlateMcpServer::catalog_tools()
        + SlateMcpServer::favorites_tools()
        + SlateMcpServer::playlists_tools()
        + SlateMcpServer::state_tools()
        + SlateMcpServer::playback_tools()
}
