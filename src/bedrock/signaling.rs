// HTTP signaling: GET /v1/join for server info, POST /v1/join/{networkId} for the
// SDP offer/answer exchange.

use super::{Bedrock, PROTOCOL_VERSION, VERSION_NAME, peer};
use crate::server::GameMode;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
use tracing::warn;

pub fn router(bedrock: Arc<Bedrock>) -> Router {
    Router::new()
        .route("/v1/join", get(server_info))
        .route("/v1/join/{network_id}", post(join))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(bedrock)
}

async fn server_info(State(bedrock): State<Arc<Bedrock>>) -> Json<Value> {
    let server = &bedrock.server;
    Json(json!({
        "name": server.config.motd,
        "protocol": PROTOCOL_VERSION,
        "version": VERSION_NAME,
        "level": "world",
        "players": server.players.count(),
        "maxPlayers": server.config.max_players,
        // Bedrock has no spectator here; 0 survival, 1 creative, 2 adventure.
        "gameType": match server.config.gamemode {
            GameMode::Creative => 1,
            GameMode::Adventure => 2,
            _ => 0,
        },
    }))
}

async fn join(State(bedrock): State<Arc<Bedrock>>, Path(network_id): Path<String>, offer: String) -> Response {
    // Peers that never finish connecting still hold a socket until they time out.
    let limit = (bedrock.server.config.max_players.max(1) as usize) * 2;
    if bedrock.server.bedrock_peers.load(Ordering::Relaxed) >= limit {
        return (StatusCode::SERVICE_UNAVAILABLE, "server is full").into_response();
    }

    match peer::accept(bedrock, network_id.clone(), offer).await {
        Ok(answer) => ([(CONTENT_TYPE, "application/sdp")], answer).into_response(),
        Err(e) => {
            warn!("NetherNet offer from {network_id} failed: {e:#}");
            (StatusCode::BAD_REQUEST, "bad offer").into_response()
        }
    }
}
