# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Bad Spaceship is a 3D game built on the **Bevy 0.11** engine (ECS), with
`bevy_rapier3d` for physics and `bevy_egui` for UI. It is a Cargo workspace with
three crates that compiles both to a **native** binary and to a **WASM** web
build playable in the browser.

## Toolchain & reproducibility (read first)

This is a 2023-era project pinned for reproducible builds — do not "upgrade" your
way out of build errors:

- **Rust is pinned to 1.75.0** via `rust-toolchain.toml` (auto-selected by rustup).
  Bevy 0.11 / wgpu 0.16 build fine on it, but many *transitive* deps have since
  published releases that raise their MSRV (crates that moved to edition 2024, or
  pulled in `getrandom` 0.3 / `wit-bindgen` needing Rust 1.85). The committed
  `Cargo.lock` pins the graph *back* to a compatible set — notably `indexmap` 2.5,
  `ahash` 0.8.11, `uuid` 1.11, `jobserver` 0.1.31, `spade` 2.12, `home` 0.5.9, `url`
  2.5.0 (drops the `idna`/ICU4X stack). The oldest such pins that still build (e.g.
  `spade`) need the relaxed private-in-public rule from Rust 1.74, hence 1.75. Do not
  `cargo update` the whole graph — it will pull newer releases and break the toolchain.
- **`Cargo.lock` is committed** and holds an MSRV-compatible dependency set. Always build
  with `--locked`; when a deliberate re-pin is needed, bump direct deps with targeted
  `cargo update -p <crate> --precise <ver>` rather than a blanket update.
- The web build targets the **WebGL2 backend** via the `bevy/webgl2` feature (in the
  client's `web` feature): Bevy 0.11's wgpu 0.16 otherwise compiles the WebGPU backend
  on wasm, which needs `--cfg=web_sys_unstable_apis` and a WebGPU-capable browser. WebGL2
  is the broad-support renderer the 0.10/wgpu 0.15 build already used on Pages.
- The web build needs a **version-matched `wasm-bindgen` CLI (exactly 0.2.84)**, matching
  the `wasm-bindgen` crate pinned in the client (kept compatible with Bevy 0.11 / wgpu 0.16).
  Use the prebuilt binary from
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
  `node`, Homebrew. Install the pinned **Rust 1.75.0** toolchain (see *Toolchain &
  reproducibility*) before building.
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
  `client/assets/config/` (`character.ron`, `player.ron`), deserialized by the
  `ConfigPlugin`'s custom RON `AssetLoader` (`shared/src/config.rs`) into per-domain
  `character::Config` / `player::Config` types. Both binaries keep the asset `Handle`s
  alive in a `load_configs` startup system (dropping a handle unloads the asset) and
  enable hot-reload via `AssetPlugin { watch_for_changes: ChangeWatcher::with_delay(..), .. }`
  (Bevy 0.11 replaced the old `watch_for_changes: bool` flag with a debounced watcher).

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
