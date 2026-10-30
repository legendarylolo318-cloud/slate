use std::net::SocketAddr;
use std::sync::Arc;

use crate::AppHandle;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, ListResourcesResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
    ReadResourceResult, Resource, ResourceContents, ServerCapabilities, ServerInfo,
    SubscribeRequestParams, UnsubscribeRequestParams,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ErrorData;
use tokio_util::sync::CancellationToken;

use crate::error::SlateError;
use crate::mcp::policy::{self, Decision};
use crate::mcp::runtime::{Runtime, URI_EVENTS, URI_NOW_PLAYING, URI_QUEUE};
use crate::AppState;

#[derive(Clone)]
pub struct SlateMcpServer {
    pub(crate) app_handle: AppHandle,
    pub(crate) tool_router: ToolRouter<Self>,
    pub(crate) rt: Arc<Runtime>,
}

/// The connecting agent's name from the `initialize` handshake.
pub(crate) fn client_name(ctx: &RequestContext<RoleServer>) -> String {
    ctx.peer
        .peer_info()
        .map(|i| i.client_info.name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "An agent".to_string())
}

/// Identity of a connection, stable for its lifetime.
fn peer_key(ctx: &RequestContext<RoleServer>) -> usize {
    ctx.peer.peer_info().map_or(0, |i| Arc::as_ptr(&i) as usize)
}

fn text(s: impl Into<String>) -> Vec<ContentBlock> {
    vec![ContentBlock::text(s)]
}

impl SlateMcpServer {
    /// One tool call with every check applied: unknown tool, read-only mode, `confirm`, the
    /// user's approval. Mutating calls are logged. `batch` runs each inner call through here.
    pub(crate) async fn guarded(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let name = request.name.to_string();
        let client = client_name(&ctx);
        let confirm = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("confirm"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        match policy::decide(&self.rt.policy(), &name, confirm) {
            Decision::Allow => {}
            Decision::Reject(why) => return Ok(CallToolResult::error(text(why))),
            Decision::Describe => {
                let what = self.describe(&request).await;
                return Ok(CallToolResult::success(text(
                    serde_json::json!({
                        "done": false,
                        "would": what,
                        "note": "Nothing was changed. Call again with \"confirm\": true to do it.",
                    })
                    .to_string(),
                )));
            }
            Decision::AskUser => {
                let what = self.describe(&request).await;
                if !self.rt.ask_user(&self.app_handle, &client, &what).await {
                    return Ok(CallToolResult::error(text(format!(
                        "The user did not allow this: {client} wants to {what}."
                    ))));
                }
            }
        }
        let before = self.capture_before(&request).await;
        let args = request.arguments.clone();
        let result = self
            .tool_router
            .call(ToolCallContext::new(self, request, ctx))
            .await;
        if policy::is_mutating(&name) && name != "batch" {
            self.record(&client, &name, args.as_ref(), before, &result);
        }
        result
    }
}

impl ServerHandler for SlateMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_subscribe()
                .build(),
        )
        .with_protocol_version(ProtocolVersion::V_2024_11_05)
        .with_server_info(Implementation::new("slate", env!("CARGO_PKG_VERSION")))
        .with_instructions(
            "Slate — Tidal music player. Tools cover catalog search, favorites, playlists, \
             playback and queue control. Destructive tools need \"confirm\": true and may wait \
             for the user to allow them in the app. Subscribe to slate://now-playing, \
             slate://queue and slate://events for changes (track_skipped, track_replayed, \
             queue_low).",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let policy = self.rt.policy();
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| policy::visible(&policy, &t.name))
            .collect();
        Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        self.guarded(request, context).await
    }

    fn get_tool(&self, name: &str) -> Option<rmcp::model::Tool> {
        self.tool_router
            .get(name)
            .filter(|t| policy::visible(&self.rt.policy(), &t.name))
            .cloned()
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let r = |uri: &str, name: &str, desc: &str| {
            Resource::new(uri, name)
                .with_description(desc)
                .with_mime_type("application/json")
        };
        Ok(ListResourcesResult::with_all_items(vec![
            r(
                URI_NOW_PLAYING,
                "Now playing",
                "The playing track, position and play state.",
            ),
            r(
                URI_QUEUE,
                "Queue",
                "The upcoming tracks with their total duration.",
            ),
            r(
                URI_EVENTS,
                "Events",
                "Recent track_skipped, track_replayed and queue_low events.",
            ),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, ErrorData> {
        let state = self.app_handle.state::<AppState>();
        let body = match request.uri.as_str() {
            URI_NOW_PLAYING => self.now_playing_json(&state).await.to_string(),
            URI_QUEUE => self.queue_json(&state, 50, 0).await.to_string(),
            URI_EVENTS => serde_json::json!({ "events": self
                .rt
                .recent_events()
                .iter()
                .filter_map(|e| serde_json::from_str::<serde_json::Value>(e).ok())
                .collect::<Vec<_>>() })
            .to_string(),
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("No resource {other}. Try slate://now-playing, slate://queue or slate://events."),
                    None,
                ))
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            body,
            request.uri,
        )
        .with_mime_type("application/json")]))
    }

    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if !matches!(
            request.uri.as_str(),
            URI_NOW_PLAYING | URI_QUEUE | URI_EVENTS
        ) {
            return Err(ErrorData::resource_not_found(
                format!("Cannot subscribe to {}.", request.uri),
                None,
            ));
        }
        self.rt
            .subscribe(peer_key(&context), context.peer.clone(), &request.uri);
        Ok(())
    }

    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.rt.unsubscribe(peer_key(&context), &request.uri);
        Ok(())
    }
}

