use std::net::SocketAddr;
use std::sync::Arc;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use futures::{SinkExt, StreamExt};
use tracing::{error, info};

use harness_core::EventBus;

const INDEX_HTML: &str = include_str!("assets/index.html");

#[derive(Clone)]
struct AppState {
    event_bus: EventBus,
}

pub async fn start_web_dashboard(bus: EventBus, port: u16) -> Result<(), String> {
    let state = Arc::new(AppState { event_bus: bus });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("Starting Dex-Harness Web Dashboard at http://localhost:{port}");

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("Failed to bind to port {port}: {e}"))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| format!("Web server error: {e}"))
}

async fn serve_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut _receiver) = socket.split();
    let mut bus_rx = state.event_bus.subscribe();

    while let Ok(event) = bus_rx.recv().await {
        if let Ok(json_str) = serde_json::to_string(&event) {
            if let Err(e) = sender.send(Message::Text(json_str.into())).await {
                error!("WebSocket client disconnected: {e}");
                break;
            }
        }
    }
}
