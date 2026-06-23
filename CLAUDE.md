# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Bad Spaceship is a 3D game built on the **Bevy 0.19** engine (ECS), with
**Avian** (`avian3d`, an XPBD physics engine) for physics and `bevy_egui` for UI.
It is a Cargo workspace with three crates that compiles both to a **native**
binary and to a **WASM** web build playable in the browser. (Physics was migrated
off `bevy_rapier3d` — see the "Migrating bevy_rapier3d → Avian" section below.)

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
  copy). Symptom if either is missing: `the wasm*-unknown-unknown targets are not
  supported by default` at `getrandom` compile.
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
both exposing a `PlatformPlugin`. The **web** implementation wires browser input
directly through DOM event listeners (`web-sys` / `gloo`) — pointer lock, mouse
motion, wheel, and keyboard — rather than relying on `winit`. Most platform-specific
code is gated on `#[cfg(target_arch = "wasm32")]`. The WASM canvas is sized to the
viewport via CSS (`canvas { width/height: 100% }` in `index.html`): Bevy 0.13 removed
`Window::fit_canvas_to_parent` (which had itself replaced the old
`bevy_web_fullscreen` plugin), and the recommended replacement is plain CSS.

One Bevy 0.13 web gotcha lives here: the 0.13 bump pulled winit 0.28 → 0.29,
which (unlike 0.28) emits its *own* `MouseMotion` events from the web canvas
under pointer lock. Those phantom deltas double-fed the DOM-listener look path
and spun the camera on its own with the mouse held still. The fix in
`get_mouse_motion` (web): take `ResMut<Events<MouseMotion>>`, `clear()` the
buffer each frame to drop winit's events, then re-send only our tracker's
delta — so look is driven solely by our listeners. If wheel/keyboard ever
start double-firing on web, it's the same cause and the same clear-then-inject
pattern is the remedy.

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

### Bevy 0.19 migration gotchas

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
