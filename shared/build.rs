// Emits the build's version identity as a generated source file that
// `shared/src/net.rs` `include!`s, so both the client and server binaries compile
// the SAME constants:
//   BS_PROTOCOL_ID — fed into the lightyear netcode handshake so a client and
//                    server built from different commits refuse to connect
//                    (lightyear 0.27 has no protocol digest of its own).
//   BS_VERSION     — the short git SHA, for diagnostics.
//
// Correctness under a SHARED CARGO_TARGET_DIR (the deploy reuses one cache across
// versions for fast incrementals): we key the rebuild on the `BS_BUILD_SHA` env
// the deploy passes, via `rerun-if-env-changed`, so switching versions always
// regenerates these constants instead of reusing a cached value from another SHA.
use std::{env, fs, path::Path, process::Command};

fn main() {
    println!("cargo:rerun-if-env-changed=BS_BUILD_SHA");

    // Prefer the explicit SHA the deploy injects; fall back to the live checkout.
    let sha = env::var("BS_BUILD_SHA")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default();

    // Local-dev convenience: rebuild when HEAD moves. `--git-path` resolves the
    // real HEAD file even inside a `git worktree`.
    if let Ok(o) = Command::new("git").args(["rev-parse", "--git-path", "HEAD"]).output() {
        if let Ok(p) = String::from_utf8(o.stdout) {
            let p = p.trim();
            if !p.is_empty() {
                println!("cargo:rerun-if-changed={p}");
            }
        }
    }

    // FNV-1a 64-bit over the SHA bytes -> a stable, effectively-unique protocol id.
    // No git (empty SHA) -> the FNV offset basis, a fine constant for local dev.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in sha.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let short = if sha.len() >= 7 { &sha[0..7] } else { "dev" };

    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("bs_version.rs");
    fs::write(
        &out,
        format!(
            "/// Netcode protocol id derived from the build's git commit (FNV-1a of the\n\
             /// full SHA). Gates cross-version client<->server connections.\n\
             pub const BS_PROTOCOL_ID: u64 = {h};\n\
             /// Short git SHA this binary was built from.\n\
             pub const BS_VERSION: &str = \"{short}\";\n"
        ),
    )
    .unwrap();
}
