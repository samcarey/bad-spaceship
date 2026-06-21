# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Bad Spaceship is a 3D game built on the **Bevy 0.8** engine (ECS), with
`bevy_rapier3d` for physics and `bevy_egui` for UI. It is a Cargo workspace with
three crates that compiles both to a **native** binary and to a **WASM** web
build playable in the browser.

## Toolchain & reproducibility (read first)

This is a 2022-era project pinned for reproducible builds — do not "upgrade" your
way out of build errors:

- **Rust is pinned to 1.66.0** via `rust-toolchain.toml` (auto-selected by rustup).
  Bevy 0.8 / wgpu 0.13 build fine on 1.66, but several *transitive* deps have since
  published releases that raise their MSRV above 1.66 (e.g. `flate2` ≥1.1, `fdeflate`
  ≥0.3.6, `uuid` ≥1.12 which drags in `getrandom` 0.3 → `wit-bindgen` needing Rust 1.85,
  and `indexmap` 2.x which uses edition 2024). The committed `Cargo.lock` pins all of
  these *back* to 1.66-compatible versions. Do not `cargo update` the whole graph — it
  will pull those newer releases and break the pinned toolchain.
- **`Cargo.lock` is committed** and holds an MSRV-compatible dependency set (including
  the Bevy-0.8 commit of the `bevy_web_fullscreen` git dependency). Always build with
  `--locked`; when a deliberate re-pin is needed, bump direct deps with targeted
  `cargo update -p <crate> --precise <ver>` rather than a blanket update.
- The web build needs a **version-matched `wasm-bindgen` CLI (exactly 0.2.83)**, matching
  the `wasm-bindgen` crate that Bevy 0.8 / wgpu 0.13 require. Use the prebuilt binary from
  the rustwasm GitHub release, not `cargo install` (building the CLI from source hits the
  same dependency bitrot).

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

## Architecture

The key idea is a **shared simulation** crate that both the client and the headless
server run, so game logic stays identical across renderers and platforms.

- **`shared/`** (`bad-spaceship-shared`, lib) — all platform-agnostic game logic,
  exposed as the `CommonPlugins` plugin group (`shared/src/lib.rs`): third-party
  `RapierPhysicsPlugin` + `EasingsPlugin`, plus the custom `Character`, `Config`,
  `Map`, `Part`, and `Player` plugins. Game tuning lives in RON files under
  `client/assets/config/` (`character.ron`, `player.ron`), deserialized by the
  `ConfigPlugin`'s custom RON `AssetLoader` (`shared/src/config.rs`) into per-domain
  `character::Config` / `player::Config` types. Both binaries keep the asset `Handle`s
  alive in a `load_configs` startup system (dropping a handle unloads the asset) and
  call `watch_for_changes()` for hot-reload.

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
motion, wheel, and keyboard — rather than relying on `winit`. On wasm the client
also adds `bevy_web_fullscreen::FullViewportPlugin`. Most platform-specific code is
gated on `#[cfg(target_arch = "wasm32")]`.

### Build metadata

`client/build.rs` uses `shadow-rs` and `git rev-parse HEAD` to inject a
`SHORT_GIT_HASH` shown in-game, so the build needs git history available.

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
