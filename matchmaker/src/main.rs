//! Bad Spaceship matchmaker — the version-aware lobby/router tier.
//!
//! A small standalone HTTP service (no bevy/lightyear) that tracks open matches
//! and routes each one to the game-server **version** it was created on, so a
//! room's frontend and backend always match even as new versions deploy.
//!
//! Two on-disk files form the contract with the deploy automation:
//!   * `BS_REGISTRY`  (default ~/bs-mac/versions/registry.json) — WRITTEN by the
//!     deploy scripts, READ here. Lists the live server versions and which is
//!     `latest`. New matches land on `latest`; a room whose version is no longer
//!     listed has been retired (→ 410).
//!   * `BS_ROOMS_STATE` (default ~/bs-mac/versions/rooms.json) — WRITTEN + READ
//!     here. Persists each room's pinned version so a matchmaker restart doesn't
//!     orphan live rooms.
//!
//! Endpoints (all JSON):
//!   GET  /api/health                 -> "ok"
//!   GET  /api/matches                -> [LobbyView, ...]   (rooms on still-live versions, newest first)
//!   POST /api/matches                -> { id, sha, server, web_url, share_url } | 503 no_active_version
//!   POST /api/matches/{id}/join      -> LobbyView | 404 | 409 | 410 version_retired
//!   GET  /api/matches/{id}/resolve   -> { sha, server, web_url } | 404 | 410 version_retired
//!   GET  /api/saves                  -> [SaveView, ...]    (saved games on disk, newest first)
//!   POST /api/saves/load {file}      -> CreateResp | 400 | 404 | 422 | 503 no_active_version
//!
//! Saved games (`BS_SAVES_DIR`, default ~/bs-mac/versions/saves) are written by the
//! *game server* (a rolling `auto-<CODE>.json` per occupied room + player-named
//! `manual-*.json` files); the matchmaker shares the directory via the same env var.
//! It reads only each file's stable `{version, meta}` envelope (stable across save
//! versions — the versioned `world` payload is the game server's business, migrated
//! there on load). "Load" = create a fresh match on the latest version and stage a
//! copy of the chosen save as `pending-<NEWCODE>.json`, which that game server
//! consumes when it first creates the room.
//!
//! Public URL shape (origins configurable via env):
//!   server   wss://{BS_GAME_ORIGIN}/v/{sha}/ws
//!   web_url  https://{BS_GAME_ORIGIN}/v/{sha}/play.html?room={code}
//!   share    {BS_LOBBY_BASE}/j.html?room={code}

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

/// A room's persisted state (the matchmaker owns this; see `rooms.json`).
#[derive(Clone, Serialize, Deserialize)]
struct Room {
    name: String,
    players: u32,
    max_players: u32,
    created_unix: u64,
    /// The git SHA / version this room is pinned to. Frontend and backend both
    /// resolve from it, so the room stays internally consistent for its lifetime.
    sha: String,
}

/// What the lobby browser / join flow sees.
#[derive(Serialize)]
struct LobbyView {
    id: String,
    name: String,
    players: u32,
    max_players: u32,
    created_unix: u64,
    sha: String,
    /// `wss://…/v/<sha>/ws` — the version's game endpoint.
    server: String,
    /// Absolute play URL for this room on its matched version.
    web_url: String,
}

#[derive(Serialize)]
struct CreateResp {
    id: String,
    sha: String,
    server: String,
    web_url: String,
    /// Short, version-agnostic link to share; resolves the version on open.
    share_url: String,
}

#[derive(Deserialize)]
struct CreateReq {
    name: Option<String>,
    max_players: Option<u32>,
}

// ---- Saved games -----------------------------------------------------------

/// The stable envelope of a save file — everything the matchmaker reads. Kept
/// deliberately minimal: `version` + `meta` never change meaning across save
/// versions (the game server's documented format contract), so listing works for
/// any save without knowing its world schema.
#[derive(Deserialize)]
struct SaveEnvelope {
    version: u32,
    meta: SaveMeta,
}

#[derive(Serialize, Deserialize)]
struct SaveMeta {
    name: String,
    room_code: String,
    kind: String,
    saved_unix: u64,
}

/// What the lobby's saved-games tab sees: the envelope plus the `file` handle
/// passed back to `POST /api/saves/load` (`meta` is flattened, so the JSON is
/// one level — a new envelope field flows through without touching this).
#[derive(Serialize)]
struct SaveView {
    file: String,
    version: u32,
    #[serde(flatten)]
    meta: SaveMeta,
}

