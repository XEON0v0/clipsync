//! Axum wiring: the `/ws` route, trusted-proxy client-IP resolution, the global
//! connection quota, and server startup.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::extract::{ConnectInfo, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Router, serve};
use dashmap::DashMap;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use clipboard_core::protocol::{MAX_FRAME_BYTES, MAX_MESSAGE_BYTES};

use crate::config::ServerConfig;
use crate::connection::handle_connection;
use crate::mailbox::{HealthProbe, MailboxSink};
use crate::pairing::{Connections, PairingHandler};
use crate::ratelimit::IpRateLimiter;
use crate::registry::Registry;
use crate::room::Rooms;

/// Shared server state.
pub struct ServerState {
    pub config: ServerConfig,
    pub registry: Arc<dyn Registry>,
    pub pairing: Arc<dyn PairingHandler>,
    pub rooms: Rooms,
    pub connections: Connections,
    pub connection_permits: Arc<Semaphore>,
    pub rate_limits: IpRateLimiter,
    /// Mailbox persistence health; `/healthz` reports degraded while a
    /// publication is failing. `None` when the sink has no persistence.
    pub mailbox_health: Option<HealthProbe>,
    next_connection_id: AtomicU64,
}

impl ServerState {
    /// Builds shared state from configuration and the T8/T9 seams.
    #[must_use]
    pub fn new(
        config: ServerConfig,
        registry: Arc<dyn Registry>,
        pairing: Arc<dyn PairingHandler>,
        mailbox: Arc<dyn MailboxSink>,
    ) -> Self {
        let mailbox_health = mailbox.health_probe();
        Self {
            rate_limits: IpRateLimiter::new(&config.limits),
            connection_permits: Arc::new(Semaphore::new(config.limits.max_connections)),
            rooms: Rooms::new(
                config.limits.clone(),
                mailbox,
                registry.clone(),
                config.hooks.clone(),
            ),
            config,
            registry,
            pairing,
            connections: DashMap::new(),
            mailbox_health,
            next_connection_id: AtomicU64::new(1),
        }
    }
}

/// Builds the HTTP router.
pub fn router(state: Arc<ServerState>) -> Router {
    Router::new()
        .route("/ws", get(ws_handler))
        .route("/healthz", get(healthz_handler))
        .with_state(state)
}

async fn healthz_handler(State(state): State<Arc<ServerState>>) -> Response {
    match &state.mailbox_health {
        Some(probe) if !probe.is_healthy() => {
            (StatusCode::SERVICE_UNAVAILABLE, "degraded\n").into_response()
        }
        _ => (StatusCode::OK, "ok\n").into_response(),
    }
}

async fn ws_handler(
    State(state): State<Arc<ServerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let client_ip = state.config.client_ip(
        peer.ip(),
        headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok()),
    );
    let Ok(permit) = state.connection_permits.clone().try_acquire_owned() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "server_full").into_response();
    };
    let connection_id = state.next_connection_id.fetch_add(1, Ordering::Relaxed);
    ws
        // Frame and message limits are independently enforced at 24 MiB.
        .max_frame_size(MAX_FRAME_BYTES)
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| async move {
            let _permit = permit;
            handle_connection(state, socket, client_ip, connection_id).await;
        })
}

/// A running server. Aborts the accept loop on drop.
pub struct ServerHandle {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl ServerHandle {
    /// The bound local address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Binds `bind`, serves `/ws`, and returns once the accept loop is running.
///
/// # Errors
/// Returns the bind error from the OS.
pub async fn start(
    bind: SocketAddr,
    config: ServerConfig,
    registry: Arc<dyn Registry>,
    pairing: Arc<dyn PairingHandler>,
    mailbox: Arc<dyn MailboxSink>,
) -> io::Result<ServerHandle> {
    let state = Arc::new(ServerState::new(config, registry, pairing, mailbox));
    let listener = TcpListener::bind(bind).await?;
    let addr = listener.local_addr()?;
    let task = tokio::spawn(async move {
        let make_service = router(state).into_make_service_with_connect_info::<SocketAddr>();
        if let Err(error) = serve(listener, make_service).await {
            eprintln!("relay accept loop failed: {error}");
        }
    });
    Ok(ServerHandle { addr, task })
}
