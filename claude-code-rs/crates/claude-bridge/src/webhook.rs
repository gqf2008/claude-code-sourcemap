//! Webhook HTTP server — receives platform callbacks via HTTP.
//!
//! Provides an axum-based HTTP server that routes webhook requests
//! from various platforms to their respective adapters.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use tokio::sync::Mutex;
use tracing::{error, info};

use crate::gateway::GatewayContext;
use crate::message::{ChannelId, InboundMessage, SenderInfo};

/// Shared state for webhook handlers.
#[derive(Clone)]
#[allow(dead_code)]
struct WebhookState {
    /// Gateway context for routing inbound messages.
    ctx: GatewayContext,
    /// Registered webhook handlers by platform.
    handlers: Arc<Mutex<Vec<WebhookHandler>>>,
}

/// A registered webhook handler for a specific platform.
#[allow(dead_code)]
struct WebhookHandler {
    /// Platform name.
    platform: String,
    /// URL path prefix for this platform's webhooks.
    path_prefix: String,
}

/// Webhook server configuration.
pub struct WebhookServer {
    addr: SocketAddr,
    ctx: GatewayContext,
}

impl WebhookServer {
    /// Create a new webhook server.
    pub fn new(addr: SocketAddr, ctx: GatewayContext) -> Self {
        Self { addr, ctx }
    }

    /// Build the axum router.
    fn build_router(&self) -> Router {
        let state = WebhookState {
            ctx: self.ctx.clone(),
            handlers: Arc::new(Mutex::new(vec![])),
        };

        Router::new()
            .route("/health", get(health))
            .route("/webhook/{platform}", post(handle_webhook))
            .with_state(state)
    }

    /// Start the webhook server.
    ///
    /// Returns when the server is shut down or encounters an error.
    pub async fn serve(self) -> Result<(), std::io::Error> {
        let router = self.build_router();
        info!("Webhook server starting on {}", self.addr);

        let listener = tokio::net::TcpListener::bind(self.addr).await?;
        axum::serve(listener, router)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))
    }
}

/// Health check endpoint.
async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// Generic webhook handler — routes based on platform path parameter.
///
/// POST /webhook/{platform}
///
/// The request body is platform-specific JSON. Each platform adapter
/// provides its own parsing logic via the ChannelAdapter trait.
/// For now, this provides a simple text extraction from a generic format:
///
/// ```json
/// {
///   "channel_id": "...",
///   "user_id": "...",
///   "user_name": "...",
///   "text": "..."
/// }
/// ```
async fn handle_webhook(
    Path(platform): Path<String>,
    State(state): State<WebhookState>,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    let channel = body.get("channel_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let user_id = body.get("user_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let user_name = body.get("user_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let text = body.get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    if text.is_empty() {
        return (StatusCode::BAD_REQUEST, "Missing text field");
    }

    let msg = InboundMessage::text(
        ChannelId::new(&platform, channel),
        SenderInfo::new(user_id, user_name),
        text,
    );

    match state.ctx.route_inbound(msg) {
        Ok(_) => (StatusCode::OK, "ok"),
        Err(e) => {
            error!("Failed to route webhook message: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn make_ctx() -> GatewayContext {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        GatewayContext { inbound_tx: tx }
    }

    #[tokio::test]
    async fn health_check() {
        let server = WebhookServer::new(
            "127.0.0.1:0".parse().unwrap(),
            make_ctx(),
        );
        let app = server.build_router();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn webhook_post() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let ctx = GatewayContext { inbound_tx: tx };

        let server = WebhookServer::new(
            "127.0.0.1:0".parse().unwrap(),
            ctx,
        );
        let app = server.build_router();

        let body = serde_json::json!({
            "channel_id": "ch_123",
            "user_id": "u_456",
            "user_name": "Alice",
            "text": "Hello from webhook!"
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/feishu")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let msg = rx.try_recv().unwrap();
        assert_eq!(msg.text, "Hello from webhook!");
        assert_eq!(msg.channel_id.platform, "feishu");
        assert_eq!(msg.channel_id.channel, "ch_123");
        assert_eq!(msg.sender.user_id, "u_456");
    }

    #[tokio::test]
    async fn webhook_missing_text() {
        let server = WebhookServer::new(
            "127.0.0.1:0".parse().unwrap(),
            make_ctx(),
        );
        let app = server.build_router();

        let body = serde_json::json!({
            "channel_id": "ch_123",
            "user_id": "u_456",
        });

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/test")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_string(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