#[derive(Deserialize)]
struct LoadReq {
    file: String,
}

// ---- Deploy registry (read-only here) -------------------------------------

#[derive(Default, Deserialize)]
struct Registry {
    latest: Option<String>,
    #[serde(default)]
    versions: HashMap<String, VersionEntry>,
}

#[derive(Deserialize)]
struct VersionEntry {
    /// building | active | draining | failed. Only `active` accepts new matches;
    /// `active` + `draining` keep serving existing rooms.
    #[serde(default)]
    status: String,
}

impl Registry {
    /// The version new matches should use: `latest`, if it's `active`.
    fn current(&self) -> Option<String> {
        let sha = self.latest.as_ref()?;
        let v = self.versions.get(sha)?;
        (v.status == "active").then(|| sha.clone())
    }
    /// A version still backs existing rooms while it's present at all (active or
    /// draining). Absent ⇒ retired ⇒ rooms on it are dead (410).
    fn serves(&self, sha: &str) -> bool {
        self.versions.contains_key(sha)
    }
}

// ---- Shared state ----------------------------------------------------------

#[derive(Clone)]
struct AppState {
    rooms: Arc<Mutex<HashMap<String, Room>>>,
    registry_path: String,
    rooms_path: String,
    saves_dir: String,
    game_origin: Arc<str>, // e.g. "game.badspaceship.com:7443"
    lobby_base: Arc<str>,  // e.g. "https://badspaceship.com"
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

fn load_registry(path: &str) -> Registry {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn load_rooms(path: &str) -> HashMap<String, Room> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Atomic write (temp + rename in the same dir, so the rename can't cross
/// filesystems) — a crash never leaves a half-written file and readers never see
/// a partial one. Shared by the rooms persistence and save staging.
fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Persist the rooms map (see `atomic_write` — a crash never leaves a
/// half-written rooms file).
fn save_rooms(path: &str, rooms: &HashMap<String, Room>) {
    if let Ok(json) = serde_json::to_string_pretty(rooms) {
        let _ = atomic_write(std::path::Path::new(path), json.as_bytes());
    }
}

fn server_url(origin: &str, sha: &str) -> String {
    format!("wss://{origin}/v/{sha}/ws")
}
fn web_url(origin: &str, sha: &str, code: &str) -> String {
    format!("https://{origin}/v/{sha}/play.html?room={code}")
}
fn share_url(lobby_base: &str, code: &str) -> String {
    format!("{lobby_base}/j.html?room={code}")
}

fn view(state: &AppState, id: &str, room: &Room) -> LobbyView {
    LobbyView {
        id: id.to_string(),
        name: room.name.clone(),
        players: room.players,
        max_players: room.max_players,
        created_unix: room.created_unix,
        sha: room.sha.clone(),
        server: server_url(&state.game_origin, &room.sha),
        web_url: web_url(&state.game_origin, &room.sha, id),
    }
}

// ---- Handlers --------------------------------------------------------------

async fn health() -> &'static str {
    "ok"
}

/// Open lobbies whose version is still live, newest first.
async fn list_matches(State(state): State<AppState>) -> Json<Vec<LobbyView>> {
    let reg = load_registry(&state.registry_path);
    let rooms = state.rooms.lock().unwrap();
    let mut out: Vec<LobbyView> = rooms
        .iter()
        .filter(|(_, r)| reg.serves(&r.sha))
        .map(|(id, r)| view(&state, id, r))
        .collect();
    out.sort_by(|a, b| b.created_unix.cmp(&a.created_unix));
    Json(out)
}

/// New rooms default to this capacity (`create_match` accepts an override).
const DEFAULT_MAX_PLAYERS: u32 = 8;

/// Insert a fresh room under a newly-generated unique code, returning the code.
/// Owns the room-naming policy: trims + caps the requested name; empty/absent ⇒
/// the "Match <code>" auto-name. Shared by match creation and save loading; the
/// caller persists (`save_rooms`) after any other mutations.
fn alloc_room(
    rooms: &mut HashMap<String, Room>,
    name: Option<String>,
    max_players: u32,
    sha: String,
) -> String {
    let id = loop {
        let candidate = gen_id();
        if !rooms.contains_key(&candidate) {
            break candidate;
        }
    };
    let name = name
        .map(|n| n.trim().chars().take(40).collect::<String>())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("Match {id}"));
    let room = Room {
        name,
        players: 1,
        max_players,
        created_unix: now_unix(),
        sha,
    };
    rooms.insert(id.clone(), room);
    id
}

async fn create_match(
    State(state): State<AppState>,
    body: Option<Json<CreateReq>>,
) -> impl IntoResponse {
    let req = body.map(|Json(b)| b).unwrap_or(CreateReq {
        name: None,
        max_players: None,
    });

    // Assign to the current live version. None yet (e.g. first deploy still
    // building) ⇒ 503 so the lobby retries instead of making a dead room.
    let reg = load_registry(&state.registry_path);
    let Some(sha) = reg.current() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "no_active_version" })),
        )
            .into_response();
    };

    let mut rooms = state.rooms.lock().unwrap();
    let max_players = req.max_players.unwrap_or(DEFAULT_MAX_PLAYERS).clamp(2, 32);
    let id = alloc_room(&mut rooms, req.name, max_players, sha.clone());
    save_rooms(&state.rooms_path, &rooms);

    (
        StatusCode::CREATED,
        Json(CreateResp {
            server: server_url(&state.game_origin, &sha),
            web_url: web_url(&state.game_origin, &sha, &id),
            share_url: share_url(&state.lobby_base, &id),
            id,
            sha,
        }),
    )
        .into_response()
}