pub struct McpHandle {
    pub(crate) port: u16,
    pub(crate) token: String,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
    app: AppHandle,
    rt: Arc<Runtime>,
}

impl McpHandle {
    /// The running server's policy, token and activity log.
    pub fn runtime(&self) -> Arc<Runtime> {
        Arc::clone(&self.rt)
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/{}/mcp", self.port, self.token)
    }

    /// Stop the server and wait until the port is fully released. Must
    /// complete before rebinding the same port (regenerate/enable paths).
    /// Everything the server owned is dropped with it.
    pub async fn shutdown(self) {
        let state = self.app.try_state();
        if let Some(state) = &state {
            state
                .mcp_active
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
        crate::http_util::shutdown_bounded(self.cancel, self.task, "MCP").await;
        // The runtime goes with the server: log saved, subscribers and watcher dropped.
        let rt = state.and_then(|s| s.mcp_rt.write().ok().and_then(|mut g| g.take()));
        if let Some(rt) = rt {
            rt.persist();
        }
    }
}

pub async fn start_server(
    app_handle: AppHandle,
    port: u16,
    token: String,
) -> Result<McpHandle, SlateError> {
    let cancel = CancellationToken::new();
    let state = app_handle.try_state();
    let settings = state
        .as_ref()
        .and_then(|s| s.load_settings())
        .unwrap_or_default();
    let rt = Arc::new(Runtime::new(
        policy::Policy {
            read_only: settings.mcp_read_only,
            ask_destructive: settings.mcp_ask_destructive,
        },
        settings.mcp_bearer.clone(),
        settings.mcp_agent_dj,
        crate::config_dir_for_env(),
    ));

    let server = SlateMcpServer {
        app_handle: app_handle.clone(),
        tool_router: crate::mcp::tools::build_router(),
        rt: Arc::clone(&rt),
    };

    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        // Stateful, so clients get a session: the `initialize` name is remembered and
        // resource subscriptions can push notifications. Same URL and protections as before.
        StreamableHttpServerConfig::default(),
    );

    let bearer_rt = Arc::clone(&rt);
    let handle_rt = Arc::clone(&rt);
    let path = format!("/{}/mcp", token);
    let app = axum::Router::new()
        .nest_service(&path, service)
        .layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                let rt = Arc::clone(&bearer_rt);
                async move {
                    let expected = rt.bearer();
                    if !expected.is_empty() {
                        let given = req
                            .headers()
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok());
                        if !policy::bearer_ok(given, &expected) {
                            return axum::http::Response::builder()
                                .status(axum::http::StatusCode::UNAUTHORIZED)
                                .header(axum::http::header::WWW_AUTHENTICATE, "Bearer")
                                .body(axum::body::Body::from("Unauthorized"))
                                .unwrap_or_default();
                        }
                    }
                    next.run(req).await
                }
            },
        ))
        .layer(axum::middleware::from_fn(
            crate::http_util::require_loopback_host,
        ));

    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| SlateError::Mcp(format!("bind {addr}: {e}")))?;
    let bound_port = listener
        .local_addr()
        .map_err(|e| SlateError::Mcp(format!("local_addr: {e}")))?
        .port();

    // The token is the URL path and acts as the credential: never log it.
    log::info!("MCP server listening on http://127.0.0.1:{bound_port}/<token>/mcp");

    let task = crate::http_util::spawn_with_shutdown(listener, app, cancel.clone(), "MCP");

    if let Some(state) = &state {
        if let Ok(mut slot) = state.mcp_rt.write() {
            *slot = Some(rt);
        }
        state
            .mcp_active
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    Ok(McpHandle {
        port: bound_port,
        token,
        cancel,
        task,
        app: app_handle,
        rt: handle_rt,
    })
}
