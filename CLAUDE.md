# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Bad Spaceship is a 3D game built on the **Bevy 0.18** engine (ECS), with
**Avian** (`avian3d`, an XPBD physics engine) for physics and `bevy_egui` for UI.
It is a Cargo workspace with three crates that compiles both to a **native**
binary and to a **WASM** web build playable in the browser. (Physics was migrated
off `bevy_rapier3d` — see the "Migrating bevy_rapier3d → Avian" section below.)

> **Engine version note:** the repo briefly ran on Bevy **0.19** but was stepped
> back to **0.18** so the multiplayer netcode stack (`lightyear`) builds — see
> "Temporarily on Bevy 0.18 for multiplayer" below. **Engine versions are now
> pinned in one place:** `[workspace.dependencies]` in the root `Cargo.toml`.

## Toolchain & reproducibility (read first)

This is a pinned-for-reproducibility project — do not "upgrade" your way out of
build errors (the deliberate Bevy bumps are the exception, done branch-by-branch):

- **Rust is pinned to 1.96.0** via `rust-toolchain.toml` (auto-selected by rustup).
  Bevy 0.17 / wgpu 25 raise the MSRV past the old 1.85 floor (wgpu 25's MSRV is 1.87),
  so the pin moved to a recent stable. Because 1.96 is well ahead of every dep's
  `rust-version`, the whole *class* of MSRV-back-pins the 0.16 lockfile needed (`image`
  0.25.9, `wayland-protocols` 0.32.12, `built` 0.8.0, `wasip2` 1.0.1) **dissolved** — a
  fresh `cargo generate-lockfile` resolves the graph forward freely with no "requires
  Rust 1.XX" notes. Do not `cargo update` the whole graph blindly on a non-upgrade
  branch, but on a Bevy bump regenerating the lock from scratch is the right move.
- **`Cargo.lock` is committed.** Always build with `--locked`; when a deliberate re-pin
  is needed, bump direct deps with targeted `cargo update -p <crate> --precise <ver>`
  rather than a blanket update.
- **getrandom on wasm needs explicit backends.** Bevy 0.17 moved to `getrandom` 0.3,
  which no longer auto-selects a wasm backend: it needs `--cfg getrandom_backend="wasm_js"`
  (set for the wasm target in the committed **`.cargo/config.toml`**) plus the matching
  `wasm_js` feature (client `web` feature). Separately, `rand` 0.8 (client + shared) pulls
  `getrandom` *0.2* transitively, which on wasm needs *its* `js` feature — enabled via a
  renamed `getrandom_02 = { package = "getrandom", version = "0.2", features = ["js"] }`
  optional dep wired into the `web` feature (feature unification covers the transitive
  copy). `lightyear` (multiplayer) then pulls a *third* line, `getrandom` **0.4**, which
  like 0.3 needs the `getrandom_backend="wasm_js"` cfg (already global) *plus* its `wasm_js`
  feature — handled by the same trick (`getrandom_04 = { package = "getrandom", version =
  "0.4", features = ["wasm_js"] }`, in the `web` feature). Symptom if any is missing: `the
  wasm*-unknown-unknown targets are not supported by default` at `getrandom` compile.
- The web build targets the **WebGL2 backend** via the `bevy/webgl2` feature (in the
  client's `web` feature): Bevy 0.17's wgpu 26 otherwise compiles the WebGPU backend
  on wasm, which needs `--cfg=web_sys_unstable_apis` and a WebGPU-capable browser. WebGL2
  is the broad-support renderer the build has used on Pages all along.
- The web build needs a **version-matched `wasm-bindgen` CLI (exactly 0.2.125)**, matching
  the `wasm-bindgen` crate the graph resolves (bevy 0.17 / wgpu 26's `web-sys` 0.3.102
  still resolves to 0.2.125 — unchanged from 0.16; the client pins `wasm-bindgen =
  "=0.2.125"` to keep it stable). The CLI version must match the crate exactly or
  `wasm-bindgen` refuses the module (schema mismatch). Use the prebuilt binary from the
  rustwasm GitHub release, not `cargo install` (building the CLI from source hits the same
  dependency bitrot). The Pages CI also hardcodes `RUST_TOOLCHAIN` and
  `WASM_BINDGEN_VERSION` in `.github/workflows/pages.yml` (it `rustup override set`s the
  toolchain, shadowing `rust-toolchain.toml`) — bump **both** in lockstep on a Bevy upgrade.
  (Note: on the Apple-silicon Mac build box the x86_64 `wasm-bindgen` binary aborts under
  emulation with a bogus huge allocation; that is an emulation artifact — the GitHub
  x86_64 runners run it natively and fine. Verify wasm *compilation* on the Mac and leave
  the `wasm-bindgen` step to CI.)

## Build & run

This is a workspace, so **build artifacts go to the repo-root `target/`**, not
`client/target/` — important when locating the compiled `.wasm`.

```bash
# Native game (the primary dev loop):
cd client && cargo run --features native --release      # or drop --release for debug

# Headless server (runs the shared simulation at 60 Hz):
cd server && cargo run                                   # add --release as needed

# Web build (two steps; mirrors the GitHub Pages CI):
cargo build --locked --manifest-path client/Cargo.toml \
  --target wasm32-unknown-unknown --features web --release
wasm-bindgen --out-dir <out> --out-name wasm --target web --no-typescript \
  target/wasm32-unknown-unknown/release/bad-spaceship-client.wasm
# Serve index.html + assets/ + the generated target/{wasm.js,wasm_bg.wasm}.
```

`client` requires **exactly one** of the `native` / `web` features (they pull in
mutually exclusive deps). `.vscode/tasks.json` has equivalents for all of the above.
There is no test suite configured.

## Remote build & test box (Mac mini "mini4")

The Claude Code **web** sandbox can't run the heavy parts of this project — full
`--release` native/WASM builds, `wasm-bindgen`, long compiles, or anything wanting a
real machine. Offload those to the **Mac mini dev box** (`samcarey@mini4`, Apple M4,
32 GB, public IP `65.28.10.210`). Agents reach it over HTTP — **no SSH needed for
build/test work**.

### How an agent runs commands on it

A small "cmd-api" service exposes a shell inside a **Linux `aarch64` Docker container**
on the Mac (Colima VM) — *not* macOS directly. Commands run as `root`, home `/root`.

- Endpoint: `https://cmd-api.dev.whoeverwants.com` (also exposed as `$MAC_API_URL` in
  some environments; fall back to the literal URL when that var is unset).
- Auth: bearer token in the **`MAC_API_TOKEN`** env var (present in web sessions).
  **Never** hardcode the token in code, commits, or logs.
- `POST /exec` with JSON `{"cmd": "<shell>"}`; response is `{"exit_code", "stdout",
  "stderr"}`. (`/run`, `/command`, and `/` behave the same; `/exec` is canonical. The
  body key must be `cmd` — `command` is ignored.)

Paste-able helper:

```bash
mac() {  # usage: mac 'shell command; another'
  curl -sS -m 600 -X POST "${MAC_API_URL:-https://cmd-api.dev.whoeverwants.com}/exec" \
    -H "Authorization: Bearer $MAC_API_TOKEN" -H "Content-Type: application/json" \
    --data "$(python3 -c 'import json,sys; print(json.dumps({"cmd": sys.argv[1]}))' "$1")"
}
mac 'hostname; nproc; df -h /'
```

### What's on the box (verified 2026-06-22)

- 6 CPUs, ~23 GB RAM, ~45 GB free disk; outbound internet works (can fetch crates/toolchains).
- Preinstalled: `git`, `docker`, `python3`. **Not** present: `rustc`/`cargo`/`rustup`,
  `node`, Homebrew. Install the pinned **Rust 1.85.0** toolchain (see *Toolchain &
  reproducibility*) before building. A stale `wasm-bindgen` CLI from a prior session may
  also linger in `/usr/local/bin` — force-reinstall the version-matched one (the schema
  check rejects a mismatched module). Note: `rustup override set` from a prior session
  can shadow `rust-toolchain.toml` for `/root/bs` — `rustup override unset` in the repo
  if `rustc --version` doesn't match the pin.
- Treat the container filesystem as **disposable** — clone fresh and don't rely on
  long-term state surviving a host/VM restart.

### When the agent needs the user (host-level changes)

The cmd-api only reaches *inside* the Linux container. Anything on the **macOS host**
itself — Colima CPU/RAM sizing, `~/devbox/` config, LaunchAgents, the cmd-api service,
rebuilding the container image, or debugging when the endpoint is down — needs the user,
who has SSH/physical access to `mini4` and can install/configure tooling on request.

**Rule for asking the user:** request **one command at a time** (a single line; chaining
with `;` / `&&` is fine). Wait for the result before sending the next. Do **not** dump a
long multi-step checklist at once.

## Architecture

The key idea is a **shared simulation** crate that both the client and the headless
server run, so game logic stays identical across renderers and platforms.

- **`shared/`** (`bad-spaceship-shared`, lib) — all platform-agnostic game logic,
  exposed as the `CommonPlugins` plugin group (`shared/src/lib.rs`): third-party
  Avian `PhysicsPlugins` (added via `add_group`, since it's a `PluginGroup`),
  plus the custom `Character`, `Config`,
  `Map`, `Part`, and `Player` plugins. (The former `bevy_easings` dependency was
  dropped — see "Dropping bevy_easings" below — so the one camera tween it powered
  is now hand-rolled in `player.rs`.) Game tuning lives in RON files under
  `client/assets/config/` (`character.character.ron`, `player.player.ron`), deserialized
  into per-domain `character::Config` / `player::Config` types. Because Bevy 0.12 resolves
  asset loaders by **file extension only**, `ConfigPlugin` (`shared/src/config.rs`)
  registers a generic `RonConfigLoader<T>` once per type under a type-specific extension
  (`character.ron` / `player.ron`) — hence the doubled-up filenames. Both binaries keep the
  asset `Handle`s alive in a `load_configs` startup system (dropping a handle unloads the
  asset) and enable hot-reload via `AssetPlugin { watch_for_changes_override: Some(true), .. }`
  (Bevy 0.12 replaced the 0.11 `ChangeWatcher` with this simple override flag; the watcher
  only runs when the `file_watcher` feature is on, i.e. native).

  Note: asset paths in the `load_configs` systems are written with **Windows-style
  backslashes** (e.g. `"config\\character.ron"`); preserve that style when editing
  those calls rather than "fixing" them to forward slashes.

- **`client/`** (`bad-spaceship-client`, bin) — the playable game (`#[bevy_main]` in
  `client/src/main.rs`). Adds `DefaultPlugins` + `CommonPlugins` and the
  rendering/UI/input layers: `UiPlugin`, `InputPlugin`, `HighlightPlugin`,
  `RenderMainPassPlugin`, and `RenderSecondaryPassPlugin` (a second camera pass for
  gizmo/cone overlays). A Bevy `AppState` state machine (`Initial` → `InGame` ↔
  `InGameMenu`, in `client/src/main.rs`) gates input handling and cursor/pointer-lock.

- **`server/`** (`bad-spaceship-server`, bin) — headless host: `MinimalPlugins` +
  `AssetPlugin` + `CommonPlugins`, no rendering, fixed 60 Hz loop. Loads assets from
  `../client/assets`.

### Platform abstraction

`client/src/platform/mod.rs` `#[cfg]`-switches between `native.rs` and `web.rs`,
both exposing a `PlatformPlugin`. **Both platforms now read the same winit-native
input** — `ButtonInput<KeyCode>` / `ButtonInput<MouseButton>` / `MouseWheel` /
`MouseMotion` — so `input.rs` has a single code path (see "Unifying web input on
winit-native" below). Most platform-specific code is gated on
`#[cfg(target_arch = "wasm32")]`. The WASM canvas is sized to the viewport via CSS
(`canvas { width/height: 100% }` in `index.html`): Bevy 0.13 removed
`Window::fit_canvas_to_parent` (which had itself replaced the old
`bevy_web_fullscreen` plugin), and the recommended replacement is plain CSS.

`web.rs` is now just the genuinely browser-specific glue: a `pointerlockchange`
DOM listener (the browser owns the Esc-to-exit-lock gesture, so menu toggling keys
off lock state rather than a keypress), a `request_pointer_lock()` call on entering
the game, and the `signal_game_ready` loader-overlay handshake (tags `<body
data-game-ready>` once the ground mesh has drawn). Mouse-look relies on winit
emitting `MouseMotion` under pointer lock — see the unification section for the
history (this used to be fought as a *bug*).

### Build metadata

`client/build.rs` uses `shadow-rs` and `git rev-parse HEAD` to inject a
`SHORT_GIT_HASH` shown in-game, so the build needs git history available.

### Bevy 0.13 rendering/UI gotchas

Two more 0.13-bump surprises that don't show up at build time, only as wrong
visuals at runtime:

- **`bevy_egui` needs its `render` feature.** We pull `bevy_egui` with
  `default-features = false` (to drop native clipboard/`open` deps that don't
  belong on web); under bevy_egui 0.28 that also drops `render`, which silently
  disables *all* egui drawing — the pause menu and the instructions/FPS overlays
  vanish while egui still runs. Re-add `"render"` to the feature list (kept
  separate from the clipboard/hyperlink features) so the UI paints.
- **Ambient light is now in lux.** Bevy 0.13's lighting/exposure overhaul made
  `AmbientLight::brightness` a physical lux value (the 0.12 default of `0.05`
  no longer means the same thing). Left at the new default, shadowed faces read
  almost black under the bright directional sun. We set `brightness` to ~`600`
  to restore the soft fill the 0.12 build had; tune this single value if the
  dark sides look too flat or too dark. (Bevy 0.14 left these light units
  unchanged, so the `600` carried over as-is.)

### Bevy 0.14 migration gotchas

The 0.13 → 0.14 bump (third-party deps: `bevy_rapier3d` 0.25 → 0.27, `bevy_egui`
0.25 → 0.28, `bevy_easings` 0.13 → 0.14). Most changes were compile-time and
mechanical, but a few are easy to get wrong:

- **`bevy_state` is a Cargo feature now.** 0.14 split the state machine into its own
  crate gated behind `bevy/bevy_state`. The client uses `default-features = false`,
  so `init_state`/`NextState`/`OnEnter` vanish until that feature is re-added (it's
  in the client's `default` feature list).
- **`multi-threaded` → `multi_threaded`.** Bevy renamed the hyphenated feature to an
  underscore; the `file_watcher` hot-reload path still needs it (client + server).
- **Color is `srgb`, and no longer a uniform type.** `Color::rgb/rgba` →
  `Color::srgb/srgba`, and per-channel setters (`set_r/g/b`) are gone (rebuild the
  highlight colours outright). `Color` also dropped `ShaderType`, so `GizmoMaterial`'s
  `#[uniform(0)]` stores `LinearRgba` (via `Color::to_linear()`) — the same
  representation the 0.13 `Color` uniform already serialized, so colours are unchanged.
- **WGSL matrix helpers renamed.** `get_model_matrix` → `get_world_from_local` (part of
  0.14's `<dest>_from_<src>` naming). This is a *runtime* shader-compile failure, not a
  Rust error — see `client/assets/gizmo_material.wgsl`.
- **`AssetLoader::load` is a native `async fn`** (no more hand-rolled `BoxedFuture`).
  The trait ties every `&'a` argument to one lifetime while leaving the
  `Reader`/`LoadContext` *inner* lifetimes elided (`Reader<'_>`, `LoadContext<'_>`);
  matching that exactly is the only fiddly part (see `shared/src/config.rs`).
- **rapier 0.27 joints + contacts.** `ImpulseJoint::data` is now a `TypedJoint` enum;
  reach the underlying `GenericJoint` (and its `.raw` rapier frame) via
  `AsRef<GenericJoint>` (`joint.data.as_ref().raw…`). `ContactPairView::
  has_any_active_contacts` lost its trailing `s` → `has_any_active_contact`.

### Bevy 0.15 migration gotchas

The 0.14 → 0.15 bump (third-party deps: `bevy_rapier3d` 0.27 → 0.28, `bevy_egui`
0.28 → 0.31, `bevy_easings` 0.14 → 0.15). The headline change is **required
components** replacing prefab bundles, which touched almost every spawn:

- **Bundles → required components, and `Handle<T>` is no longer a `Component`.**
  `Camera3dBundle` → `Camera3d::default()`, `DirectionalLightBundle` →
  `DirectionalLight`, `PbrBundle`/`MaterialMeshBundle` → the `Mesh3d(handle)` +
  `MeshMaterial3d(handle)` wrappers, `TransformBundle::from(t)` → bare `t` (since
  `Transform` now *requires* `GlobalTransform`). Marker components can ride along in
  the spawn tuple. The wrappers `Deref` to the inner `Handle`, but `Assets::get_mut`
  wants an id — use `mesh_material.id()`. Querying a material is now
  `&MeshMaterial3d<StandardMaterial>`, not `&Handle<StandardMaterial>`
  (`client/src/highlight.rs`). `CascadeShadowConfigBuilder` is a standalone component
  via `.build()`. User-defined `#[derive(Bundle)]` structs still work and stay the
  right tool for grouping the game's *own* components — but drop now-redundant
  `GlobalTransform`/`InheritedVisibility`/`ViewVisibility` fields that the required
  components of `Transform`/`Visibility` now supply (see `TransformGizmoBundle`).
- **Gamepads are entities.** No more `Res<ButtonInput<GamepadButton>>` /
  `Res<Axis<GamepadAxis>>` / `GamepadConnectionEvent` lobby bookkeeping. Query
  `Query<&Gamepad>` and read state off the component (`gamepad.get(GamepadAxis::…)`,
  `gamepad.just_pressed(GamepadButton::…)`); the enums lost their `*Type` suffix
  (`GamepadButtonType::South` → `GamepadButton::South`). This let the whole
  `GamepadLobby` resource + connection system be deleted (`client/src/input.rs`).
- **rapier 0.28 made `RapierContext`/`RapierConfiguration` components.** They were
  resources. `Res<RapierContext>` → the `ReadDefaultRapierContext` system param
  (derefs to `RapierContext`, so method calls are unchanged); `RapierConfiguration`
  (for `.gravity`) is now read via `Query<&RapierConfiguration>` + `.single()`. (The
  *further* split of `RapierContext` into `RapierContextColliders`/`…Joints`/etc. is
  0.29 — not yet relevant on 0.28.)
- **bevy_egui 0.30+ made `EguiSettings` a component**, not a resource — query
  `Query<&mut EguiSettings>` (one per egui context) instead of `ResMut<EguiSettings>`.
- **`Window.cursor` → `Window.cursor_options`** (grab mode / visibility live in the
  `cursor_options` sub-struct now; `client/src/platform/native.rs`).
- **`AsyncReadExt`/`Reader` simplification.** `AssetLoader::load` now takes
  `reader: &mut dyn Reader` with fully elided lifetimes (drop the `<'a>`), and
  `Reader` provides `read_to_end` itself — the `AsyncReadExt` import becomes unused
  (`shared/src/config.rs`).
- **Smaller renames:** `Time::delta_seconds()` → `delta_secs()`;
  `EntityCommands::push_children()` → `add_children()`; `EasingsPlugin` is now a
  `Default` struct (`EasingsPlugin::default()`); `bevy_easings::custom_ease_system`
  gained a leading `Time<T>` type param (`custom_ease_system::<(), MyComponent>`).
- **`Reflect` is in the prelude and impl'd for atomics.** Bevy 0.15's
  `PartialReflect::set` plus `Reflect` impls for `AtomicBool`/`AtomicI32` collide with
  any custom `.set()` extension method on those types under `use bevy::prelude::*`.
  The fix was renaming the project's atomic-ext `set` → `store_val`
  (`client/src/platform/web.rs`). If a bare method call on a std type suddenly
  "expects `Box<dyn Reflect>`", this is why — rename, don't de-glob the prelude.

### Bevy 0.16 migration gotchas

The 0.15 → 0.16 bump (third-party deps: `bevy_rapier3d` 0.28 → 0.30, `bevy_egui`
0.31 → 0.34, `bevy_easings` 0.15 → 0.16). The headline change is the **relationships
system** (`Parent`/`Children` became a generic relationship) plus a sweep of APIs
becoming fallible. Mostly compile-time and mechanical, but several are subtle:

- **`Parent` → `ChildOf`, and `Children::iter()` yields `Entity` by value.** The
  parent accessor is `child_of.parent()` (was `parent.get()`). `Children::iter()` is now
  a `RelationshipTarget` method returning owned `Entity`, *not* `&Entity` — so drop the
  `*` derefs at every iteration site (`get(*child)` → `get(child)`) and drop `.cloned()`
  when collecting (`children.iter().collect()`). `Children` still `Deref`s to `[Entity]`,
  but the trait `iter()` wins method resolution. (`shared/src/{part,player}.rs`,
  `client/src/render_secondary_pass/mod.rs`.)
- **`despawn()` is now recursive; `despawn_recursive()` is gone.** A bare `despawn()`
  clears the whole subtree. This bit the player-death path: the app-lifetime camera is
  parented under the player's orbit hierarchy, so recursively despawning the player would
  take the camera down with it (→ "No camera present in scene" panic). Fix: clear the
  camera's `ChildOf` first (`commands.entity(camera).remove::<ChildOf>()`), *then*
  `despawn()` the player — the camera survives and is re-parented to the next player by
  `add_camera_to_player` (`shared/src/player.rs`). To keep *other* children, the sanctioned
  pattern is `entity.remove::<Children>().despawn()`.
- **`Query::single`/`single_mut` return `Result` now.** Wrap with `let Ok(x) = q.single()
  else { return; };` (or `?` in a fallible system). Hits the window cursor systems
  (`native.rs`) and the rapier config/context reads.
- **`EventWriter::send` → `write`, but `Events::send` (the resource method) stays `send`.**
  Only the `EventWriter` system-param method was renamed (`send`/`send_batch`/`send_default`
  → `write`/…). The web platform writes directly into `ResMut<Events<MouseMotion>>` — that
  one keeps `.send()` (`client/src/platform/web.rs`).
- **Every light gained a mixed-lighting field.** `AmbientLight` got
  `affects_lightmapped_meshes`; `DirectionalLight`/`Point`/`Spot` got
  `affects_lightmapped_mesh_diffuse` (all default `true`). We use no lightmaps, so
  `..default()` covers it. The `brightness: 600` lux value is unchanged. (Default
  directional-shadow cascade settings also changed — verify shadows visually.)
- **`bevy::log` is gated behind the `bevy_log` feature.** With `default-features = false`
  it vanishes (the `bevy::log::info!` calls in the pause menu fail to resolve); re-add
  `"bevy/bevy_log"` to the client's `default` feature list. (It was implicitly present
  under 0.15.)
- **`Handle::weak_from_u128` is deprecated → `weak_handle!("<uuid>")`.** Imported as
  `bevy::asset::weak_handle` (same path style as the project's existing
  `bevy::asset::load_internal_asset`). The UUID `00000000-0000-0000-c1a5-db6ae813446b`
  is the old `13953800272683943019` seed expressed as a UUID, so the registered shader
  handle is byte-for-byte unchanged (`client/src/render_secondary_pass/gizmo_material.rs`).
- **bevy_egui 0.34: `EguiPlugin` carries a required flag, `EguiSettings` was renamed.**
  `EguiPlugin` is now `EguiPlugin { enable_multipass_for_primary_context: false }` — keep
  `false` since the UI is plain immediate-mode egui drawn from `Update` (multipass is
  opt-in for advanced egui features we don't use). `EguiSettings` → `EguiContextSettings`
  (still a per-context component with `scale_factor`). `EguiContexts::ctx_mut()` is
  unchanged (`client/src/ui.rs`).
- **`FrameTimeDiagnosticsPlugin` is a struct with fields now** — add it as
  `FrameTimeDiagnosticsPlugin::default()` (the `FPS`/`FRAME_TIME` `DiagnosticPath`
  consts are unchanged).
- **rapier 0.30 split `RapierContext` into component pieces.** `ReadDefaultRapierContext`
  is gone; use the `ReadRapierContext` system param and call `.single()` (now fallible) to
  get a bundled `RapierContext<'_>` view that still exposes `contact_pairs_with(...)`.
  `ContactPairView::collider1/2` now return **`Option<Entity>`** (a collider may lack a
  backing entity) — unwrap/skip with `filter_map` + `?` or a `let (Some(c1), Some(c2)) =
  … else { continue; }`. `Collider::trimesh(...)` is fallible too (returns
  `Result<_, TriMeshBuilderError>`). `RapierConfiguration` is still a component
  (`Query<&RapierConfiguration>`, `.gravity` intact); the contact-manifold/`ContactView`
  methods (`has_any_active_contact`, `find_deepest_contact`, `manifolds`, `points`,
  `local_p1/2`, `dist`) are unchanged. (`shared/src/{character,part,map}.rs`.)
- **rapier 0.30 needs CCD on the falling blocks (runtime, not compile).** The parts
  spawn high (`y 5..15`) and hit the *thin trimesh* bowl ground fast. rapier 0.30's
  discrete solver lets a fast impact penetrate deeply in one step, and its newer
  soft-contact recovery leaves the block partially embedded — the Bevy 0.12-era rapier
  pushed them back out, so this reads as a regression ("blocks stuck in the ground on
  spawn"). Fix: `.insert(Ccd::enabled())` on the dynamic parts (`shared/src/part.rs`).
  Quantified on the headless server (drop 10 blocks, settle 600 ticks, measure deepest
  `contact.dist()` against the ground): worst-case penetration ~`0.07-0.08` without CCD
  (intermittent, ~1-2 of 10 blocks) → ~`0.002` (normal contact margin, 0 stuck) with it.

### Bevy 0.17 migration gotchas

The 0.16 → 0.17 bump (third-party deps: `bevy_rapier3d` 0.30 → **0.32** — *not* 0.31,
which is still a Bevy-0.16 release and silently drags a *second* whole Bevy into the
graph; 0.32 is the 0.17 release; `bevy_egui` 0.34 → 0.38, `bevy_easings` 0.16 → 0.17).
Match third-party crates by their *declared* bevy dep (`crates.io/api/v1/crates/<c>/<v>/
dependencies`), not a blog/readme — bevy_rapier's README tracks unreleased master. The two
headline changes are the **event→message rename** and the **render-crate split**:

- **Buffered "events" are now "messages."** The `Event` trait/`EventReader`/`EventWriter`/
  `Events<E>` (the *buffered* queue API) became `Message`/`MessageReader`/`MessageWriter`/
  `Messages<M>`; `App::add_event` → `add_message`; the `Events` resource's `.send()` →
  `.write()` (the `EventWriter` method was already `.write()` since 0.16, and `.read()`/
  `.clear()` are unchanged). `Event` now means *observer* events only. Every custom buffered
  type (`PlayerClick`, `NewPart`, `AttachEvent`/`ReleaseEvent`/`HoldEvent`) derives `Message`
  now, and the built-in input streams (`MouseMotion`, `MouseWheel`, `MouseButtonInput`) are
  read via `MessageReader` (`shared/src/{lib,part,player}.rs`, `client/src/{input,ui}.rs`,
  `client/src/platform/web.rs`).
- **`bevy_render` split into focused crates** (better modularity / compile times). Types
  moved out of `bevy::render::*` / `bevy::pbr::*` to new facade modules — and the old paths
  are now *private*, so this is a compile error, not a deprecation: `AmbientLight` /
  `CascadeShadowConfigBuilder` / `NotShadowCaster` → `bevy::light`; `Indices` /
  `VertexAttributeValues` → `bevy::mesh`; `RenderAssetUsages` → `bevy::asset`; `ShaderRef`
  → `bevy::shader`. `Camera` is still in the prelude (drop the explicit
  `bevy::render::camera::Camera` imports). `AsBindGroup` and `PrimitiveTopology` stayed in
  `bevy::render::render_resource`. cargo's "import directly" suggestions point at the
  *inner* crates (`bevy_light::…`) which aren't our deps — use the `bevy::<module>` facade
  re-export instead (`bevy_internal/src/lib.rs` maps `bevy_light as light`, etc.).
- **bevy_egui 0.35+ deprecated single-pass; multipass is the only path.** Add the plugin
  with `EguiPlugin::default()` (the old `enable_multipass_for_primary_context` flag is
  deprecated), move egui-drawing systems out of `Update` into the **`EguiPrimaryContextPass`**
  schedule, and make them fallible — `EguiContexts::ctx_mut()` now returns `Result`, so the
  systems return `Result` and use `ctx_mut()?` (`client/src/ui.rs`). Systems that only read
  input/`EguiContextSettings` (not the context) stay in `Update`.
- **`CursorOptions` is its own component.** 0.15 moved the cursor fields off `Window` into a
  `cursor_options` sub-struct; 0.17 promotes that to a standalone `CursorOptions` component
  on the window entity — query `Query<&mut CursorOptions, With<PrimaryWindow>>` directly
  instead of going through `Window` (`client/src/platform/native.rs`).
- **`weak_handle!` → `uuid_handle!`.** The macro was renamed (the `Handle::Weak` variant
  became `Handle::Uuid`); import `bevy::asset::uuid_handle`. The UUID is unchanged so the
  registered shader handle is byte-for-byte identical (`render_secondary_pass/gizmo_material.rs`).
- **wgpu 25 shuffled the 3D bind groups again.** Material resources moved off `@group(2)`;
  use the `#{MATERIAL_BIND_GROUP}` shader-def placeholder instead of a hardcoded group index
  so the WGSL tracks the engine (`client/assets/gizmo_material.wgsl`). Runtime shader-compile
  failure, not a Rust error.
- **Smaller renames:** system sets standardised on the `*Systems` suffix
  (`TransformSystem::TransformPropagate` → `TransformSystems::Propagate`,
  `client/src/render_secondary_pass/normalization.rs`); `GlobalTransform::compute_matrix` →
  `to_matrix`.

### Bevy 0.18 migration gotchas

The 0.17 → 0.18 bump (third-party deps: `bevy_rapier3d` 0.32 → **0.34** — 0.33 is the
first 0.18 release, 0.34 the latest; `bevy_egui` 0.38 → **0.39** — *not* 0.40, which
already targets Bevy 0.19; `bevy_easings` 0.17 → 0.18). MSRV rose to **1.89**, still well
under the 1.96 pin, so the toolchain is unchanged. wgpu went 25/26 → **27**. This was a
small, mostly-mechanical bump — only four code changes:

- **Ambient light split into a component *and* a resource.** The scene-wide ambient that
  0.17 set via `insert_resource(AmbientLight { .. })` moved to a dedicated
  `GlobalAmbientLight` **resource**; the `AmbientLight` **component** now only overrides
  ambient per-camera. Swap the resource type — the fields and the `360.0` lux value are
  unchanged (`bevy::light::GlobalAmbientLight`, `client/src/main.rs`).
- **`AssetLoader` now requires `TypePath`.** 0.18 added a `TypePath` supertrait bound to
  `AssetLoader` (and `AssetSaver`/`AssetTransformer`/`Process`). The generic
  `RonConfigLoader<T>` must derive it (`#[derive(TypePath)]`; the derive bounds
  `T: TypePath`, which every config satisfies via its `Asset` derive) (`shared/src/config.rs`).
- **rapier 0.34 renamed `Velocity` fields and switched joint frames to glam.**
  `Velocity::linvel`/`angvel` → `linear`/`angular` (`shared/src/{character,part}.rs`). The
  raw `GenericJoint` frame's `local_frame*.translation` is now a glam `Vec3`, so the old
  nalgebra `.translation.vector` access becomes plain `.translation` (`shared/src/part.rs`).
- **`bevy_input` split its sources behind features** (`mouse`/`keyboard`/`gamepad`/`touch`/
  `gestures`). `bevy_window`/`bevy_winit` and `bevy_gilrs` auto-enable the ones they need,
  so the client (pulls `bevy_winit`, plus `bevy_gilrs` on native) and the server (references
  only the input *types*, not the gated systems) both build unchanged — **no explicit
  feature needed**. If a future headless consumer references `KeyCode`/`MouseButton`/
  `ButtonInput` and fails to resolve them, add `bevy/keyboard` + `bevy/mouse`.
- **`NextState::set` always triggers transitions now** — it fires `OnEnter`/`OnExit` even
  when the next state equals the current one (`set_if_neq` restores the old skip-if-same
  behaviour). Every `next_state.set(..)` here is already guarded by a state check, so no
  change was needed; watch this if you add an unguarded `set`.
- **No-ops for us:** bevy_egui 0.38 → 0.39 only removed the deprecated `PICKING_ORDER`
  const (we don't use picking) — the multipass / `EguiPrimaryContextPass` /
  `ctx_mut() -> Result` shape is unchanged from 0.17. bevy_easings 0.18's
  `custom_ease_system::<(), C>` signature is unchanged. wgpu 27 needed no shader/bind-group
  changes (`#{MATERIAL_BIND_GROUP}` already tracks the engine). web-sys/wasm-bindgen stayed
  at 0.3.102 / 0.2.125, so the CI `WASM_BINDGEN_VERSION` pin is unchanged.

### Temporarily on Bevy 0.18 for multiplayer (lightyear)

The repo reached Bevy **0.19** (the "Bevy 0.19 migration gotchas" section below is
that bump), then **stepped the engine back to 0.18** to add multiplayer. Reason:
multiplayer uses **`lightyear`**, and as of this writing lightyear — and the rest
of the Bevy netcode ecosystem (`bevy_replicon`, `bevy_ggrs`, `bevy_matchbox`) —
targets Bevy **0.18**; none had a 0.19-compatible release yet (0.19 support is
expected within weeks of the 0.19 engine release). Rather than wait, the engine was
downgraded, *structured so the re-bump to 0.19 is near-trivial*:

- **All engine-coupled version pins live in `[workspace.dependencies]` in the root
  `Cargo.toml`** (`bevy`, `avian3d`, `bevy_egui`, and eventually `lightyear`). Member
  crates inherit them via `{ workspace = true }` and only select *features* locally.
  This is the single knob to turn on upgrade.
- **The downgrade reversed exactly the commit-#35 bump** (`bevy 0.19→0.18`,
  `avian3d 0.7.0→0.6.1`, `bevy_egui 0.40→0.39`) plus its two code changes. The
  third change from #35 (the synthetic `MouseWheel { phase }` in `web.rs`) was
  already gone — the winit-native input rewrite (#36) deleted that code path.
- **The two code changes to re-apply on the 0.19 re-upgrade** are exactly the ones
  the "Bevy 0.19 migration gotchas" section documents: `DirectionalLight`
  `shadows_enabled` → `shadow_maps_enabled` (`render_main_pass.rs`), and the egui
  zoom-factor handling in `update_ui_scale_factor` (`ui.rs`). That section is the
  canonical list; the root `Cargo.toml` block has the step-by-step checklist.
- **Toolchain/CLI pins are unchanged** across the downgrade and the eventual
  re-upgrade: Rust `1.96.0` (above both 0.18's MSRV 1.89 and 0.19's 1.95) and
  `wasm-bindgen`/`web-sys` `0.2.125`/`0.3.102` (both engine versions resolve them).

### Bevy 0.19 migration gotchas

> **Currently reversed** — the repo is on Bevy 0.18 for multiplayer (see
> "Temporarily on Bevy 0.18 for multiplayer" above). This section is retained as the
> canonical record of the 0.18 → 0.19 changes to **re-apply** when lightyear ships a
> 0.19-compatible release.

The 0.18 → 0.19 bump (third-party deps: `avian3d` 0.6.1 → **0.7.0** — the Avian release
targeting 0.19, parry3d 0.26 → 0.27; `bevy_egui` 0.39 → **0.40** — targets 0.19, bundles
egui 0.34). MSRV rose to **1.95**, still under the 1.96 pin, so the toolchain is unchanged.
wgpu went 27 → **29**. This bump was *tiny* — `shared`/server compiled with zero code
changes, and the client needed only two one-line fixes. The 0.19 guide's two headline
changes (Resources-are-Components, rendering-as-systems) don't touch anything here.

- **`DirectionalLight::shadows_enabled` → `shadow_maps_enabled`.** A straight field rename
  (`client/src/render_main_pass.rs`). The only Bevy-side code change in the whole bump.
- **bevy_egui 0.40 removed `EguiContextSettings::scale_factor`.** UI scaling moved to egui
  0.34's per-context **zoom factor**. The Ctrl +/- zoom handler (`update_ui_scale_factor`,
  `client/src/ui.rs`) now calls `ctx.set_zoom_factor(..)` instead of writing the old
  settings field, and turns egui's *built-in* keyboard zoom off
  (`options.zoom_with_keyboard = false`) so the two don't both react to the same keypress.
  Because it now touches an egui context, the system moved from `Update` into
  `EguiPrimaryContextPass` and became fallible (`-> Result`, `ctx_mut()?`).
- **`bevy/webgl2` is still valid in 0.19** — the umbrella `bevy` crate exposes `webgl2`
  (it forwards to the inner `bevy_internal/webgl`), so the `web` feature is unchanged.
  Don't be fooled by `bevy_internal` naming the feature `webgl`; the public name is `webgl2`.
- **wasm-bindgen / web-sys unchanged at 0.2.125 / 0.3.102** despite wgpu 27 → 29 (verified by
  regenerating the lock), so the client `=0.2.125` pin and CI `WASM_BINDGEN_VERSION` hold.
- **Lockfile regenerated from scratch** (`rm Cargo.lock && cargo generate-lockfile`), the
  prescribed move on a Bevy bump: single Bevy 0.19 in the graph, no duplicate engine.
- **Known deferred deprecations (warnings, not errors):** egui 0.34 deprecated the
  top-level panel-on-context API — `egui::TopBottomPanel::{top,bottom}` and `Panel::show(ctx, ..)`
  (used by `show_instructions`/`show_bottom_panel` in `ui.rs`) warn, pointing at
  `Panel::{top,bottom}` + `show_inside(ui, ..)`. They still compile and render identically;
  the replacement nests panels inside a `Ui` rather than a context, a non-trivial restructure
  with layout-regression risk, so it's left as a separate egui-focused follow-up.
- **Verify at runtime (wgpu 29):** the custom gizmo overlay shader
  (`client/assets/gizmo_material.wgsl`). The migration guide lists no required WGSL/bind-group
  changes and `#{MATERIAL_BIND_GROUP}` tracks the engine, but wgpu major bumps occasionally
  surface as a runtime shader-compile failure (not a Rust error) — eyeball the gizmo/cone
  overlays and scene lighting/shadows on the live build.

## Migrating bevy_rapier3d → Avian

The physics engine was swapped from `bevy_rapier3d` 0.34 to **`avian3d` 0.6.1** (the
Avian release that targets Bevy 0.18; it uses parry3d 0.26 under the hood). The
motivation: Avian tracks Bevy releases far more promptly than rapier — at the time of
the swap, rapier (even its git `master`) still targeted Bevy 0.18 with no 0.19 build,
while `avian3d` 0.7 already targeted 0.19 — so moving to Avian is the path to future
Bevy bumps. This migration deliberately *stayed on Bevy 0.18* to isolate the
physics-engine change from any engine bump. Notes for anyone touching physics:

- **Cargo features.** `avian3d` is pulled with `default-features = false, features =
  ["3d", "f32", "parry-f32", "xpbd_joints"]` in all three crates. Avian's default
  features include `debug-plugin` (pulls `bevy_render` — bad for the headless server)
  and `parallel` (pulls `bevy/multi_threaded` — unwanted on wasm), so they're off by
  default; the native client and the server re-add `avian3d/simd` + `avian3d/parallel`
  for perf (rapier's `simd-stable`/`parallel` equivalents). `parry-f32` is what brings
  in the parry-backed `Collider`.
- **`xpbd_joints` is a *default* Avian feature gating the joint *solver* — it MUST be
  re-added under `default-features = false`.** This one bites silently: the joint
  *types* (`SphericalJoint`, `FixedJoint`, …) and `JointGraphPlugin` compile and the
  joints spawn fine *without* the feature, but `XpbdSolverPlugin` (the system that
  actually enforces joints) is `#[cfg(feature = "xpbd_joints")]`-gated and is the only
  thing `PhysicsPlugins` adds for joint solving. Drop the feature and attached parts
  never move together — the joint sits inert in the world with no error. Symptom:
  shift-click attach "works" (potential-joint dots appear, a `SphericalJoint` entity is
  spawned) but lifting the held part leaves the attached part behind. Enabling the
  feature is the whole fix (the solver is auto-added by `PhysicsPlugins` once it's on).
- **`shared` must now name the bevy features rapier used to pull in transitively.**
  bevy_rapier's *default* features dragged `bevy_render` / `bevy_core_pipeline` / the
  `bevy_input` source features into `shared` as a side effect; Avian (with
  `default-features = false`) does not. Since `shared` references those types directly
  (player.rs spawns a `Camera3d` + reads `Tonemapping`; lib.rs uses `KeyCode` /
  `MouseButton`), `shared`'s `default` feature now explicitly lists `bevy/bevy_render`,
  `bevy/bevy_core_pipeline`, `bevy/keyboard`, `bevy/mouse`. Symptom if missing: `cannot
  find type Camera/Camera3d`, `unresolved import KeyCode/MouseButton` in `shared`.
- **`PhysicsPlugins` is a `PluginGroup`, not a single `Plugin`** — nest it into
  `CommonPlugins` with `add_group`, not `add` (rapier's `RapierPhysicsPlugin` was a
  single plugin) (`shared/src/lib.rs`).
- **Component/API renames.** `RigidBody::Fixed` → `Static`; `Collider::ball` →
  `Collider::sphere`; `Collider::cuboid` now takes **full** extents (rapier took
  half — drop the `/2.0`); `Collider::trimesh` (fallible) → `Collider::try_trimesh`;
  `Velocity{linvel,angvel}` → separate `LinearVelocity` / `AngularVelocity` `Vec3`
  newtypes; `ColliderMassProperties::Density` → `ColliderDensity`;
  `Friction::coefficient`/`Restitution::coefficient` → `::new`; `Ccd::enabled()` →
  `SweptCcd::default()`; gravity moved off the `RapierConfiguration` component to a
  `Res<Gravity>` resource. `LockedAxes::ROTATION_LOCKED` is unchanged. Avian collides
  all collider pairs by default, so rapier's `ActiveCollisionTypes` / `ActiveEvents`
  opt-ins are simply dropped.
- **Forces are applied through the `Forces` query helper, not an `ExternalForce`
  component** (which Avian removed in 0.4). Add `Forces` to a `Query` *without*
  `&`/`&mut`; it accumulates during the physics step and **auto-clears** afterwards —
  so the old per-frame "zero the force" system is gone. `Forces` takes
  `LinearVelocity`/`AngularVelocity` **mutably** internally, so it *cannot* share a
  query with a `&LinearVelocity`/`&AngularVelocity` — read those off the helper
  (`forces.linear_velocity()` / `.angular_velocity()`, from the `ReadRigidBodyForces`
  trait — import it *and* `WriteRigidBodyForces`, since supertrait methods need the
  trait in scope). Held-part control now calls `apply_linear_acceleration` /
  `apply_angular_acceleration` and lets Avian do the mass/inertia conversion (rapier
  multiplied by mass / principal inertia by hand). Two systems both writing through
  `Forces` on the same parts must be **explicitly ordered** (`.after(...)`) or Bevy
  flags an ambiguous double-write; a single query mixing `Forces` with a conflicting
  `&mut`/`&` of its internal components **panics at startup** ("Mutable component
  access must be unique").
- **Contacts: `Collisions` system param.** `rapier_context.contact_pairs_with(e)` →
  `collisions.collisions_with(e)` (the `Collisions` param is a read-only view over the
  contact graph, yielding touching pairs). `ContactPair::collider1/2` are plain
  `Entity` (rapier's were `Option`); `has_any_active_contact()` → `is_touching()`;
  `.manifolds()`/`.points()` methods → `.manifolds`/`.points` **fields**.
  **`ContactPoint` has no local-frame points** — only world-space, **COM-relative**
  `anchor1`/`anchor2` (+ a world `point`), and `penetration` is **positive when
  overlapping** (rapier's `dist()` was negative). The part-attach logic needs each
  contact point in the body's *local* frame, so it rotates the anchor by the body's
  inverse rotation (`R⁻¹ · anchorN`); parts are centered uniform cuboids, so COM ==
  origin and that's exact.
- **Joints are standalone entities**, not children of a body. `SphericalJointBuilder` +
  `ImpulseJoint::new(other, joint)` (spawned as a child of one body) →
  `SphericalJoint::new(body1, body2).with_local_anchor1(a1).with_local_anchor2(a2)`,
  spawned as its own entity. Read anchors back with `joint.local_anchor1()` /
  `local_anchor2()` (→ `Option<Vec3>`, plain glam — no raw nalgebra frame digging) and
  the bodies via the public `joint.body1`/`body2` fields (rapier needed the `ChildOf`
  parent + `joint.parent`). **Caveat:** because the joint no longer hangs off a body,
  despawning a body does *not* take its joints with it (rapier's recursive child
  despawn did) — a fallen/attached part that gets despawned can leave a dangling joint
  referencing a missing body. Avian tolerates this without crashing, but watch for it
  if attached assemblies fall off the platform.
- **Collider shape introspection** (building render meshes from colliders, in
  `client/src/render_main_pass.rs` + `player.rs`): rapier's `collider.as_cuboid()` /
  `.as_ball()` / `.as_trimesh()` / `.raw.compute_local_bounding_sphere()` → go through
  the parry shape with `collider.shape().as_cuboid()` etc. parry's `Cuboid::half_extents`
  and `Ball::radius` are **fields** (not methods), and `TriMesh::vertices()` returns a
  **slice** of nalgebra `Point3<f32>` (rapier's view yielded glam-like points) — convert
  to glam up front. The dead nalgebra glam↔quaternion helpers in `utils.rs` (which
  imported `bevy_rapier3d::na`) were unused and were deleted outright.
- **Behaviors to verify by play-testing** (clean compile, but physics differs): ground
  detection (now `penetration > -0.002` instead of rapier's `dist() < 0.002`), the
  held-part *orientation* feel (acceleration via the full inertia tensor vs rapier's
  principal-inertia vector), and attach-point placement (the world→local anchor
  conversion). The headless server smoke-tests clean (parts spawn/fall/settle, no
  panic), but the holding/attaching/joint-deleting flow is input-driven and only
  exercised in the client.

## Dropping bevy_easings

`bevy_easings` was removed from the workspace. It tracks Bevy releases slowly — at
the time of writing its latest release (and even its git `main`) still targeted Bevy
0.18 while `avian3d` 0.7 and `bevy_egui` 0.40 already targeted 0.19 — so it was the
*sole* remaining blocker for the Bevy 0.18 → 0.19 bump. Rather than wait on upstream,
the one effect it powered was hand-rolled:

- It drove a single camera transition: the camera-orbit-center eases to a new offset
  on pick-up (`adjust_camera_on_hold`) and back on release (`reset_camera_after_release`),
  a `Vec3` `QuadraticInOut` tween over 0.5 s. That was the whole dependency.
- The replacement is a self-contained `CameraTween { start, end, elapsed, duration }`
  component plus a `quadratic_in_out` helper, both in `player.rs`. The trigger systems
  `insert` a `CameraTween` (replacing any in-progress one, so re-triggering restarts
  from the current position); `ease_camera` advances `elapsed` by `time.delta_secs()`,
  writes `start.lerp(end, quadratic_in_out(t))` into the orbit center's
  `Transform.translation`, and `remove`s the component once `t >= 1` (snapping exactly
  to `end`). This matches the old `EasingType::Once` semantics (eases once, then holds).
- The old `Translation` newtype + its `bevy_easings::Lerp` impl, the
  `custom_ease_system::<(), Translation>` registration, and the `easing: Translation`
  bundle field are all gone. `ease_camera` stays in the `EaseLabel` set so
  `mouse_motion.after(EaseLabel)` ordering (rotation written after translation) is
  preserved.
- Removing the dep also pruned its unique transitive crate `interpolation` from the
  lockfile; nothing else moved.

## Unifying web input on winit-native

The web build used to wire **all** browser input by hand through DOM event listeners
(`web-sys` / `gloo`): a `KeyCode`/`MouseButton` newtype each (`WebKeyCode` /
`WebMouseButton`), four trackers (`KeyboardTracker` / `MouseClickTracker` /
`WheelTracker` / `MouseMovementTracker`) feeding parallel `ButtonInput<Web*>`
resources, plus a `MergedKeyboardInput` step in `input.rs` that OR'd the web stream
into the native one. That whole layer dated to the **Bevy 0.12 era**, when winit's
web input didn't work. It was removed — winit **0.30** (on Bevy 0.19) delivers
keyboard, mouse buttons, the scroll wheel, and pointer-lock mouse motion natively on
web, so `input.rs` now has a **single** code path reading the standard
`ButtonInput<KeyCode>` / `ButtonInput<MouseButton>` / `MouseWheel` / `MouseMotion` on
both platforms.

- **The decisive evidence was in the repo.** The old `get_mouse_motion` had to
  `clear()` winit's `MouseMotion` buffer every frame because winit *already* emitted
  its own deltas under pointer lock (they double-fed the look and spun the camera).
  That bug-we-fought is exactly the feature we now depend on: winit emits `MouseMotion`
  whenever the document is pointer-locked, **even though `web.rs` requests the lock
  itself** (not via winit's `CursorGrabMode`) — so mouse-look needs no DOM listener.
- **Removing the hand-rolled keyboard tracker also *fixes* latent bugs**, not just
  simplifies: it only ever wired `W/A/S/D/Space/ShiftLeft/ControlLeft` (no `ShiftRight`,
  no other keys) and dropped keys on focus loss (raw `keyup` listeners). winit handles
  all keys and focus transitions properly.
- **What stayed in `web.rs`:** the `pointerlockchange` listener + `request_pointer_lock`
  (browser owns the Esc-exit gesture; `toggle_menu_on_pointer_lock` keys off lock state),
  `signal_game_ready`, and the `get_document`/`get_body`/`listen` helpers. The
  `AtomicBoolExt` trait shrank to `store_val`/`get` (its `toggle` and the whole
  `AtomicI32Ext` were only used by the deleted trackers).
- **`shared` shed `WebKeyCode`/`WebMouseButton`** and no longer references
  `KeyCode`/`MouseButton` itself, but its `bevy/keyboard` + `bevy/mouse` features are
  **kept** so those types still compile workspace-wide for the client (`input.rs`) and
  server, carried to every consumer by feature unification.
- **Sensitivity caveat (verify on Pages):** the old DOM path scaled deltas by `0.15`
  before the per-config look sensitivity; winit's native deltas skip that, so web
  mouse-look may feel faster/slower than before and now matches native — tune the
  `player.player.ron` look sensitivity if needed. Also note Bevy issue
  [#18855](https://github.com/bevyengine/bevy/issues/18855) (open, `S-Blocked`): on
  some browsers native wasm `MouseMotion` under pointer lock under-reports slow motions.
  If mouse-look regresses on web, the fallback is to reinstate **only** the
  `MouseMovementTracker` DOM shim (movementX/Y) for motion while keeping everything
  else on the winit-native path.

## Pull request workflow

Whenever the user asks to open a pull request, do all of the following before
ending the turn (in order):

1. **Document lessons learned** — capture anything non-obvious discovered while
   doing the work (gotchas, dead ends, decisions) in the PR description and/or the
   relevant docs so it isn't lost.
2. **Run a `/simplify` pass** over the changes and apply the cleanups.
3. **Rebase on `master`** (the default branch; `git fetch` + `git rebase
   origin/master`), resolving any conflicts, before pushing.
4. **Monitor the PR until it is fully ready to merge** — subscribe to PR activity,
   keep CI green, and address review feedback until the PR is mergeable.

## Multiplayer front-end & matchmaker

The web front door is **plain static HTML** (no WASM until you actually play, so
it paints instantly), split into three pages under `client/` — all published to
the site root by the Pages CI (`cp client/*.html _site/`):

- **`index.html`** — landing page. Two buttons: *Single Player* → `play.html`
  (loads the game offline, exactly as before), *Multiplayer* → `lobby.html`.
- **`play.html`** — the game loader (the old `index.html`: progress bar, WASM
  streaming, `data-game-ready` handshake — unchanged). It additionally parses
  `?room=CODE` / `?server=` off the URL into `window.__BS_NET__` *before* WASM
  init, so the client can read who to connect to on boot (consumed by the live
  netcode tier). No `room` ⇒ single-player.
- **`lobby.html`** — the lobby browser. Lists open matches, *Create Match*
  (→ shareable `play.html?room=CODE` link + Enter), *Join by code*, and join
  buttons per row. Auto-refreshes every 4s. Talks to the matchmaker via
  `fetch()`.

**`matchmaker/`** (`bad-spaceship-matchmaker`, bin) is the lobby-coordination
tier — a small **Axum** service, deliberately decoupled from the game (no
bevy/lightyear/avian). In-memory lobby store; endpoints `GET /api/health`,
`GET /api/matches`, `POST /api/matches`, `POST /api/matches/{id}/join`. Lobby
codes are 6-char unambiguous (no 0/O/1/I/L). Run it with
`cargo run -p bad-spaceship-matchmaker` (env: `BIND` default `0.0.0.0:5000`;
`STATIC_DIR=client` to also serve the HTML on the same origin for local
single-origin testing). CORS is `permissive()` for now — **lock it to the site
origin before going public.**

- **Hosting:** GitHub Pages serves the static HTML; it *cannot* run the
  matchmaker (or the game server). The matchmaker must run somewhere with a
  public endpoint (the Mac box / a small VPS).
- **Pointing the deployed lobby at the matchmaker:** `lobby.html` resolves the
  matchmaker base URL as `?api=<url>` (testing) → `window.BS_MATCHMAKER_URL`
  (set this for the deployed site) → `http://localhost:5000` (default, so a
  fresh `cargo run` works out of the box). Browsers also can't *host* a server,
  so "Create Match" from the web provisions a server-side match — it never makes
  the browser a host.

## Multiplayer netcode (lightyear 0.27)

Server-authoritative netcode over **lightyear 0.27** (the release targeting Bevy
0.18 — the reason the engine is held at 0.18). Current state is a **thin vertical
slice**: a dedicated server accepts WebSocket clients and replicates a player
entity per connection; the client draws each replicated player. It is **gated
off by default** (env vars below) so single-player is byte-identical, and the
live connection is **compile-verified on native + wasm** but the end-to-end
session still needs real endpoints to exercise.

**Protocol** (`shared/src/net.rs`, `ProtocolPlugin`): registered with the 0.27
builder API `app.component::<C>().replicate()` (the old `register_component` is
deprecated). Replicates `NetPlayer` (owner id) and **`NetTransform`** — a plain
`[f32;3]`+`[f32;4]` pose mirror, because Bevy's `Transform` isn't `Serialize` and
`.replicate()` requires it; map it to/from `Transform` on each side.

**Server** (`server/src/net.rs`, `NetServerPlugin`, added only when
`BS_MULTIPLAYER` is set): adds `ServerPlugins { tick_duration }`, then the
protocol, then spawns the server entity `(NetcodeServer::new(NetcodeConfig::
default()), LocalAddr(addr), WebSocketServerIo { config })` and triggers `Start {
entity }`. An `On<Add, Connected>` observer spawns a `Replicate::to_clients(
NetworkTarget::All)` player per client. Bind via `BS_SERVER_BIND` (default
`0.0.0.0:5001`).

**Client** (`client/src/net.rs`, `NetClientPlugin`, added only when
`multiplayer_target()` is `Some`): adds `ClientPlugins`, the protocol, spawns
`(NetcodeClient::new(Authentication::Manual{..}, ..), WebSocketClientIo::from_url
(..))` and triggers `Connect { entity }`; a system draws a cube on each
replicated `NetPlayer` and applies `NetTransform`. `multiplayer_target()` is
cfg-split: **native** reads `BS_CONNECT=host:port` (plain `ws://`); **wasm** reads
`window.__BS_NET__.server` — the `wss://host[:port]` URL `play.html` parses from
the `?server=` query param (absent/empty ⇒ single-player). Both platforms build
the client with `WebSocketClientIo::from_url(..)`; the only differences are the
`ClientConfig` (native `builder().with_no_encryption()` vs wasm's unit struct, since
the browser owns TLS) and the netcode token's `server_addr` (real on native; a
`0.0.0.0:0` placeholder on wasm, because a `wss://` URL targets a hostname and the
browser connects via the explicit URL, making `server_addr` a logical-only field).
Reading the `__BS_NET__` global uses `js_sys::Reflect` (js-sys is in the `web`
feature). **Web `wss://` multiplayer is verified live** end to end from mobile
Safari — see "Live test endpoint" below.

**Networked input → pose mirroring** (`PlayerInput`, `shared/src/net.rs`). The
client controls its own player and the server mirrors that to everyone, using
lightyear's **native message inputs** (the `input_native` feature; `InputPlugin::
<PlayerInput>` registered once in `ProtocolPlugin`, role-agnostic — it adds the
client half under lightyear's `client` feature and the server half under `server`,
so one registration wires both binaries). Native inputs require `Serialize`/
`Deserialize`/`Clone`/`PartialEq`/`Debug`/`Default` + `Reflect` + `MapEntities`
(no-op here). The flow:
- **Server**: each per-client player spawns with `ControlledBy { owner: client,
  lifetime: SessionBased }` (binds that client's input to the entity; auto-despawns
  on disconnect) + a seeded `ActionState::<PlayerInput>`. `apply_player_input`
  (FixedUpdate) writes the received pose into the replicated `NetTransform`.
- **Client**: the bound entity arrives carrying lightyear's `Controlled` marker;
  `mark_controlled_player` tags it with `InputMarker<PlayerInput>` + `ActionState`.
  `write_player_pose` (FixedPreUpdate, in the `WriteClientInputs` set) fills the
  `ActionState`, and lightyear sends it.
- **Why pose, not movement intent.** The player is a single rotation-locked Avian
  sphere (`Player` and `Character` are the *same* entity), so a kinematic
  re-simulation on the server drifts and feels wrong (tried first: inverted/world-
  space direction, mismatched speed, floats). Instead the client forwards its
  character's authoritative `GlobalTransform` translation + a yaw-derived rotation
  (`Quat::from_rotation_y(-yaw)`, matching `move_character`'s look basis, since the
  ball's own rotation is locked to identity), and the server just mirrors it. The
  avatar then tracks position *and* heading exactly, offset only by network round-
  trip — which is also what a second client should see. The replicated cube carries
  a contrasting front "nose" child so the yaw is visible. (`ActionState`/
  `InputMarker`/`Controlled` paths: `lightyear::prelude::input::native::*` and
  `lightyear::prelude::{Controlled, ControlledBy, Lifetime}`; the client
  write-set is `lightyear::prelude::client::input::InputSystems::WriteClientInputs`.)

**Interpolation** smooths the replicated motion. `NetTransform` is registered with
`add_interpolation_with(lerp_net_transform)` (a translation-lerp / rotation-slerp,
since `NetTransform` isn't `Ease`), and both the per-client players and the demo
bot carry `InterpolationTarget::to_clients(All)`. lightyear then maintains, on each
receiving client, a separate **`Interpolated`** entity whose `NetTransform` it eases
between confirmed snapshots every frame. The client renders the `Interpolated`
copies (`draw_replicated_players`/`apply_net_transform` filter `With<Interpolated>`);
the raw **`Confirmed`** entities stay invisible but still carry `Controlled`, so
input/control is unaffected (`InterpolationTarget` and `ControlledBy` are
orthogonal — the owner gets both a Confirmed entity it controls and an Interpolated
copy it renders). Trade-off: a small fixed interpolation delay (needs two snapshots
to blend) in exchange for smooth motion. (`Interpolated`, `InterpolationTarget`,
and the `add_interpolation_with`/`LerpFn` registration are in `lightyear::prelude`
under the `interpolation` feature.)

**Prediction of the owner's own avatar.** Interpolation alone leaves the owner's
own avatar a fixed delay behind their character. But this is a *client-authoritative
pose* model — the client forwards its real `Character` pose, the server only mirrors
it — so the client already knows its own position with **zero delay** (the local
pose), and "prediction" is exact and rollback-free (unlike lightyear's server-
authoritative input-replay prediction, which doesn't fit). `mark_own_avatar` tags
the `Interpolated` entity whose `NetPlayer.client_id` matches the `Controlled` one
with `OwnAvatar`; `apply_net_transform` excludes `OwnAvatar` (so interpolation
doesn't fight it) and `predict_own_avatar` drives its transform from the live local
character pose each frame. Other players' avatars and the demo bot stay interpolated.
Gotcha: read the character's **`Transform`, not `GlobalTransform`** — the latter is
only refreshed in PostUpdate propagation, so reading it in `Update` lags a frame
(a visible trail that converges on stop); the character is a root entity, so its
`Transform` is the current world pose and the avatar then propagates in lockstep
with the rendered character.

**Shared part world (server-authoritative, slice 1).** The loose blocks are no
longer two independent local sims — the server simulates them and replicates them
so every client sees the same world. A replicated `NetPart { half_extents }` carries
the cuboid shape; `replicate_parts` (server) tags each spawned part (`Holdable` +
cuboid collider) with `NetPart` + `NetTransform` + `Replicate` + `InterpolationTarget`,
and `sync_part_transforms` streams each part's authoritative pose into `NetTransform`
(only on change, so settled parts go quiet). The client inserts a `SuppressLocalParts`
marker resource (in `NetClientPlugin::build`) that gates off `PartPlugin`'s
part-creation systems (`spawn_initial_parts`/`spawn_part`/`replace_fallen_parts`),
and `draw_replicated_parts` renders the replicated parts (cuboid mesh from `NetPart`,
pose via the shared `apply_net_transform`). The authoritative server never inserts
`SuppressLocalParts`, so it keeps simulating. **Slice 2** makes the parts *collidable*:
each replicated part also gets a `RigidBody::Kinematic` + a cuboid `Collider` (full
extents = 2 × `half_extents`), so it follows the server's interpolated pose while
blocking the local dynamic character — the player bumps the shared world but can't
push the parts (the server stays authoritative). *Limitation (next slice):* grab/
attach is suppressed in multiplayer (`SuppressLocalParts`), so building over the
network — and the networked *dynamic* interaction that would let a player shove a
block — is the remaining piece.

**Networked grab/hold (slice 3).** A player can now pick up a shared block over
the network, server-authoritative. `PlayerInput` carries `grab` plus the real
`grab_origin` (camera-orbit-center) and `hold_target` (HoldPoint) world positions
— forwarded from the client's actual entities (`write_player_pose`) rather than
recomputed, so the hold matches single-player exactly (the hold point hangs off
the orbit center, *above* the character — recomputing it as `char + look×5` put it
too low). The grab intent is a client-side toggle (`WantHold`, flipped on each
non-`Modifying` `PlayerClick` in `read_grab_intent` — works on desktop click and
the mobile grab button alike, since the local `toggle_holding` is inert with no
local parts to focus). On the server, each player avatar carries a `HeldPart`;
`server_grab` latches the part the player is most directly looking at
(`focused_part` — smallest look-angle within `MAX_INTERACT_DISTANCE`/`ANGLE`,
matching `update_focused`), and `server_hold` floats it to the hold point with the
*same* critically-damped **anti-gravity force** as `position_held_part`
(`hold_acceleration` via Avian's `Forces`, `apply_linear_acceleration(accel −
gravity)`) — keeping the part **dynamic** so it still collides (an earlier
kinematic version tunnelled through the floor). The selection/hold helpers live in
`shared/src/net.rs` so client highlight and server agree. Client feedback reuses
the **real** game gizmo: `highlight_grabbable` tints the focused part yellow (the
single-player focus colour), and `position_gizmo` (secondary pass) was extended to
place the existing `GizmoHub` (RGB axes) at the hold point in multiplayer (the
local hold that normally drives it is suppressed). The delete-zone sphere overlay
is gated off under `SuppressLocalParts`.

**Networked attach + orientation + joint visuals (slice 4).** Players can now
*build* over the network — rotate a held block to a target orientation and attach
it to another, server-authoritative, with the single-player visuals (joint
previews, the orientation gizmo, the context button) all lit up. The key tactic
is **mirroring the networked grab back into the local single-player state** so the
game's own systems engage instead of being re-implemented:
- **`mirror_grab_state`** (client) writes the networked grab into the local
  Player's `Holding` + `FocusedInteractable` (latching the looked-at part with the
  same `focused_part` rule the server grabs by, over the `Interpolated` parts — the
  copies that carry the collider Avian reports contacts on). That single mirror
  re-engages: the **mobile button label** (keys on `Holding` → "Join Parts" vs
  "Delete Joints"), **`update_active_joints`** → `PotentialJoints` →
  **`display_potential_joints`** (the violet potential-joint preview, rendered with
  the real `JointAppearance` assets), and the **rotate gesture** (mobile
  `apply_pointer` keys rotate-mode on `Holding`, so it feeds `Modifying` +
  `MouseMotionDelta` → `set_part_rotation` computes the player's `PartRotation`).
  `toggle_holding` (player.rs) and `assign_parts` (render_main_pass.rs) are gated
  off under `SuppressLocalParts` so the local path doesn't fight the mirror.
- **Orientation** is client-tracked and server-driven, mirroring single-player's
  `TargetOrientation` accumulation but over the wire. `HeldRotation` (client
  resource) is **seeded to the part's orientation at pickup** and each frame folds
  in the rotate gesture (`target = PartRotation * target`, exactly
  `apply_part_rotation`); `write_player_pose` forwards it as
  `PlayerInput::hold_rotation`, and `server_hold` drives the (still dynamic) part
  toward it with `orient_acceleration` (the softer `ORIENTING_STIFFNESS = 5`,
  matching `orient_held_part`, via `to_rotation_vector`'s shortest-path error).
  This intentionally follows the same **client-forwards-a-target / server-springs**
  shape as the *position* hold (the client doesn't carry a real `TargetOrientation`
  entity, so `HeldRotation` is the minimal carrier).
- **Attach** (`server_attach`): on the `attach` intent (modifier click →
  `WantAttach`), joint the held part to whatever other `NetPart` it's touching, at
  the contact anchors — porting single-player's `update_active_joints`/`attach`
  anchor math (`rot⁻¹ · anchor + com`), then release it. Joints are server physics,
  so the joined parts move together and their replicated `NetTransform`s tell the
  story.
- **Joint visuals**: rather than replicate the joint constraint, the server spawns
  a lightweight **`NetJoint`** marker entity at the joint's world anchor and streams
  its pose (`sync_joint_transforms`: `body1.translation + body1.rotation · anchor1`).
  The client draws each replicated `NetJoint` with the game's **real**
  `JointAppearance` mesh + `GizmoMaterial` (`draw_replicated_joints`), so existing
  joints look identical to single-player and draw on top via the secondary pass.
- **Sticky highlight + target gizmo**: `highlight_grabbable` keeps the *held* part
  lit while holding (the latched `FocusedInteractable`) instead of jumping to
  whatever you look at, and only recolours on change (mutating a material flags a
  GPU re-upload). `position_gizmo`'s multiplayer branch orients the `GizmoHub` to
  `HeldRotation` (the target orientation), shown only while holding — so the RGB
  axes indicate the orientation the part is being rotated toward, like single-player.

**0.27 API gotchas worth remembering** (the published book lags the crate; the
ground truth is the crate source in `~/.cargo/registry/src/.../lightyear*-0.27.0`):
- Plugin groups are `ClientPlugins`/`ServerPlugins` structs with a `tick_duration`
  field (Default 1/60s); add the group *before* the protocol *before* spawning the
  connection entity.
- Replication is built on **bevy_replicon** under the hood.
- Connection is **entity/component-based**: `NetcodeClient`/`NetcodeServer`
  components (require `Link`/`Client`), IO components (`WebSocketClientIo` /
  `WebSocketServerIo`), triggered by `Connect`/`Start` (EntityEvents with an
  `entity` field). Observers read the target via `trigger.entity` (a field, not
  `.target()`).
- **Dev transport is plain `ws://` via `with_no_encryption()`** on both
  `ServerConfig::builder()` and `ClientConfig::builder()` — no TLS certs needed
  for a native loopback test. Production / browsers need `wss://` (server
  `with_identity(Identity::self_signed([..]))` or a real cert).
- `Entity` → `u64` is `entity.to_bits()` (`.index()` returns `EntityIndex` now).
- wasm note: aeronet's `ClientConfig` is a unit struct on wasm (the browser owns
  TLS), so the `builder().with_no_encryption()` path is native-only.

**Run the native loopback slice** (two terminals):
```bash
BS_MULTIPLAYER=1 cargo run -p bad-spaceship-server        # ws://0.0.0.0:5001
cd client && BS_CONNECT=127.0.0.1:5001 cargo run --features native
```

The server also spawns one **persistent "demo bot"** at startup (`spawn_demo_bot` /
`move_demo_bot` in `server/src/net.rs`): a server-driven `NetPlayer` that orbits in
a slow circle, its `NetTransform` rewritten every frame so the change replicates to
every client. It exists purely so a **single** client can witness live position
replication (motion streamed over the wire) without a second device — necessary
because mobile browsers suspend background tabs, so two tabs on one phone never hold
simultaneous connections. Remove it once real player movement lands.

**Remaining for real multiplayer** (needs live testing): exercising **two real
devices** for genuine peer visibility (vs the single-device demo bot), and wiring
the **matchmaker** to hand out real game-server endpoints. (Browser `wss://`, a
faithful self-avatar — position + heading, driven from the real `Character` pose
over networked input — **interpolation** of remote avatars, zero-delay
**prediction** of the owner's own avatar, the server-authoritative **shared part
world** — replicated *and* collidable (slices 1–2) — **networked grab/hold** of a
part (slice 3), and **networked rotate + attach** so players can *build*, with the
real joint previews/gizmo/button visuals (slice 4) are **done**, verified live from
mobile Safari.)

### Live test endpoint (Mac mini + Tailscale)

The public `wss://` slice was verified by standing up a throwaway test endpoint on
the Mac box (`mini4`), reachable from a phone over Tailscale. The pieces, all on the
Mac's Docker (Colima) daemon — the cmd-api reaches *inside* the container, but the
`tailscale serve` and Colima port-forward bits are macOS-**host** steps the user runs:

- **Two containers**, both `--restart unless-stopped`, published to host loopback
  (Colima forwards `127.0.0.1:<port>` to the macOS host loopback):
  - `bs-game-server` — the `BS_MULTIPLAYER=1` server binary in a minimal image
    (`debian-slim` + the binary + `../client/assets`), `-p 127.0.0.1:5001:5001`.
  - `bs-web` — `nginx:alpine` serving the web build's `_site` (the same layout the
    Pages CI produces: `wasm-bindgen` output in `target/`, the three HTML files,
    `assets/`, `loader-manifest.json`), `-p 127.0.0.1:8099:80`.
- **The web build is produced on the Mac**, not in the sandbox (heavy `--release`
  wasm). Gotcha: use the **aarch64** `wasm-bindgen` 0.2.125 (the box may have a
  leftover **x86_64** one on `PATH` that segfaults with a bogus "capacity overflow"
  under emulation inside the aarch64 container — exactly the emulation artifact the
  toolchain notes warn about). The native aarch64 binary lives under
  `/root/wasm-bindgen-0.2.125-aarch64-unknown-linux-gnu/`.
- **Tailscale `serve`** on the macOS host bridges the loopback ports onto the tailnet
  *with a valid MagicDNS HTTPS cert* (which is what makes `wss://` work with no
  client certs — Tailscale terminates TLS and proxies the WS upgrade to the plain
  `ws://` server):
  ```
  tailscale serve --bg --https=443  http://127.0.0.1:8099   # web → https://<node>.ts.net/
  tailscale serve --bg --https=8443 http://127.0.0.1:5001   # game → wss://<node>.ts.net:8443
  ```
  (Serve must be enabled once per tailnet in the admin console; allowed HTTPS serve
  ports are 443/8443/10000.) Phone test URL:
  `https://<node>.ts.net/play.html?server=wss://<node>.ts.net:8443`.
- **iOS gotcha:** Safari suspends background tabs, dropping the suspended tab's
  WebSocket; on return the client clears its replicated entities (the cube vanishes).
  So two *tabs* on one phone can't show two live players — use two *devices*, or rely
  on the always-present demo bot for a single-device check.

## Deployment

`.github/workflows/pages.yml` builds the web client and publishes it to GitHub
Pages on every push to `master` (migrated from the original GitLab Pages CI).

**The Pages source must be set to "GitHub Actions", not "Deploy from a branch".**
This is a one-time, manual repo setting (Settings → Pages → Source) that the
workflow *cannot* set for you — `configure-pages`' `enablement: true` only turns
Pages on when it's fully off; it will not convert an existing branch-source site.
If the source is left on "Deploy from a branch", GitHub silently runs its own
legacy **Jekyll** build of the repo root *alongside* this workflow, and that
Jekyll deploy wins — so the live site renders `README.md` as the index instead of
the game (symptom: the site shows the README; `loader-manifest.json` and
`target/wasm.js` 404). Both pipelines show up green in the Actions tab, which
masks the conflict. Tell-tales of the wrong setting: the served HTML carries a
`<meta name="generator" content="Jekyll …">` tag, and a "pages build and
deployment" run fires on each push next to "Deploy web build to GitHub Pages".
Fix: flip the source to "GitHub Actions" (`gh api -X PUT repos/<o>/<r>/pages -f
build_type=workflow`) and re-run this workflow.