async fn join_match(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let id = id.to_uppercase();
    let reg = load_registry(&state.registry_path);
    let mut rooms = state.rooms.lock().unwrap();
    match rooms.get_mut(&id) {
        None => (StatusCode::NOT_FOUND, "no such match").into_response(),
        Some(room) if !reg.serves(&room.sha) => (
            StatusCode::GONE,
            Json(serde_json::json!({ "error": "version_retired" })),
        )
            .into_response(),
        Some(room) if room.players >= room.max_players => {
            (StatusCode::CONFLICT, "match is full").into_response()
        }
        Some(room) => {
            room.players += 1;
            let v = view(&state, &id, room);
            save_rooms(&state.rooms_path, &rooms);
            Json(v).into_response()
        }
    }
}

/// Short-link / direct-link resolution: map a room code to its versioned play
/// URL, or 410 if the version has been retired (so the front-end shows a clean
/// "this game has ended" instead of a dead connect).
async fn resolve_match(State(state): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    let id = id.to_uppercase();
    let reg = load_registry(&state.registry_path);
    let rooms = state.rooms.lock().unwrap();
    match rooms.get(&id) {
        None => (StatusCode::NOT_FOUND, "no such match").into_response(),
        Some(room) if !reg.serves(&room.sha) => (
            StatusCode::GONE,
            Json(serde_json::json!({ "error": "version_retired" })),
        )
            .into_response(),
        Some(room) => Json(serde_json::json!({
            "sha": room.sha,
            "server": server_url(&state.game_origin, &room.sha),
            "web_url": web_url(&state.game_origin, &room.sha, &id),
        }))
        .into_response(),
    }
}

/// A save-file name as handed out by `list_saves` / accepted by `load_save`:
/// a bare `auto-*` / `manual-*` `.json` file name, never a path. Rejecting
/// anything else keeps `load_save` from reading outside the saves dir.
fn valid_save_file(file: &str) -> bool {
    (file.starts_with("auto-") || file.starts_with("manual-"))
        && file.ends_with(".json")
        && file.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !file.contains("..")
}

/// Saved games on disk, newest first. Files that don't parse (mid-write, foreign,
/// or corrupt) are simply skipped — listing must never 500 over one bad file.
async fn list_saves(State(state): State<AppState>) -> Json<Vec<SaveView>> {
    let mut out: Vec<SaveView> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&state.saves_dir) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if !valid_save_file(&file) {
                continue;
            }
            let Some(envelope) = std::fs::read_to_string(entry.path())
                .ok()
                .and_then(|s| serde_json::from_str::<SaveEnvelope>(&s).ok())
            else {
                continue;
            };
            out.push(SaveView { file, version: envelope.version, meta: envelope.meta });
        }
    }
    out.sort_by(|a, b| b.meta.saved_unix.cmp(&a.meta.saved_unix));
    Json(out)
}

