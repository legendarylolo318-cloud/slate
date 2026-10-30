pub(crate) mod agent;
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
        + SlateMcpServer::agent_tools()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::policy;

    /// Every tool the router serves is classified in the policy table and vice versa, so a
    /// new tool cannot slip past read-only mode or the destructive-action checks.
    #[test]
    fn the_router_and_the_policy_table_list_the_same_tools() {
        let router = build_router();
        let mut served: Vec<String> = router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        served.sort();
        let mut classified: Vec<String> = policy::TOOLS
            .iter()
            .map(|(n, _)| (*n).to_string())
            .collect();
        classified.sort();
        assert_eq!(served, classified);
    }

    #[test]
    fn every_tool_description_has_an_example_call() {
        let router = build_router();
        for t in router.list_all() {
            let d = t.description.as_deref().unwrap_or("");
            assert!(
                d.contains("Example"),
                "{} has no example in its description",
                t.name
            );
        }
    }
}
