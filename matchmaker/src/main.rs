//! Bad Spaceship matchmaker — the lobby-coordination tier.
//!
//! A small standalone HTTP service (no bevy/lightyear) that tracks open
//! multiplayer matches and hands out shareable join links. The static lobby
//! browser (`client/lobby.html`) talks to it via `fetch()`. It is hosted
//! separately from the static site (GitHub Pages can't run a server).
//!
//! Endpoints (all JSON):
//!   GET  /api/health            -> "ok"
//!   GET  /api/matches           -> [Lobby, ...]      (open lobbies, newest first)
//!   POST /api/matches           -> { id, join_path } (create a lobby)
//!   POST /api/matches/{id}/join -> Lobby             (claim a slot; 404/409 on miss/full)
//!
//! State is in-memory for now (a process restart clears lobbies). Swappable for
//! Redis/a DB later without touching the front-end contract.
//!
//! Config via env:
//!   BIND                 listen addr (default 0.0.0.0:5000)
//!   STATIC_DIR           if set, also serve this dir at `/` (local single-origin testing)
//!   BS_GAME_SERVER_URL   the dedicated game server's public `wss://host[:port]`
//!                        endpoint, handed out with every match so the lobby's
//!                        join link actually connects the client to it. Unset ⇒
//!                        empty ⇒ the join link is single-player (offline). A
//!                        single shared server for now: every match points at it
//!                        and shares one world (per-room world isolation is a
//!                        later slice).

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tower_http::{cors::CorsLayer, services::ServeDir};

/// One open multiplayer match, as the lobby browser sees it.
#[derive(Clone, Serialize)]
struct Lobby {
    /// Short shareable code, also the join-link token.
    id: String,
    name: String,
    players: u32,
    max_players: u32,
    /// Seconds since the Unix epoch.
    created_unix: u64,
    /// The game server's public `wss://` endpoint for this match (from
    /// `BS_GAME_SERVER_URL`). The front-end threads it into the join link as
    /// `?server=…`, which the WASM client reads on boot to know who to connect
    /// to. Empty ⇒ single-player (no server configured).
    server: String,
}

#[derive(Deserialize)]
struct CreateReq {
    name: Option<String>,
    max_players: Option<u32>,
}

#[derive(Serialize)]
struct CreateResp {
    id: String,
    /// Relative room link (`play.html?room=ID`); the front-end appends the
    /// `&server=…` (it already builds the absolute, shareable URL and owns the
    /// query encoding).
    join_path: String,
    /// The game server endpoint for the new match (echoes `Lobby.server`), so
    /// the front-end can thread it into the join link.
    server: String,
}

type Db = Arc<Mutex<HashMap<String, Lobby>>>;

/// Shared handler state: the in-memory lobby store plus the configured game
/// server endpoint handed out with every match.
#[derive(Clone)]
struct AppState {
    db: Db,
    game_server: Arc<str>,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Unambiguous lobby code: no 0/O/1/I/L, uppercase, 6 chars.
fn gen_id() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

async fn health() -> &'static str {
    "ok"
}

async fn list_matches(State(state): State<AppState>) -> Json<Vec<Lobby>> {
    let db = state.db.lock().unwrap();
    let mut lobbies: Vec<Lobby> = db.values().cloned().collect();
    // Newest first.
    lobbies.sort_by(|a, b| b.created_unix.cmp(&a.created_unix));
    Json(lobbies)
}

async fn create_match(
    State(state): State<AppState>,
    body: Option<Json<CreateReq>>,
) -> impl IntoResponse {
    let req = body.map(|Json(b)| b).unwrap_or(CreateReq {
        name: None,
        max_players: None,
    });

    let mut db = state.db.lock().unwrap();
    // Avoid the (astronomically unlikely) id collision.
    let id = loop {
        let candidate = gen_id();
        if !db.contains_key(&candidate) {
            break candidate;
        }
    };

    let name = req
        .name
        .map(|n| n.trim().chars().take(40).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("Match {id}"));
    let max_players = req.max_players.unwrap_or(8).clamp(2, 32);
    let server = state.game_server.to_string();

    let lobby = Lobby {
        id: id.clone(),
        name,
        players: 1,
        max_players,
        created_unix: now_unix(),
        server: server.clone(),
    };
    db.insert(id.clone(), lobby);

    (
        StatusCode::CREATED,
        Json(CreateResp {
            join_path: format!("play.html?room={id}"),
            id,
            server,
        }),
    )
}

async fn join_match(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let id = id.to_uppercase();
    let mut db = state.db.lock().unwrap();
    match db.get_mut(&id) {
        None => (StatusCode::NOT_FOUND, "no such match").into_response(),
        Some(lobby) if lobby.players >= lobby.max_players => {
            (StatusCode::CONFLICT, "match is full").into_response()
        }
        Some(lobby) => {
            lobby.players += 1;
            Json(lobby.clone()).into_response()
        }
    }
}

#[tokio::main]
async fn main() {
    let db: Db = Arc::new(Mutex::new(HashMap::new()));
    // The dedicated game server endpoint handed out with every match. Empty when
    // unset, so the lobby flow degrades to single-player rather than a dead link.
    let game_server: Arc<str> = std::env::var("BS_GAME_SERVER_URL")
        .unwrap_or_default()
        .trim()
        .into();
    if game_server.is_empty() {
        println!(
            "warning: BS_GAME_SERVER_URL is unset — matches will have no server \
             (single-player join links). Set it to the game server's wss:// URL."
        );
    } else {
        println!("handing out game server endpoint: {game_server}");
    }
    let state = AppState { db, game_server };

    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/matches", get(list_matches).post(create_match))
        .route("/api/matches/{id}/join", post(join_match))
        // Permissive CORS for now (the lobby page lives on a different origin).
        // Lock this down to the deployed site origin before going public.
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Optional: serve the static client dir on the same origin for local
    // end-to-end testing (no CORS needed). Off in production — Pages serves it.
    if let Ok(dir) = std::env::var("STATIC_DIR") {
        app = app.fallback_service(ServeDir::new(dir));
    }

    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:5000".to_string());
    let addr: SocketAddr = bind.parse().expect("BIND must be host:port");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("matchmaker listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