/// Load a saved game: create a fresh match on the current version (exactly like
/// `create_match`) and stage a copy of the chosen save as `pending-<NEWCODE>.json`
/// in the saves dir, which the game server consumes when the first player's input
/// creates the room — spawning the saved world instead of a random one. Loading an
/// old-version save on a newer server is expected: the server migrates on load.
async fn load_save(State(state): State<AppState>, Json(req): Json<LoadReq>) -> impl IntoResponse {
    if !valid_save_file(&req.file) {
        return (StatusCode::BAD_REQUEST, "invalid save file name").into_response();
    }
    let path = std::path::Path::new(&state.saves_dir).join(&req.file);
    let Ok(bytes) = std::fs::read(&path) else {
        return (StatusCode::NOT_FOUND, "no such save").into_response();
    };
    let Ok(envelope) = serde_json::from_slice::<SaveEnvelope>(&bytes) else {
        return (StatusCode::UNPROCESSABLE_ENTITY, "save file unreadable").into_response();
    };

    let reg = load_registry(&state.registry_path);
    let Some(sha) = reg.current() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "no_active_version" })),
        )
            .into_response();
    };

    let mut rooms = state.rooms.lock().unwrap();
    let name = format!("{} (loaded)", envelope.meta.name);
    let id = alloc_room(&mut rooms, Some(name), DEFAULT_MAX_PLAYERS, sha.clone());

    // Stage the pending copy atomically, keyed by the new room's code. If staging
    // fails the room would come up as a random world — undo it and report instead.
    let pending = std::path::Path::new(&state.saves_dir).join(format!("pending-{id}.json"));
    if atomic_write(&pending, &bytes).is_err() {
        rooms.remove(&id);
        return (StatusCode::INTERNAL_SERVER_ERROR, "failed to stage save").into_response();
    }
    save_rooms(&state.rooms_path, &rooms);
    println!("matchmaker: staged save {} as room {id} on {sha}", req.file);

    (
        StatusCode::CREATED,
        Json(CreateResp {
            server: server_url(&state.game_origin, &sha),
            web_url: web_url(&state.game_origin, &sha, &id),
            share_url: share_url(&state.lobby_base, &id),
            id,
            sha,
        }),
    )
        .into_response()
}

#[tokio::main]
async fn main() {
    let home = std::env::var("HOME").unwrap_or_default();
    let registry_path = std::env::var("BS_REGISTRY")
        .unwrap_or_else(|_| format!("{home}/bs-mac/versions/registry.json"));
    let rooms_path = std::env::var("BS_ROOMS_STATE")
        .unwrap_or_else(|_| format!("{home}/bs-mac/versions/rooms.json"));
    // Must match the game servers' BS_SAVES_DIR — the save/load handoff is this
    // shared directory (the game server writes saves + consumes pending files;
    // the matchmaker lists saves + stages pending files).
    let saves_dir = std::env::var("BS_SAVES_DIR")
        .unwrap_or_else(|_| format!("{home}/bs-mac/versions/saves"));
    let game_origin: Arc<str> = std::env::var("BS_GAME_ORIGIN")
        .unwrap_or_else(|_| "game.badspaceship.com:7443".into())
        .into();
    let lobby_base: Arc<str> = std::env::var("BS_LOBBY_BASE")
        .unwrap_or_else(|_| "https://badspaceship.com".into())
        .into();

    // Restore rooms persisted from a previous run so live rooms survive restart.
    let rooms = Arc::new(Mutex::new(load_rooms(&rooms_path)));
    println!(
        "matchmaker: registry={registry_path} rooms={rooms_path} game_origin={game_origin} \
         (restored {} rooms)",
        rooms.lock().unwrap().len()
    );

    let state = AppState {
        rooms,
        registry_path,
        rooms_path,
        saves_dir,
        game_origin,
        lobby_base,
    };

    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/matches", get(list_matches).post(create_match))
        .route("/api/matches/{id}/join", post(join_match))
        .route("/api/matches/{id}/resolve", get(resolve_match))
        .route("/api/saves", get(list_saves))
        .route("/api/saves/load", post(load_save))
        .layer(CorsLayer::permissive())
        .with_state(state);

    // Optional same-origin static serving for local end-to-end testing.
    if let Ok(dir) = std::env::var("STATIC_DIR") {
        app = app.fallback_service(ServeDir::new(dir));
    }

    let bind = std::env::var("BIND").unwrap_or_else(|_| "0.0.0.0:5000".to_string());
    let addr: SocketAddr = bind.parse().expect("BIND must be host:port");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    println!("matchmaker listening on http://{addr}");
    axum::serve(listener, app).await.unwrap();
}
