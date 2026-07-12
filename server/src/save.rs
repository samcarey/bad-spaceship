//! Versioned save-game files: the on-disk format for a room's world, the
//! version-migration chain that keeps every old save loadable, and the (atomic)
//! file I/O. The ECS side — snapshotting a live room and respawning a saved one —
//! lives in `net.rs`; this module is pure data + disk.
//!
//! **Format contract.** A save is one JSON file:
//!
//! ```json
//! { "version": 1, "meta": { .. }, "world": { .. } }
//! ```
//!
//! * `version` + `meta` are the **stable envelope**: their fields may gain
//!   siblings but never change meaning or disappear, so *any* consumer that only
//!   lists saves (the matchmaker's `/api/saves`) can read every version without
//!   the migration chain.
//! * `world` is the versioned payload. When its schema changes, bump
//!   [`SAVE_VERSION`] and append a step to [`MIGRATIONS`]; [`parse_save`] then
//!   upgrades any older file step-by-step on load. The frozen [`tests::V1_FIXTURE`]
//!   pins the promise that a v1 file parses forever.
//!
//! The save schema is deliberately its **own** set of structs rather than the
//! wire-protocol types (`NetPart`/`NetJoint`/`PartShape`): the wire format may
//! change freely between builds (both ends always run the same commit), but a
//! save written last month must load today — the two must be free to diverge.
//!
//! **File layout** (all under [`saves_dir`], one flat directory shared by the
//! game server(s) and the matchmaker via `BS_SAVES_DIR`):
//! * `auto-<CODE>.json` — the rolling autosave, replaced in place every
//!   [`AUTOSAVE_SECS`] while the room is occupied.
//! * `manual-<CODE>-<slug>.json` — player-named manual saves; never touched by
//!   the autosave.
//! * `pending-<CODE>.json` — staged by the matchmaker's "load" endpoint: a copy
//!   of the chosen save keyed by the *new* room's code, consumed (and deleted)
//!   by the game server when it first creates that room.
//! * `recordings/<CODE>-<unix>.jsonl` — opt-in (`BS_RECORD`) **flight
//!   recordings** for machine analysis: one [`SaveWorld`] snapshot per simulated
//!   tick, plus every player's input for that tick, as JSON-lines (a versioned
//!   header line, then one line per tick). Where a save answers "put my world
//!   back", a recording answers "what *exactly* happened, frame by frame" — an
//!   agent can bisect it for the tick a bug appears, diff adjacent ticks, and see
//!   which input caused what. Same [`SaveWorld`] schema (and version) as saves,
//!   so one migration chain covers both. See `record_room_frames` in `net.rs`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Current save-file schema version. Bump on any `world` schema change, and append
/// the matching upgrade step to [`MIGRATIONS`].
pub const SAVE_VERSION: u32 = 2;

/// How often the server autosaves each occupied room (seconds).
pub const AUTOSAVE_SECS: f32 = 10.0;

/// One save file: the stable envelope (`version`, `meta`) around the versioned
/// world payload.
#[derive(Serialize, Deserialize, Debug)]
pub struct SaveFile {
    pub version: u32,
    pub meta: SaveMeta,
    pub world: SaveWorld,
}

impl SaveFile {
    /// A current-version save of `world`, stamped now. The single assembly point
    /// for both the autosave and manual-save writers, so the meta conventions
    /// can't drift between them.
    pub fn new(name: String, room_code: String, kind: &str, world: SaveWorld) -> Self {
        Self {
            version: SAVE_VERSION,
            meta: SaveMeta { name, room_code, kind: kind.to_string(), saved_unix: now_unix() },
            world,
        }
    }
}

/// The stable listing envelope — what the lobby's saved-games tab shows. Fields
/// here must keep their meaning across versions (see the module doc).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SaveMeta {
    /// Display name: the player-chosen name for a manual save; the room code for
    /// an autosave.
    pub name: String,
    /// The 6-char lobby code of the room this was saved from.
    pub room_code: String,
    /// `"auto"` or `"manual"`.
    pub kind: String,
    /// Unix seconds when the save was written.
    pub saved_unix: u64,
}

