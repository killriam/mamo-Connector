//! Local HTTP simulation server.
//!
//! Listens on `127.0.0.1:52340` and exposes:
//!   GET  /health   → 200 `{"ok":true}`
//!   POST /simulate → accepts same binary wire format as WASM worker,
//!                    runs mamo-sim natively, returns same JSON as Vercel path.
//!
//! The server is spawned as a background tokio task alongside the UI.
//! It is unreachable from outside the machine (loopback only).

use anyhow::Result;
use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use log::{error, info};
use serde::Deserialize;
use tower_http::cors::{Any, CorsLayer};

pub const LOCAL_SIM_PORT: u16 = 52340;

// ── Request / response types ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SimulateRequest {
    /// Base64-encoded binary wire format (same as ArrayBuffer sent to WASM workers).
    pub encoded: String,
    /// Mechanic group keys in index order.
    pub mech_keys: Vec<String>,
    /// Number of games to simulate.
    pub games: u32,
    /// Max turns per game.
    pub max_turns: u8,
    /// RNG seed.
    pub seed: u32,
}

// ── Handlers ──────────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    (StatusCode::OK, Json(serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") })))
}

async fn simulate(Json(req): Json<SimulateRequest>) -> impl IntoResponse {
    // Decode base64 payload
    let bytes = match BASE64.decode(&req.encoded) {
        Ok(b) => b,
        Err(e) => {
            error!("local_server: base64 decode error: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("base64 decode failed: {}", e) })),
            );
        }
    };

    // Run the simulation natively (no WASM overhead — pure Rust)
    let json_str = mamo_sim::run_batch_native(&bytes, req.mech_keys, req.games, req.max_turns, req.seed);

    let metrics: serde_json::Value = match serde_json::from_str(&json_str) {
        Ok(v) => v,
        Err(e) => {
            error!("local_server: failed to parse simulation output: {}", e);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": format!("simulation output parse failed: {}", e) })),
            );
        }
    };

    // Inject _source tag into the metrics object
    let mut obj = metrics.as_object().cloned().unwrap_or_default();
    obj.insert("_source".to_string(), serde_json::Value::String("mamo-connector".to_string()));

    (StatusCode::OK, Json(serde_json::Value::Object(obj)))
}

// ── Server startup ────────────────────────────────────────────────────────────

/// Spawn the local simulation HTTP server as a background tokio task.
/// Returns immediately — the server runs until the process exits.
pub fn spawn(runtime: &tokio::runtime::Handle) {
    runtime.spawn(async move {
        if let Err(e) = run_server().await {
            error!("local_server: fatal error: {}", e);
        }
    });
}

async fn run_server() -> Result<()> {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/health", get(health))
        .route("/simulate", post(simulate))
        .layer(cors);

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], LOCAL_SIM_PORT));
    info!("local_server: listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
