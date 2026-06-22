# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Bad Spaceship is a 3D game built on the **Bevy 0.15** engine (ECS), with
`bevy_rapier3d` for physics and `bevy_egui` for UI. It is a Cargo workspace with
three crates that compiles both to a **native** binary and to a **WASM** web
build playable in the browser.

## Toolchain & reproducibility (read first)

This is a pinned-for-reproducibility project — do not "upgrade" your way out of
build errors (the deliberate Bevy bumps are the exception, done branch-by-branch):

- **Rust is pinned to 1.82.0** via `rust-toolchain.toml` (auto-selected by rustup).
  Bevy 0.15's MSRV is exactly 1.82.0, so that is the floor (it was 1.79.0 under Bevy
  0.14). Many *transitive* deps publish releases that raise their MSRV — either by
  moving to **edition 2024** (which needs a Cargo ≥ 1.85 nightly to even *parse*, so no
  amount of toolchain bumping short of 1.85 helps) or by bumping `rust-version` past
  1.82. The committed `Cargo.lock` pins the graph *back* to a 1.82-compatible set.
  Two flavours of pin:
  - *edition2024 parse blockers* — `indexmap` 2.13.0 (2.14 went edition2024),
    `proc-macro-crate` 3.2.0 (3.5 pulls `toml_edit` 0.25 → `toml_datetime` 1.1.1,
    edition2024), `image` 0.25.5 (0.25.6+ pulls `moxcms` → `pxfm`, edition2024),
    `blake3` 1.5.5 + `constant_time_eq` 0.3.1 (newer `blake3` needs `cpufeatures` 0.3,
    edition2024; pinning `blake3` drags both back), `stackfuture` 0.3.0 (0.3.1 went
    edition2024; pulled by `bevy_asset`), and `wayland-protocols` 0.32.12 (only 0.32.13
    went edition2024 — the rest of the `wayland-*` family is fine at its current
    versions now, so pin just this one).
  - *rustc-MSRV (>1.82) blockers* — `gilrs` 0.11.1 (0.11.2 needs rustc 1.84; native
    gamepad dep), `async-lock` 3.4.1 (3.4.2 needs rustc 1.85), plus `ahash` 0.8.11
    (0.8.12 pulls **`getrandom` 0.3**, which fails to compile for
    `wasm32-unknown-unknown` — it needs an explicit `wasm_js` cfg the old build never
    set; pinning keeps the wasm graph on `getrandom` 0.2).
  Note the 1.82 floor *relaxed* several pins the 0.14 build needed (`rayon` 1.11/1.13
  wanted 1.80, `spade` 2.15 wanted 1.82, `uuid` — those resolve forward freely now).
  Do not `cargo update` the whole graph — it will pull newer releases and break the
  toolchain. To hunt offenders fast: `cargo build` prints a precise "requires rustc
  1.XX" error naming the crate + a `cargo update -p … --precise …` hint. For the
  edition2024 ones, `cargo metadata --filter-platform <target>` (run for
  `wasm32-unknown-unknown` **and** the native target) parses only the deps that
  *actually* build for that platform — the unfiltered `cargo metadata`/`cargo tree`
  flag **all** targets, including Android-only deps (`jni`/`android-activity`) and the
  unused-on-our-targets `wayland-protocols`, that never compile for native-Linux or
  wasm. The crates.io versions API exposes a per-version `edition`/`rust_version`
  field, handy for finding the newest pre-edition2024 / pre-MSRV-bump release to pin to.
- **`Cargo.lock` is committed** and holds an MSRV-compatible dependency set. Always build
  with `--locked`; when a deliberate re-pin is needed, bump direct deps with targeted
  `cargo update -p <crate> --precise <ver>` rather than a blanket update.
- The web build targets the **WebGL2 backend** via the `bevy/webgl2` feature (in the
  client's `web` feature): Bevy 0.15's wgpu 23 otherwise compiles the WebGPU backend
  on wasm, which needs `--cfg=web_sys_unstable_apis` and a WebGPU-capable browser. WebGL2
  is the broad-support renderer the build has used on Pages all along.
- The web build needs a **version-matched `wasm-bindgen` CLI (exactly 0.2.97)**, matching
  the `wasm-bindgen` crate pinned in the client. Note 0.2.97 is *not* wgpu 23's floor
  (0.2.95) — `bevy_egui` 0.31 pulls `web-sys` 0.3.74, which hard-pins
  `wasm-bindgen = "=0.2.97"`, so the whole graph is dragged up to 0.2.97 (and the CLI
  must match). Use the prebuilt binary from the rustwasm GitHub release, not
  `cargo install` (building the CLI from source hits the same dependency bitrot). The
  Pages CI also hardcodes `RUST_TOOLCHAIN` and `WASM_BINDGEN_VERSION` in
  `.github/workflows/pages.yml` (it `rustup override set`s the toolchain, shadowing
  `rust-toolchain.toml`) — bump **both** in lockstep on a Bevy upgrade.

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
  `node`, Homebrew. Install the pinned **Rust 1.82.0** toolchain (see *Toolchain &
  reproducibility*) before building. Note: `rustup override set` from a prior session
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
  `RapierPhysicsPlugin` + `EasingsPlugin`, plus the custom `Character`, `Config`,
  `Map`, `Part`, and `Player` plugins. Game tuning lives in RON files under
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