/// Everything that reconstructs a room's world. Derived state (assembly
/// membership, center of mass, hold springs) is deliberately absent — the
/// server's per-frame systems recompute it from parts + joints.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
pub struct SaveWorld {
    pub parts: Vec<SavePart>,
    pub joints: Vec<SaveJoint>,
    /// The players in the room at snapshot time. **Ignored on load** (players are
    /// live connections, and whoever loads the save is a different set) — carried
    /// for analysis: a snapshot without the players is half a crime scene.
    #[serde(default)]
    pub avatars: Vec<SaveAvatar>,
    /// Whether the room had already blasted off (its rockets fire from load).
    /// A save taken mid-countdown records `false` — the countdown is not resumed;
    /// players just swipe again.
    pub launched: bool,
    /// The room's floating-origin frame (v2+): all part/avatar poses above are
    /// **room-local**; true position = `frame.offset` + local. Zero for grounded
    /// rooms (and for every v1 save — the migration fills it in), so loading a
    /// mid-flight save resumes the flight with the same frame.
    #[serde(default)]
    pub frame: SaveFrame,
}

/// A room's floating-origin frame at snapshot time (the save-file mirror of the
/// wire `NetRoomFrame`, kept separate like `SaveShape` vs `PartShape`).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq)]
pub struct SaveFrame {
    /// Frame origin in true world coordinates (f64 — it grows without bound).
    pub offset: [f64; 3],
    /// Frame velocity in true world coordinates (the co-moving boost).
    pub velocity: [f32; 3],
}

/// One player's state at snapshot time (see [`SaveWorld::avatars`] — analysis
/// context, not restored on load).
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SaveAvatar {
    pub client_id: u64,
    pub name: String,
    pub position: [f32; 3],
    /// Look yaw (radians) — the avatar body itself is rotation-locked.
    pub yaw: f32,
    pub linear_velocity: [f32; 3],
    /// Index into [`SaveWorld::parts`] of the part this player is holding.
    pub held_part: Option<u32>,
}

/// One part: shape + appearance seed + full dynamic pose.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SavePart {
    pub shape: SaveShape,
    pub seed: u32,
    pub position: [f32; 3],
    pub rotation: [f32; 4],
    pub linear_velocity: [f32; 3],
    pub angular_velocity: [f32; 3],
}

/// A part's shape — the save-file mirror of the wire `PartShape` (kept separate
/// so the wire type can evolve without silently changing the save format).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum SaveShape {
    Cuboid { half_extents: [f32; 3] },
    RocketEngine,
}

/// A joint endpoint: an index into [`SaveWorld::parts`], or the ground. Indices
/// (not entity ids) because entity ids are meaningless across runs — the loader
/// respawns the parts and remaps.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub enum SaveBody {
    Part(u32),
    Ground,
}

/// One spherical joint: its two endpoints and their body-local anchors.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct SaveJoint {
    pub body1: SaveBody,
    pub body2: SaveBody,
    pub anchor1: [f32; 3],
    pub anchor2: [f32; 3],
}

// ---- Version migration -------------------------------------------------------

/// One upgrade step over the raw JSON. `MIGRATIONS[i]` rewrites a version `i+1`
/// document into a version `i+2` document. The array length is tied to
/// [`SAVE_VERSION`] in the type, so bumping the version without appending its
/// migration step is a **compile error**, not a latent load failure. Steps
/// operate on `serde_json::Value` because the old schema's Rust types no longer
/// exist by the time a migration is written.
type Migration = fn(serde_json::Value) -> Result<serde_json::Value, String>;

const MIGRATIONS: [Migration; (SAVE_VERSION - 1) as usize] = [migrate_v1_frame];

/// v1 → v2: worlds gained a floating-origin `frame` (see [`SaveFrame`]). Every v1
/// save was written in true world coordinates, i.e. a zero frame.
fn migrate_v1_frame(mut value: serde_json::Value) -> Result<serde_json::Value, String> {
    let world = value
        .get_mut("world")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or("save has no world object")?;
    world.insert(
        "frame".into(),
        serde_json::json!({ "offset": [0.0, 0.0, 0.0], "velocity": [0.0, 0.0, 0.0] }),
    );
    Ok(value)
}

/// Parse a save file of **any** supported version, upgrading it step-by-step to
/// the current schema. This is the only entry point for reading a save — never
/// deserialize [`SaveFile`] directly, or old files stop loading.
pub fn parse_save(json: &str) -> Result<SaveFile, String> {
    let mut value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("malformed save: {e}"))?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .ok_or("save has no version field")? as u32;
    if version == 0 || version > SAVE_VERSION {
        return Err(format!(
            "save version {version} unsupported (this build reads 1..={SAVE_VERSION})"
        ));
    }
    for from in version..SAVE_VERSION {
        value = MIGRATIONS[(from - 1) as usize](value)
            .map_err(|e| format!("migrating save v{from} -> v{}: {e}", from + 1))?;
    }
    let mut save: SaveFile = serde_json::from_value(value)
        .map_err(|e| format!("save (v{version}, migrated to v{SAVE_VERSION}) invalid: {e}"))?;
    save.version = SAVE_VERSION;
    Ok(save)
}

// ---- Files -------------------------------------------------------------------

/// The saves directory: `BS_SAVES_DIR`, or `saves/` under the process cwd. The
/// deploy sets the env var to one shared absolute path so every game-server
/// version and the matchmaker see the same saves.
pub fn saves_dir() -> PathBuf {
    std::env::var("BS_SAVES_DIR").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("saves"))
}

pub fn auto_file_name(code: &str) -> String {
    format!("auto-{code}.json")
}

pub fn manual_file_name(code: &str, name: &str) -> String {
    format!("manual-{code}-{}.json", file_slug(name))
}

/// A save name reduced to filename-safe characters (lowercase alphanumerics,
/// runs of anything else collapsed to one dash). Same-slug manual saves in the
/// same room overwrite each other — "save again under this name" semantics.
fn file_slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() { "save".to_string() } else { slug }
}

/// The room code as a display/file string. All-zero (a native client with no
/// `BS_ROOM`) maps to `DEFAULT` — 7 chars, so it can never collide with a real
/// 6-char matchmaker code.
pub fn code_string(code: [u8; 6]) -> String {
    let s: String = code.iter().take_while(|&&b| b != 0).map(|&b| b as char).collect();
    if s.is_empty() { "DEFAULT".to_string() } else { s }
}

/// Atomically write a save (temp file + rename in the same directory), creating
/// the saves dir on first use — a crash or a concurrent reader never sees a
/// half-written file, and the previous version survives until the rename.
pub fn write_save(file_name: &str, save: &SaveFile) -> Result<(), String> {
    let dir = saves_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {dir:?}: {e}"))?;
    let json = serde_json::to_string_pretty(save).map_err(|e| e.to_string())?;
    let path = dir.join(file_name);
    let tmp = dir.join(format!("{file_name}.tmp"));
    std::fs::write(&tmp, json).map_err(|e| format!("writing {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("renaming into {path:?}: {e}"))
}

/// Consume the pending "load this save into this room" file the matchmaker staged
/// for a freshly-created room code: parse (+migrate) it and delete it, so the
/// load happens exactly once. `None` (with a log line on parse failure) means
/// "no pending load — spawn the normal random world".
pub fn take_pending(code: &str) -> Option<SaveWorld> {
    let path = saves_dir().join(format!("pending-{code}.json"));
    let json = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    match parse_save(&json) {
        Ok(save) => {
            println!(
                "[save] room {code}: loading pending save '{}' (from room {}, {} parts)",
                save.meta.name,
                save.meta.room_code,
                save.world.parts.len()
            );
            Some(save.world)
        }
        Err(e) => {
            println!("[save] room {code}: pending save unusable, spawning fresh world: {e}");
            None
        }
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Open a room's flight-recording file (`recordings/<CODE>-<unix>.jsonl` under the
/// saves dir) and write its versioned header line. The header carries the same
/// `version` as saves — every line's `world` is a [`SaveWorld`], so the one
/// migration chain covers recordings too.
pub fn open_recording(code: &str) -> Result<std::fs::File, String> {
    use std::io::Write;
    let dir = saves_dir().join("recordings");
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating {dir:?}: {e}"))?;
    let path = dir.join(format!("{code}-{}.jsonl", now_unix()));
    let mut file = std::fs::File::create(&path).map_err(|e| format!("creating {path:?}: {e}"))?;
    let header = serde_json::json!({
        "recording": true,
        "version": SAVE_VERSION,
        "room_code": code,
        "started_unix": now_unix(),
        // The exact simulated-tick timebase (from the shared TICK the server loop
        // runs on) — analysis tooling times frames off this, so it must never be
        // a hand-written approximation.
        "tick_secs": bad_spaceship_shared::net::TICK.as_secs_f64(),
    });
    writeln!(file, "{header}").map_err(|e| format!("writing header to {path:?}: {e}"))?;
    println!("[save] recording room {code} -> {path:?}");
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;


    /// A **frozen** v1 save file, byte-for-byte as v1 wrote it. This fixture must
    /// NEVER be edited to track schema changes — that's the point: when the schema
    /// moves, bump `SAVE_VERSION`, add the migration step, and this test proves the
    /// old file still loads through the chain. If this test fails to compile-parse,
    /// an old player save would fail the same way.
    const V1_FIXTURE: &str = r#"{
      "version": 1,
      "meta": { "name": "test stack", "room_code": "ABCDEF", "kind": "manual", "saved_unix": 1751500000 },
      "world": {
        "parts": [
          {
            "shape": { "Cuboid": { "half_extents": [0.5, 0.75, 0.5] } },
            "seed": 42,
            "position": [1.0, 0.75, -2.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "linear_velocity": [0.0, 0.0, 0.0],
            "angular_velocity": [0.0, 0.0, 0.0]
          },
          {
            "shape": "RocketEngine",
            "seed": 0,
            "position": [1.0, 2.25, -2.0],
            "rotation": [0.0, 0.0, 0.0, 1.0],
            "linear_velocity": [0.0, 0.1, 0.0],
            "angular_velocity": [0.0, 0.0, 0.1]
          }
        ],
        "joints": [
          { "body1": { "Part": 0 }, "body2": { "Part": 1 }, "anchor1": [0.0, 0.75, 0.0], "anchor2": [0.0, -0.75, 0.0] },
          { "body1": { "Part": 0 }, "body2": "Ground", "anchor1": [0.0, -0.75, 0.0], "anchor2": [1.0, 0.0, -2.0] }
        ],
        "avatars": [
          {
            "client_id": 12345,
            "name": "Player 1",
            "position": [0.0, 0.6, 0.0],
            "yaw": 1.57,
            "linear_velocity": [0.0, 0.0, 0.0],
            "held_part": 1
          }
        ],
        "launched": false
      }
    }"#;

    /// The backwards-compatibility promise: a v1 file always loads.
    #[test]
    fn v1_fixture_always_loads() {
        let save = parse_save(V1_FIXTURE).expect("frozen v1 save must always parse");
        assert_eq!(save.version, SAVE_VERSION);
        assert_eq!(save.meta.room_code, "ABCDEF");
        assert_eq!(save.meta.kind, "manual");
        assert_eq!(save.world.parts.len(), 2);
        assert_eq!(save.world.parts[1].shape, SaveShape::RocketEngine);
        assert_eq!(save.world.joints.len(), 2);
        assert_eq!(save.world.joints[1].body2, SaveBody::Ground);
        assert_eq!(save.world.avatars.len(), 1);
        assert_eq!(save.world.avatars[0].held_part, Some(1));
        assert!(!save.world.launched);
        // v1 predates the floating-origin frame; the migration fills in zero
        // (v1 worlds were written in true world coordinates).
        assert_eq!(save.world.frame, SaveFrame::default());
    }

    /// What we write today parses back through the same (migrating) entry point.
    #[test]
    fn current_format_round_trips() {
        let save = SaveFile {
            version: SAVE_VERSION,
            meta: SaveMeta {
                name: "roundtrip".into(),
                room_code: "QQQQQQ".into(),
                kind: "auto".into(),
                saved_unix: 1,
            },
            world: SaveWorld {
                parts: vec![SavePart {
                    shape: SaveShape::Cuboid { half_extents: [1.0, 2.0, 3.0] },
                    seed: 7,
                    position: [0.0, 1.0, 0.0],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    linear_velocity: [0.0; 3],
                    angular_velocity: [0.0; 3],
                }],
                joints: vec![],
                avatars: vec![],
                launched: true,
                // A mid-flight save: the frame must survive the round trip exactly.
                frame: SaveFrame { offset: [12.5, 123456.789, -3.25], velocity: [0.5, 812.0, -0.25] },
            },
        };
        let json = serde_json::to_string(&save).unwrap();
        let back = parse_save(&json).unwrap();
        assert_eq!(back.world, save.world);
    }

    /// Unknown / future versions are refused (not misread as the current schema).
    #[test]
    fn future_version_is_refused() {
        let json = format!(r#"{{ "version": {}, "meta": {{}}, "world": {{}} }}"#, SAVE_VERSION + 1);
        assert!(parse_save(&json).is_err());
    }

    #[test]
    fn slugs_and_codes() {
        assert_eq!(file_slug("My Cool Save!!"), "my-cool-save");
        assert_eq!(file_slug("   "), "save");
        assert_eq!(code_string(*b"ABCD\0\0"), "ABCD");
        assert_eq!(code_string([0; 6]), "DEFAULT");
    }
}
